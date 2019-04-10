#![feature(nll)]

extern crate boxfnonce;
extern crate byte_slice_cast;

extern crate glib_sys as glib_ffi;
extern crate gstreamer_sys as gst_ffi;

#[macro_use]
extern crate glib;
#[macro_use]
extern crate gstreamer as gst;
extern crate gstreamer_app as gst_app;
extern crate gstreamer_audio as gst_audio;
extern crate gstreamer_gl as gst_gl;
extern crate gstreamer_player as gst_player;
extern crate gstreamer_sdp as gst_sdp;
extern crate gstreamer_video as gst_video;
extern crate gstreamer_webrtc as gst_webrtc;
extern crate ipc_channel;
#[macro_use]
extern crate lazy_static;

extern crate servo_media;
extern crate servo_media_audio;
extern crate servo_media_player;
extern crate servo_media_streams;
extern crate servo_media_webrtc;
extern crate url;

pub mod audio_decoder;
pub mod audio_sink;
pub mod media_capture;
pub mod media_stream;
mod media_stream_source;
pub mod player;
mod source;
pub mod webrtc;

use gst::ClockExt;
use ipc_channel::ipc::IpcSender;
use media_stream::GStreamerMediaStream;
use servo_media::{Backend, BackendInit};
use servo_media_audio::context::{AudioContext, AudioContextOptions};
use servo_media_audio::decoder::AudioDecoder;
use servo_media_audio::sink::AudioSinkError;
use servo_media_audio::AudioBackend;
use servo_media_player::{FrameRenderer, Player, PlayerEvent, StreamType};
use servo_media_streams::capture::MediaTrackConstraintSet;
use servo_media_streams::registry::MediaStreamId;
use servo_media_streams::MediaOutput;
use servo_media_webrtc::{WebRtcBackend, WebRtcController, WebRtcSignaller};
use std::sync::{Arc, Mutex};

lazy_static! {
    static ref BACKEND_BASE_TIME: gst::ClockTime = { gst::SystemClock::obtain().get_time() };
}

pub struct GStreamerBackend;

impl Backend for GStreamerBackend {
    fn create_player(
        &self,
        stream_type: StreamType,
        event_handler: IpcSender<PlayerEvent>,
        renderer: Arc<Mutex<FrameRenderer>>,
    ) -> Box<Player> {
        Box::new(player::GStreamerPlayer::new(
            stream_type,
            event_handler,
            renderer,
        ))
    }

    fn create_audio_context(&self, options: AudioContextOptions) -> AudioContext {
        AudioContext::new::<Self>(options)
    }

    fn create_webrtc(&self, signaller: Box<WebRtcSignaller>) -> WebRtcController {
        WebRtcController::new::<Self>(signaller)
    }

    fn create_audiostream(&self) -> MediaStreamId {
        GStreamerMediaStream::create_audio()
    }

    fn create_videostream(&self) -> MediaStreamId {
        GStreamerMediaStream::create_video()
    }

    fn create_stream_output(&self) -> Box<MediaOutput> {
        Box::new(media_stream::MediaSink::new())
    }

    fn create_audioinput_stream(&self, set: MediaTrackConstraintSet) -> Option<MediaStreamId> {
        media_capture::create_audioinput_stream(set)
    }

    fn create_videoinput_stream(&self, set: MediaTrackConstraintSet) -> Option<MediaStreamId> {
        media_capture::create_videoinput_stream(set)
    }
}

impl AudioBackend for GStreamerBackend {
    type Sink = audio_sink::GStreamerAudioSink;
    fn make_decoder() -> Box<AudioDecoder> {
        Box::new(audio_decoder::GStreamerAudioDecoder::new())
    }
    fn make_sink() -> Result<Self::Sink, AudioSinkError> {
        audio_sink::GStreamerAudioSink::new()
    }
}

impl WebRtcBackend for GStreamerBackend {
    type Controller = webrtc::GStreamerWebRtcController;

    fn construct_webrtc_controller(
        signaller: Box<WebRtcSignaller>,
        thread: WebRtcController,
    ) -> Self::Controller {
        webrtc::construct(signaller, thread).expect("WebRTC creation failed")
    }
}

impl BackendInit for GStreamerBackend {
    fn init() -> Box<Backend> {
        gst::init().unwrap();
        Box::new(GStreamerBackend)
    }
}

pub fn set_element_flags<T: glib::IsA<gst::Object> + glib::IsA<gst::Element>>(
    element: &T,
    flags: gst::ElementFlags,
) {
    unsafe {
        use glib::translate::ToGlib;
        use gst_ffi;

        let ptr: *mut gst_ffi::GstObject = element.as_ptr() as *mut _;
        let _guard = MutexGuard::lock(&(*ptr).lock);
        (*ptr).flags |= flags.to_glib();
    }
}

struct MutexGuard<'a>(&'a glib_ffi::GMutex);

impl<'a> MutexGuard<'a> {
    pub fn lock(mutex: &'a glib_ffi::GMutex) -> Self {
        use glib::translate::mut_override;
        unsafe {
            glib_ffi::g_mutex_lock(mut_override(mutex));
        }
        MutexGuard(mutex)
    }
}

impl<'a> Drop for MutexGuard<'a> {
    fn drop(&mut self) {
        use glib::translate::mut_override;
        unsafe {
            glib_ffi::g_mutex_unlock(mut_override(self.0));
        }
    }
}
