use byte_slice_cast::*;
use gst;
use gst::prelude::*;
use gst_app::{AppSrc, AppSrcCallbacks};
use gst_audio;
use servo_media_audio::block::{Chunk, FRAMES_PER_BLOCK};
use servo_media_audio::render_thread::AudioRenderThreadMsg;
use servo_media_audio::sink::{AudioSink, AudioSinkError};
use std::cell::{Cell, RefCell};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::Builder;

const DEFAULT_SAMPLE_RATE: f32 = 44100.;

pub struct GStreamerAudioSink {
    pipeline: gst::Pipeline,
    appsrc: Arc<AppSrc>,
    sample_rate: Cell<f32>,
    audio_info: RefCell<Option<gst_audio::AudioInfo>>,
    sample_offset: Cell<u64>,
}

impl GStreamerAudioSink {
    pub fn new() -> Result<Self, AudioSinkError> {
        if let Some(category) = gst::DebugCategory::get("openslessink") {
            category.set_threshold(gst::DebugLevel::Trace);
        }
        gst::init().map_err(|_| AudioSinkError::Backend("GStreamer init failed".to_owned()))?;

        let appsrc = gst::ElementFactory::make("appsrc", None)
            .ok_or(AudioSinkError::Backend("appsrc creation failed".to_owned()))?;
        let appsrc = appsrc.downcast::<AppSrc>().unwrap();
        Ok(Self {
            pipeline: gst::Pipeline::new(None),
            appsrc: Arc::new(appsrc),
            sample_rate: Cell::new(DEFAULT_SAMPLE_RATE),
            audio_info: RefCell::new(None),
            sample_offset: Cell::new(0),
        })
    }
}

impl GStreamerAudioSink {
    fn set_audio_info(&self, sample_rate: f32, channels: u8) -> Result<(), AudioSinkError> {
        let audio_info = gst_audio::AudioInfo::new(
            gst_audio::AUDIO_FORMAT_F32,
            sample_rate as u32,
            channels.into(),
        )
        .build()
        .ok_or(AudioSinkError::Backend("AudioInfo failed".to_owned()))?;
        self.appsrc.set_caps(&audio_info.to_caps().unwrap());
        *self.audio_info.borrow_mut() = Some(audio_info);
        Ok(())
    }

    fn set_channels_if_changed(&self, channels: u8) -> Result<(), AudioSinkError> {
        let curr_channels = if let Some(ch) = self.audio_info.borrow().as_ref() {
            ch.channels()
        } else {
            return Ok(());
        };
        if channels != curr_channels as u8 {
            self.set_audio_info(self.sample_rate.get(), channels)?;
        }
        Ok(())
    }
}

impl AudioSink for GStreamerAudioSink {
    fn init(
        &self,
        sample_rate: f32,
        graph_thread_channel: Sender<AudioRenderThreadMsg>,
    ) -> Result<(), AudioSinkError> {
        self.sample_rate.set(sample_rate);
        self.set_audio_info(sample_rate, 2)?;
        self.appsrc.set_property_format(gst::Format::Time);

        // Allow only a single chunk.
        self.appsrc.set_max_bytes(1);

        let appsrc = self.appsrc.clone();
        Builder::new()
            .name("GstAppSrcCallbacks".to_owned())
            .spawn(move || {
                let need_data = move |_: &AppSrc, _: u32| {
                    graph_thread_channel
                        .send(AudioRenderThreadMsg::SinkNeedData)
                        .unwrap();
                };
                appsrc.set_callbacks(AppSrcCallbacks::new().need_data(need_data).build());
            })
            .unwrap();

        let appsrc = self.appsrc.as_ref().clone().upcast();
        let resample = gst::ElementFactory::make("audioresample", None).ok_or(
            AudioSinkError::Backend("audioresample creation failed".to_owned()),
        )?;
        let convert = gst::ElementFactory::make("audioconvert", None).ok_or(
            AudioSinkError::Backend("audioconvert creation failed".to_owned()),
        )?;
        let sink = gst::ElementFactory::make("autoaudiosink", None).ok_or(
            AudioSinkError::Backend("autoaudiosink creation failed".to_owned()),
        )?;
        self.pipeline
            .add_many(&[&appsrc, &resample, &convert, &sink])
            .map_err(|e| AudioSinkError::Backend(e.to_string()))?;
        gst::Element::link_many(&[&appsrc, &resample, &convert, &sink])
            .map_err(|e| AudioSinkError::Backend(e.to_string()))?;

        Ok(())
    }

    fn play(&self) -> Result<(), AudioSinkError> {
        self.pipeline
            .set_state(gst::State::Playing)
            .map(|_| ())
            .map_err(|_| AudioSinkError::StateChangeFailed)
    }

    fn stop(&self) -> Result<(), AudioSinkError> {
        self.pipeline
            .set_state(gst::State::Paused)
            .map(|_| ())
            .map_err(|_| AudioSinkError::StateChangeFailed)
    }

    fn has_enough_data(&self) -> bool {
        self.appsrc.get_current_level_bytes() >= self.appsrc.get_max_bytes()
    }

    fn push_data(&self, mut chunk: Chunk) -> Result<(), AudioSinkError> {
        if let Some(block) = chunk.blocks.get(0) {
            self.set_channels_if_changed(block.chan_count())?;
        }

        let sample_rate = self.sample_rate.get() as u64;
        let audio_info = self.audio_info.borrow();
        let audio_info = audio_info.as_ref().unwrap();
        let channels = audio_info.channels();
        let bpf = audio_info.bpf() as usize;
        assert!(bpf == 4 * channels as usize);
        let n_samples = FRAMES_PER_BLOCK.0 as u64;
        let buf_size = (n_samples as usize) * (bpf);
        let mut buffer = gst::Buffer::with_size(buf_size).unwrap();
        {
            let buffer = buffer.get_mut().unwrap();
            let mut sample_offset = self.sample_offset.get();
            // Calculate the current timestamp (PTS) and the next one,
            // and calculate the duration from the difference instead of
            // simply the number of samples to prevent rounding errors
            let pts = sample_offset
                .mul_div_floor(gst::SECOND_VAL, sample_rate)
                .unwrap()
                .into();
            let next_pts: gst::ClockTime = (sample_offset + n_samples)
                .mul_div_floor(gst::SECOND_VAL, sample_rate)
                .unwrap()
                .into();
            buffer.set_pts(pts);
            buffer.set_duration(next_pts - pts);

            // sometimes nothing reaches the output
            if chunk.len() == 0 {
                chunk.blocks.push(Default::default());
                chunk.blocks[0].repeat(channels as u8);
            }
            debug_assert!(chunk.len() == 1);
            let mut data = chunk.blocks[0].interleave();
            let data = data.as_mut_byte_slice().expect("casting failed");

            // XXXManishearth if we have a safe way to convert
            // from Box<[f32]> to Box<[u8]> (similarly for Vec)
            // we can use Buffer::from_slice instead
            buffer.copy_from_slice(0, data).expect("copying failed");

            sample_offset += n_samples;
            self.sample_offset.set(sample_offset);
        }

        self.appsrc
            .push_buffer(buffer)
            .map(|_| ())
            .map_err(|_| AudioSinkError::BufferPushFailed)
    }

    fn set_eos_callback(&self, _: Box<Fn(Box<AsRef<[f32]>>) + Send + Sync + 'static>) {}
}

impl Drop for GStreamerAudioSink {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
