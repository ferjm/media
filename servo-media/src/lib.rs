pub extern crate servo_media_audio as audio;
#[cfg(not(target_os = "android"))]
extern crate servo_media_gstreamer;
use std::sync::{self, Arc, Mutex, Once};

use audio::context::{AudioContext, AudioContextOptions};
use audio::decoder::DummyAudioDecoder;
use audio::sink::DummyAudioSink;
use audio::AudioBackend;

pub struct ServoMedia;

static INITIALIZER: Once = sync::ONCE_INIT;
static mut INSTANCE: *mut Mutex<Option<Arc<ServoMedia>>> = 0 as *mut _;

pub struct DummyBackend {}

impl AudioBackend for DummyBackend {
    type Decoder = DummyAudioDecoder;
    type Sink = DummyAudioSink;
    fn make_decoder() -> Self::Decoder {
        DummyAudioDecoder
    }

    fn make_sink() -> Result<Self::Sink, ()> {
        Ok(DummyAudioSink)
    }
    fn init() {}
}

#[cfg(not(target_os = "android"))]
pub type Backend = servo_media_gstreamer::GStreamerBackend;
#[cfg(target_os = "android")]
pub type Backend = DummyBackend;

impl ServoMedia {
    pub fn new() -> Self {
        Backend::init();

        Self {}
    }

    pub fn get() -> Result<Arc<ServoMedia>, ()> {
        INITIALIZER.call_once(|| unsafe {
            INSTANCE = Box::into_raw(Box::new(Mutex::new(Some(Arc::new(ServoMedia::new())))));
        });
        let instance = unsafe { &*INSTANCE }.lock().unwrap();
        match *instance {
            Some(ref instance) => Ok(instance.clone()),
            None => Err(()),
        }
    }

    pub fn create_audio_context(&self, options: AudioContextOptions) -> AudioContext<Backend> {
        AudioContext::new(options)
    }
}
