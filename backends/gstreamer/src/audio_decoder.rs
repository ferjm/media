use byte_slice_cast::*;
use gst;
use gst::prelude::*;
use gst_app;
use gst_audio;
use servo_media_audio::decoder::{AudioDecoder, AudioDecoderCallbacks};
use servo_media_audio::decoder::{AudioDecoderError, AudioDecoderOptions};
use std::io::Cursor;
use std::io::Read;
use std::sync::{mpsc, Arc, Mutex};

pub struct GStreamerAudioDecoderProgress(gst::buffer::MappedBuffer<gst::buffer::Readable>);

impl AsRef<[f32]> for GStreamerAudioDecoderProgress {
    fn as_ref(&self) -> &[f32] {
        self.0.as_ref().as_slice_of::<f32>().unwrap()
    }
}

pub struct GStreamerAudioDecoder {}

impl GStreamerAudioDecoder {
    pub fn new() -> Self {
        Self {}
    }
}

impl AudioDecoder for GStreamerAudioDecoder {
    fn decode(
        &self,
        data: Vec<u8>,
        callbacks: AudioDecoderCallbacks,
        options: Option<AudioDecoderOptions>,
    ) {
        let pipeline = gst::Pipeline::new(None);
        let callbacks = Arc::new(callbacks);

        let appsrc = match gst::ElementFactory::make("appsrc", None) {
            Some(appsrc) => appsrc,
            None => {
                return callbacks.error(AudioDecoderError::Backend(
                    "appsrc creation failed".to_owned(),
                ));
            }
        };

        let decodebin = match gst::ElementFactory::make("decodebin", None) {
            Some(decodebin) => decodebin,
            None => {
                return callbacks.error(AudioDecoderError::Backend(
                    "decodebin creation failed".to_owned(),
                ));
            }
        };

        // decodebin uses something called a "sometimes-pad", which is basically
        // a pad that will show up when a certain condition is met,
        // in decodebins case that is media being decoded
        if let Err(e) = pipeline.add_many(&[&appsrc, &decodebin]) {
            return callbacks.error(AudioDecoderError::Backend(e.to_string()));
        }

        if let Err(e) = gst::Element::link_many(&[&appsrc, &decodebin]) {
            return callbacks.error(AudioDecoderError::Backend(e.to_string()));
        }

        let appsrc = appsrc.downcast::<gst_app::AppSrc>().unwrap();

        let options = options.unwrap_or_default();

        let (sender, receiver) = mpsc::channel();
        let sender = Arc::new(Mutex::new(sender));

        let pipeline_ = pipeline.downgrade();
        let callbacks_ = callbacks.clone();
        let sender_ = sender.clone();
        // Initial pipeline looks like
        //
        // appsrc ! decodebin2! ...
        //
        // We plug in the second part of the pipeline, including the deinterleave element,
        // once the media starts being decoded.
        decodebin.connect_pad_added(move |_, src_pad| {
            // A decodebin pad was added, if this is an audio file,
            // plug in a deinterleave element to separate each planar channel.
            //
            // Sub pipeline looks like
            //
            // ... decodebin2 ! audioconvert ! audioresample ! capsfilter ! deinterleave ...
            //
            // deinterleave also uses a sometime-pad, so we need to wait until
            // a pad for a planar channel is added to plug in the last part of
            // the pipeline, with the appsink that will be pulling the data from
            // each channel.

            let callbacks = &callbacks_;
            let sender = &sender_;
            let pipeline = match pipeline_.upgrade() {
                Some(pipeline) => pipeline,
                None => {
                    callbacks.error(AudioDecoderError::Backend(
                        "Pipeline failed upgrade".to_owned(),
                    ));
                    let _ = sender.lock().unwrap().send(());
                    return;
                }
            };

            let (is_audio, caps) = {
                let media_type = src_pad.get_current_caps().and_then(|caps| {
                    caps.get_structure(0).map(|s| {
                        let name = s.get_name();
                        (name.starts_with("audio/"), caps.clone())
                    })
                });

                match media_type {
                    None => {
                        callbacks.error(AudioDecoderError::Backend(
                            "Failed to get media type from pad".to_owned(),
                        ));
                        let _ = sender.lock().unwrap().send(());
                        return;
                    }
                    Some(media_type) => media_type,
                }
            };

            if !is_audio {
                callbacks.error(AudioDecoderError::InvalidMediaFormat);
                let _ = sender.lock().unwrap().send(());
                return;
            }

            let sample_audio_info = match gst_audio::AudioInfo::from_caps(&caps) {
                Some(sample_audio_info) => sample_audio_info,
                None => {
                    callbacks.error(AudioDecoderError::Backend("AudioInfo failed".to_owned()));
                    let _ = sender.lock().unwrap().send(());
                    return;
                }
            };
            let channels = sample_audio_info.channels();
            callbacks.ready(channels);

            let insert_deinterleave = || -> Result<(), AudioDecoderError> {
                let convert = gst::ElementFactory::make("audioconvert", None).ok_or(
                    AudioDecoderError::Backend("audioconvert creation failed".to_owned()),
                )?;
                let resample = gst::ElementFactory::make("audioresample", None).ok_or(
                    AudioDecoderError::Backend("audioresample creation failed".to_owned()),
                )?;
                let filter = gst::ElementFactory::make("capsfilter", None).ok_or(
                    AudioDecoderError::Backend("capsfilter creation failed".to_owned()),
                )?;
                let deinterleave = gst::ElementFactory::make("deinterleave", Some("deinterleave"))
                    .ok_or(AudioDecoderError::Backend(
                        "deinterleave creation failed".to_owned(),
                    ))?;

                deinterleave
                    .set_property("keep-positions", &true.to_value())
                    .map_err(|e| AudioDecoderError::Backend(e.to_string()))?;
                let pipeline_ = pipeline.downgrade();
                let callbacks_ = callbacks.clone();
                deinterleave.connect_pad_added(move |_, src_pad| {
                    // A new pad for a planar channel was added in deinterleave.
                    // Plug in an appsink so we can pull the data from each channel.
                    //
                    // The end of the pipeline looks like:
                    //
                    // ... deinterleave ! queue ! appsink.
                    let callbacks = &callbacks_;
                    let pipeline = match pipeline_.upgrade() {
                        Some(pipeline) => pipeline,
                        None => {
                            return callbacks.error(AudioDecoderError::Backend(
                                "Pipeline failedupgrade".to_owned(),
                            ));
                        }
                    };
                    let insert_sink = || -> Result<(), AudioDecoderError> {
                        let queue = gst::ElementFactory::make("queue", None).ok_or(
                            AudioDecoderError::Backend("queue creation failed".to_owned()),
                        )?;
                        let sink = gst::ElementFactory::make("appsink", None).ok_or(
                            AudioDecoderError::Backend("appsink creation failed".to_owned()),
                        )?;
                        let appsink = sink.clone().dynamic_cast::<gst_app::AppSink>().unwrap();
                        sink.set_property("sync", &false.to_value())
                            .map_err(|e| AudioDecoderError::Backend(e.to_string()))?;

                        let callbacks_ = callbacks.clone();
                        appsink.set_callbacks(
                            gst_app::AppSinkCallbacks::new()
                                .new_sample(move |appsink| {
                                    let sample =
                                        appsink.pull_sample().ok_or(gst::FlowError::Eos)?;
                                    let buffer = sample.get_buffer().ok_or_else(|| {
                                        callbacks_.error(AudioDecoderError::InvalidSample);
                                        gst::FlowError::Error
                                    })?;

                                    let audio_info = sample
                                        .get_caps()
                                        .and_then(|caps| {
                                            gst_audio::AudioInfo::from_caps(caps.as_ref())
                                        })
                                        .ok_or_else(|| {
                                            callbacks_.error(AudioDecoderError::Backend(
                                                "Could not get caps from sample".to_owned(),
                                            ));
                                            gst::FlowError::Error
                                        })?;
                                    assert_eq!(audio_info.channels(), 1);
                                    let positions = audio_info.positions().ok_or_else(|| {
                                        callbacks_.error(AudioDecoderError::Backend(
                                            "AudioInfo failed".to_owned(),
                                        ));
                                        gst::FlowError::Error
                                    })?;

                                    for position in positions.iter() {
                                        let buffer = buffer.clone();
                                        let map = if let Ok(map) =
                                            buffer.into_mapped_buffer_readable()
                                        {
                                            map
                                        } else {
                                            callbacks_.error(AudioDecoderError::BufferReadFailed);
                                            return Err(gst::FlowError::Error);
                                        };
                                        let progress = Box::new(GStreamerAudioDecoderProgress(map));
                                        let channel = position.to_mask() as u32;
                                        callbacks_.progress(progress, channel);
                                    }

                                    Ok(gst::FlowSuccess::Ok)
                                })
                                .build(),
                        );

                        let elements = &[&queue, &sink];
                        pipeline
                            .add_many(elements)
                            .map_err(|e| AudioDecoderError::Backend(e.to_string()))?;
                        gst::Element::link_many(elements)
                            .map_err(|e| AudioDecoderError::Backend(e.to_string()))?;

                        for e in elements {
                            e.sync_state_with_parent()
                                .map_err(|e| AudioDecoderError::Backend(e.to_string()))?;
                        }

                        let sink_pad =
                            queue
                                .get_static_pad("sink")
                                .ok_or(AudioDecoderError::Backend(
                                    "Could not get static pad sink".to_owned(),
                                ))?;
                        src_pad.link(&sink_pad).map(|_| ()).map_err(|_| {
                            AudioDecoderError::Backend("Sink pad link failed".to_owned())
                        })
                    };

                    if let Err(e) = insert_sink() {
                        callbacks.error(e);
                    }
                });

                let audio_info = gst_audio::AudioInfo::new(
                    gst_audio::AUDIO_FORMAT_F32,
                    options.sample_rate as u32,
                    channels,
                )
                .build()
                .ok_or(AudioDecoderError::Backend("AudioInfo failed".to_owned()))?;
                let caps = audio_info
                    .to_caps()
                    .ok_or(AudioDecoderError::Backend("AudioInfo failed".to_owned()))?;
                filter.set_property("caps", &caps.to_value()).map_err(|_| {
                    AudioDecoderError::Backend("Setting caps property failed".to_owned())
                })?;

                let elements = &[&convert, &resample, &filter, &deinterleave];
                pipeline
                    .add_many(elements)
                    .map_err(|e| AudioDecoderError::Backend(e.to_string()))?;
                gst::Element::link_many(elements)
                    .map_err(|e| AudioDecoderError::Backend(e.to_string()))?;

                for e in elements {
                    e.sync_state_with_parent()
                        .map_err(|e| AudioDecoderError::Backend(e.to_string()))?;
                }

                let sink_pad = convert
                    .get_static_pad("sink")
                    .ok_or(AudioDecoderError::Backend(
                        "Get static pad sink failed".to_owned(),
                    ))?;
                src_pad
                    .link(&sink_pad)
                    .map(|_| ())
                    .map_err(|_| AudioDecoderError::Backend("Sink pad link failed".to_owned()))
            };

            if let Err(e) = insert_deinterleave() {
                callbacks.error(e);
                let _ = sender.lock().unwrap().send(());
            }
        });

        appsrc.set_property_format(gst::Format::Bytes);
        appsrc.set_property_block(true);

        let bus = match pipeline.get_bus() {
            Some(bus) => bus,
            None => {
                callbacks.error(AudioDecoderError::Backend(
                    "Pipeline without bus. Shouldn't happen!".to_owned(),
                ));
                let _ = sender.lock().unwrap().send(());
                return;
            }
        };

        let callbacks_ = callbacks.clone();
        bus.set_sync_handler(move |_, msg| {
            use gst::MessageView;

            match msg.view() {
                MessageView::Error(e) => {
                    callbacks_.error(AudioDecoderError::Backend(
                        e.get_debug().unwrap_or("Unknown".to_owned()),
                    ));
                    let _ = sender.lock().unwrap().send(());
                }
                MessageView::Eos(_) => {
                    callbacks_.eos();
                    let _ = sender.lock().unwrap().send(());
                }
                _ => (),
            }
            gst::BusSyncReply::Drop
        });

        if pipeline.set_state(gst::State::Playing).is_err() {
            callbacks.error(AudioDecoderError::StateChangeFailed);
            return;
        }

        let max_bytes = appsrc.get_max_bytes() as usize;
        let data_len = data.len();
        let mut reader = Cursor::new(data);
        while (reader.position() as usize) < data_len {
            let data_left = data_len - reader.position() as usize;
            let buffer_size = if data_left < max_bytes {
                data_left
            } else {
                max_bytes
            };
            let mut buffer = gst::Buffer::with_size(buffer_size).unwrap();
            {
                let buffer = buffer.get_mut().unwrap();
                let mut map = buffer.map_writable().unwrap();
                let mut buffer = map.as_mut_slice();
                let _ = reader.read(&mut buffer);
            }
            let _ = appsrc.push_buffer(buffer);
        }
        let _ = appsrc.end_of_stream();

        // Wait until we get an error or EOS.
        receiver.recv().unwrap();
        let _ = pipeline.set_state(gst::State::Null);
    }
}
