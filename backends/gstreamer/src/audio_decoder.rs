use super::gst_app::{AppSink, AppSinkCallbacks, AppSrc};
use super::gst_audio;
use byte_slice_cast::*;
use gst;
use gst::buffer::{MappedBuffer, Readable};
use gst::prelude::*;
use servo_media_audio::decoder::{AudioDecoder, AudioDecoderCallbacks, AudioDecoderOptions};
use std::io::Cursor;
use std::io::Read;
use std::sync::Arc;

pub struct GStreamerAudioDecoderProgress(MappedBuffer<Readable>);

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
            None => return callbacks.error(),
        };

        let decodebin = match gst::ElementFactory::make("decodebin", None) {
            Some(decodebin) => decodebin,
            None => return callbacks.error(),
        };

        // decodebin uses something called a "sometimes-pad", which is basically
        // a pad that will show up when a certain condition is met,
        // in decodebins case that is media being decoded
        if pipeline.add_many(&[&appsrc, &decodebin]).is_err() {
            return callbacks.error();
        }

        if gst::Element::link_many(&[&appsrc, &decodebin]).is_err() {
            return callbacks.error();
        }

        let appsrc = match appsrc.downcast::<AppSrc>() {
            Ok(appsrc) => appsrc,
            Err(_) => {
                return callbacks.error();
            }
        };

        let options = options.unwrap_or_default();

        let pipeline_ = pipeline.clone();
        let callbacks_ = callbacks.clone();
        decodebin.connect_pad_added(move |_, src_pad| {
            // We only want a sink per audio stream.
            if src_pad.is_linked() {
                return;
            }

            let pipeline = &pipeline_;
            let callbacks = &callbacks_;

            let (is_audio, _) = {
                let media_type = src_pad.get_current_caps().and_then(|caps| {
                    caps.get_structure(0).map(|s| {
                        let name = s.get_name();
                        (name.starts_with("audio/"), name.starts_with("video/"))
                    })
                });

                match media_type {
                    None => {
                        eprintln!("Failed to get media type from pad {}", src_pad.get_name());
                        return callbacks.error();
                    }
                    Some(media_type) => media_type,
                }
            };

            if !is_audio {
                return callbacks.error();
            }

            let insert_sink = || -> Result<(), ()> {
                let queue = gst::ElementFactory::make("queue", None).ok_or(())?;
                let convert = gst::ElementFactory::make("audioconvert", None).ok_or(())?;
                let resample = gst::ElementFactory::make("audioresample", None).ok_or(())?;
                let sink = gst::ElementFactory::make("appsink", None).ok_or(())?;
                let appsink = sink.clone().dynamic_cast::<AppSink>().map_err(|_| ())?;

                let audio_info = gst_audio::AudioInfo::new(
                    gst_audio::AUDIO_FORMAT_F32,
                    options.sample_rate as u32,
                    options.channels,
                ).build()
                    .ok_or(())?;
                appsink.set_caps(&audio_info.to_caps().unwrap());

                let pipeline_ = pipeline.clone();
                let pipeline__ = pipeline.clone();
                let callbacks_ = callbacks.clone();
                let callbacks__ = callbacks.clone();
                appsink.set_callbacks(
                    AppSinkCallbacks::new()
                        .new_sample(move |appsink| {
                            let sample = match appsink.pull_sample() {
                                None => {
                                    return gst::FlowReturn::Eos;
                                }
                                Some(sample) => sample,
                            };

                            let buffer = if let Some(buffer) = sample.get_buffer() {
                                buffer
                            } else {
                                callbacks_.error();
                                let _ = pipeline_.set_state(gst::State::Null);
                                return gst::FlowReturn::Error;
                            };

                            let map = if let Ok(map) = buffer.into_mapped_buffer_readable() {
                                map
                            } else {
                                callbacks_.error();
                                let _ = pipeline_.set_state(gst::State::Null);
                                return gst::FlowReturn::Error;
                            };

                            let progress = Box::new(GStreamerAudioDecoderProgress(map));
                            callbacks_.progress(progress);

                            gst::FlowReturn::Ok
                        })
                        .eos(move |_| {
                            callbacks__.eos();
                            let _ = pipeline__.set_state(gst::State::Null);
                        })
                        .build(),
                );

                let elements = &[&queue, &convert, &resample, &sink];
                pipeline.add_many(elements).map_err(|_| ())?;
                gst::Element::link_many(elements).map_err(|_| ())?;

                for e in elements {
                    e.sync_state_with_parent().map_err(|_| ())?;
                }

                let sink_pad = queue.get_static_pad("sink").ok_or(())?;
                src_pad
                    .link(&sink_pad)
                    .into_result()
                    .map(|_| ())
                    .map_err(|_| ())
            };

            if insert_sink().is_err() {
                callbacks.error();
            }
        });

        appsrc.set_property_format(gst::Format::Bytes);
        appsrc.set_property_block(true);

        let _ = pipeline.set_state(gst::State::Playing);

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
    }
}
