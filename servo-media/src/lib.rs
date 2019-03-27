pub extern crate servo_media_audio as audio;

pub extern crate servo_media_player as player;
pub extern crate servo_media_webrtc as webrtc;
pub extern crate servo_media_streams as streams;
use std::ops::Deref;
use std::sync::{self, Arc, Mutex, Once};

use audio::context::{AudioContext, AudioContextOptions};
use player::Player;
use streams::{AutoMediaStream, MediaStream, MediaStreamCallbacks, MediaOutput};
use streams::capture::MediaTrackConstraintSet;
use webrtc::{WebRtcController, WebRtcSignaller};

pub struct ServoMedia(Box<Backend>);

static INITIALIZER: Once = sync::ONCE_INIT;
static mut INSTANCE: *mut Mutex<Option<Arc<ServoMedia>>> = 0 as *mut _;

pub trait BackendInit {
    fn init() -> Box<Backend>;
}

pub trait Backend: Send + Sync {
    fn create_player(&self) -> Box<Player>;
    fn create_audiostream(&self) -> Box<MediaStream>;
    fn create_videostream(&self) -> Box<MediaStream>;
    fn create_stream_output(&self) -> Box<MediaOutput>;
    fn create_audioinput_stream(&self, set: MediaTrackConstraintSet) -> Option<Box<MediaStream>>;
    fn create_videoinput_stream(&self, set: MediaTrackConstraintSet) -> Option<Box<MediaStream>>;
    fn create_audio_context(&self, options: AudioContextOptions) -> AudioContext;
    fn create_webrtc(&self, signaller: Box<WebRtcSignaller>) -> WebRtcController;

    fn play_auto_media_stream(&self, callbacks: MediaStreamCallbacks);
}

impl ServoMedia {
    pub fn init<B: BackendInit>() {
        INITIALIZER.call_once(|| unsafe {
            let instance = Arc::new(ServoMedia(B::init()));
            INSTANCE = Box::into_raw(Box::new(Mutex::new(Some(instance))));
        })
    }

    pub fn get() -> Result<Arc<ServoMedia>, ()> {
        let instance = unsafe { &*INSTANCE }.lock().unwrap();
        match *instance {
            Some(ref instance) => Ok(instance.clone()),
            None => Err(()),
        }
    }
}

impl Deref for ServoMedia {
    type Target = Backend + 'static;
    fn deref(&self) -> &(Backend + 'static) {
        &*self.0
    }
}
