extern crate boxfnonce;

use boxfnonce::SendBoxFnOnce;
use std::any::Any;
use std::sync::Mutex;

pub mod capture;

impl MediaStreamCallbacks {
    pub fn new() -> MediaStreamCallbacksBuilder {
        MediaStreamCallbacksBuilder {
            eos: None,
            error: None,
            progress: None,
        }
    }

    pub fn eos(&self) {
        let eos = self.eos.lock().unwrap().take();
        match eos {
            None => return,
            Some(callback) => callback.call(),
        };
    }

    pub fn error(&self) {
        let callback = self.error.lock().unwrap().take();
        match callback {
            None => return,
            Some(callback) => callback.call(),
        };
    }

    pub fn progress(&self, buffer: Box<AsRef<[u8]>>) {
        match self.progress {
            None => return,
            Some(ref callback) => callback(buffer),
        };
    }
}

pub struct MediaStreamCallbacksBuilder {
    eos: Option<SendBoxFnOnce<'static, ()>>,
    error: Option<SendBoxFnOnce<'static, ()>>,
    progress: Option<Box<Fn(Box<AsRef<[u8]>>) + Send + Sync + 'static>>,
}

impl MediaStreamCallbacksBuilder {
    pub fn eos<F: FnOnce() + Send + 'static>(self, eos: F) -> Self {
        Self {
            eos: Some(SendBoxFnOnce::new(eos)),
            ..self
        }
    }

    pub fn error<F: FnOnce() + Send + 'static>(self, error: F) -> Self {
        Self {
            error: Some(SendBoxFnOnce::new(error)),
            ..self
        }
    }

    pub fn progress<F: Fn(Box<AsRef<[u8]>>) + Send + Sync + 'static>(
        self,
        progress: F,
    ) -> Self {
        Self {
            progress: Some(Box::new(progress)),
            ..self
        }
    }

    pub fn build(self) -> MediaStreamCallbacks {
        MediaStreamCallbacks {
            eos: Mutex::new(self.eos),
            error: Mutex::new(self.error),
            progress: self.progress,
        }
    }
}

pub struct MediaStreamCallbacks {
    pub eos: Mutex<Option<SendBoxFnOnce<'static, ()>>>,
    pub error: Mutex<Option<SendBoxFnOnce<'static, ()>>>,
    pub progress: Option<Box<Fn(Box<AsRef<[u8]>>) + Send + Sync + 'static>>,
}

/// PoC of a media stream producing test data and exposing it through
/// callbacks.
pub trait AutoMediaStream {
    fn play(callbacks: MediaStreamCallbacks);
}

pub trait MediaStream: Any + Send {
    fn as_any(&self) -> &Any;
    fn as_mut_any(&mut self) -> &mut Any;
}

/// This isn't part of the webrtc spec; it's a leaky abstaction while media streams
/// are under development and example consumers need to be able to inspect them.
pub trait MediaOutput: Send {
    fn add_stream(&mut self, stream: Box<MediaStream>);
}
