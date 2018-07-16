use std::boxed::FnBox;
use std::sync::Mutex;

pub struct AudioDecoderCallbacks {
    pub eos: Mutex<Option<Box<FnBox() + Send + 'static>>>,
    pub error: Mutex<Option<Box<FnBox() + Send + 'static>>>,
    pub progress: Option<Box<Fn(Box<AsRef<[f32]>>) + Send + Sync + 'static>>,
}

impl AudioDecoderCallbacks {
    pub fn new() -> AudioDecoderCallbacksBuilder {
        AudioDecoderCallbacksBuilder {
            eos: None,
            error: None,
            progress: None,
        }
    }

    pub fn eos(&self) {
        let eos = self.eos.lock().unwrap().take();
        match eos {
            None => return,
            Some(callback) => callback(),
        };
    }

    pub fn error(&self) {
        let error = self.error.lock().unwrap().take();
        match error {
            None => return,
            Some(callback) => callback(),
        };
    }

    pub fn progress(&self, buffer: Box<AsRef<[f32]>>) {
        match self.progress {
            None => return,
            Some(ref callback) => callback(buffer),
        };
    }
}

unsafe impl Send for AudioDecoderCallbacks {}
unsafe impl Sync for AudioDecoderCallbacks {}

pub struct AudioDecoderCallbacksBuilder {
    eos: Option<Box<FnBox() + Send + 'static>>,
    error: Option<Box<FnBox() + Send + 'static>>,
    progress: Option<Box<Fn(Box<AsRef<[f32]>>) + Send + Sync + 'static>>,
}

impl AudioDecoderCallbacksBuilder {
    pub fn eos<F: FnOnce() + Send + 'static>(self, eos: F) -> Self {
        Self {
            eos: Some(Box::new(eos)),
            ..self
        }
    }

    pub fn error<F: FnOnce() + Send + 'static>(self, error: F) -> Self {
        Self {
            error: Some(Box::new(error)),
            ..self
        }
    }

    pub fn progress<F: Fn(Box<AsRef<[f32]>>) + Send + Sync + 'static>(self, progress: F) -> Self {
        Self {
            progress: Some(Box::new(progress)),
            ..self
        }
    }

    pub fn build(self) -> AudioDecoderCallbacks {
        AudioDecoderCallbacks {
            eos: Mutex::new(self.eos),
            error: Mutex::new(self.error),
            progress: self.progress,
        }
    }
}

pub struct AudioDecoderOptions {
    pub sample_rate: f32,
    pub channels: u32,
}

impl Default for AudioDecoderOptions {
    fn default() -> Self {
        AudioDecoderOptions {
            sample_rate: 48000.,
            channels: 1,
        }
    }
}

pub trait AudioDecoder {
    fn decode(&self, data: Vec<u8>, callbacks: AudioDecoderCallbacks, options: Option<AudioDecoderOptions>);
}

pub struct DummyAudioDecoder;

impl AudioDecoder for DummyAudioDecoder {
    fn decode(&self, _: Vec<u8>, _: AudioDecoderCallbacks, _: Option<AudioDecoderOptions>) {}
}
