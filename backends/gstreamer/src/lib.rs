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
extern crate gstreamer_player as gst_player;
extern crate gstreamer_sdp as gst_sdp;
extern crate gstreamer_video as gst_video;
extern crate gstreamer_webrtc as gst_webrtc;
extern crate ipc_channel;
#[macro_use]
extern crate lazy_static;

extern crate servo_media_audio;
extern crate servo_media_player;
extern crate servo_media_webrtc;
extern crate url;

use servo_media_audio::sink::AudioSinkError;
use servo_media_audio::AudioBackend;
use servo_media_player::PlayerBackend;
use servo_media_webrtc::{WebRtcBackend, WebRtcController, WebRtcSignaller};

pub mod audio_decoder;
pub mod audio_sink;
pub mod media_capture;
pub mod media_stream;
pub mod player;
mod source;
pub mod webrtc;

pub struct GStreamerBackend;

impl AudioBackend for GStreamerBackend {
    type Decoder = audio_decoder::GStreamerAudioDecoder;
    type Sink = audio_sink::GStreamerAudioSink;
    fn make_decoder() -> Self::Decoder {
        audio_decoder::GStreamerAudioDecoder::new()
    }
    fn make_sink() -> Result<Self::Sink, AudioSinkError> {
        audio_sink::GStreamerAudioSink::new()
    }
}

impl PlayerBackend for GStreamerBackend {
    type Player = player::GStreamerPlayer;
    fn make_player() -> Self::Player {
        player::GStreamerPlayer::new()
    }
}

impl WebRtcBackend for GStreamerBackend {
    type Controller = webrtc::GStreamerWebRtcController;

    fn construct_webrtc_controller(
        signaller: Box<WebRtcSignaller>,
        thread: WebRtcController,
    ) -> Self::Controller {
        webrtc::construct(signaller, thread)
    }
}

impl GStreamerBackend {
    pub fn init() {
        gst::init().unwrap();
    }

    pub fn create_audiostream() -> media_stream::GStreamerMediaStream {
        media_stream::GStreamerMediaStream::create_audio()
    }

    pub fn create_videostream() -> media_stream::GStreamerMediaStream {
        media_stream::GStreamerMediaStream::create_video()
    }

    pub fn create_stream_output() -> media_stream::MediaSink {
        media_stream::MediaSink::new()
    }

    pub fn create_audioinput_stream() -> Option<media_stream::GStreamerMediaStream> {
        media_capture::create_audioinput_stream()
    }

    pub fn create_videoinput_stream() -> Option<media_stream::GStreamerMediaStream> {
        media_capture::create_videoinput_stream()
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
