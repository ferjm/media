pub extern crate servo_media_audio as audio;
pub extern crate servo_media_player as player;
pub extern crate servo_media_streams as streams;
pub extern crate servo_media_traits as traits;
pub extern crate servo_media_webrtc as webrtc;

pub use traits::*;

use std::ops::Deref;
use std::sync::{Arc, Mutex, Once};

use audio::context::{AudioContext, AudioContextOptions};
use player::audio::AudioRenderer;
use player::context::PlayerGLContext;
use player::ipc_channel::ipc::IpcSender;
use player::video::VideoFrameRenderer;
use player::{Player, PlayerEvent, StreamType};
use streams::capture::MediaTrackConstraintSet;
use streams::device_monitor::MediaDeviceMonitor;
use streams::registry::MediaStreamId;
use streams::{MediaOutput, MediaSocket, MediaStreamType};
use webrtc::{WebRtcController, WebRtcSignaller};

pub struct ServoMedia(Box<dyn Backend>);

static INITIALIZER: Once = Once::new();
static mut INSTANCE: *mut Mutex<Option<Arc<ServoMedia>>> = std::ptr::null_mut();

#[derive(Debug)]
pub enum ServoMediaError {
    /// Specific Backend implementation error.
    Backend(String),
    /// The given MediaStreamId is not registered or invalid.
    UnknownStream,
}

pub trait BackendInit {
    fn init() -> Box<dyn Backend>;
}

pub trait Backend: Send + Sync {
    fn create_player(
        &self,
        id: &ClientContextId,
        stream_type: StreamType,
        sender: IpcSender<PlayerEvent>,
        video_renderer: Option<Arc<Mutex<dyn VideoFrameRenderer>>>,
        audio_renderer: Option<Arc<Mutex<dyn AudioRenderer>>>,
        gl_context: Box<dyn PlayerGLContext>,
    ) -> Arc<Mutex<dyn Player>>;
    fn create_audiostream(&self) -> MediaStreamId;
    fn create_videostream(&self) -> MediaStreamId;
    fn create_stream_output(&self) -> Box<dyn MediaOutput>;
    fn create_stream_and_socket(
        &self,
        ty: MediaStreamType,
    ) -> (Box<dyn MediaSocket>, MediaStreamId);
    fn create_audioinput_stream(&self, set: MediaTrackConstraintSet) -> Option<MediaStreamId>;
    fn create_videoinput_stream(&self, set: MediaTrackConstraintSet) -> Option<MediaStreamId>;
    fn create_client_capture_stream(&self) -> Result<MediaStreamId, ServoMediaError>;
    fn push_client_capture_stream_data(
        &self,
        stream_id: &MediaStreamId,
        data: Vec<u8>,
    ) -> Result<(), ServoMediaError>;
    fn create_audio_context(
        &self,
        id: &ClientContextId,
        options: AudioContextOptions,
    ) -> Arc<Mutex<AudioContext>>;
    fn create_webrtc(&self, signaller: Box<dyn WebRtcSignaller>) -> WebRtcController;
    fn can_play_type(&self, media_type: &str) -> SupportsMediaType;
    fn set_capture_mocking(&self, _mock: bool) {}
    /// Allow muting/unmuting the media instances associated with the given client context identifier.
    /// Backend implementations are responsible for keeping a match between client contexts
    /// and the media instances created for these contexts.
    /// The client context identifier is currently an abstraction of Servo's PipelineId.
    fn mute(&self, _id: &ClientContextId, _val: bool) {}
    /// Allow suspending the activity of all media instances associated with the given client
    /// context identifier.
    /// Note that suspending does not involve releasing any resources, so media playback can
    /// be restarted.
    /// Backend implementations are responsible for keeping a match between client contexts
    /// and the media instances created for these contexts.
    /// The client context identifier is currently an abstraction of Servo's PipelineId.
    fn suspend(&self, _id: &ClientContextId) {}
    /// Allow resuming the activity of all the media instances associated with the given client
    /// context identifier.
    /// Backend implementations are responsible for keeping a match between client contexts
    /// and the media instances created for these contexts.
    /// The client context identifier is currently an abstraction of Servo's PipelineId.
    fn resume(&self, _id: &ClientContextId) {}

    fn get_device_monitor(&self) -> Box<dyn MediaDeviceMonitor>;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SupportsMediaType {
    Maybe,
    No,
    Probably,
}

impl ServoMedia {
    pub fn init<B: BackendInit>() {
        INITIALIZER.call_once(|| unsafe {
            let instance = Arc::new(ServoMedia(B::init()));
            INSTANCE = Box::into_raw(Box::new(Mutex::new(Some(instance))));
        })
    }

    pub fn init_with_backend(backend: Box<dyn Backend>) {
        INITIALIZER.call_once(|| unsafe {
            let instance = Arc::new(ServoMedia(backend));
            INSTANCE = Box::into_raw(Box::new(Mutex::new(Some(instance))));
        })
    }

    pub fn get() -> Result<Arc<ServoMedia>, ()> {
        if INITIALIZER.is_completed() {
            let instance = unsafe { &*INSTANCE }.lock().unwrap();
            match *instance {
                Some(ref instance) => Ok(instance.clone()),
                None => Err(()),
            }
        } else {
            Err(())
        }
    }
}

impl Deref for ServoMedia {
    type Target = dyn Backend + 'static;
    fn deref(&self) -> &(dyn Backend + 'static) {
        &*self.0
    }
}
