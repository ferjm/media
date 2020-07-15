#![feature(nll)]
extern crate boxfnonce;
extern crate byte_slice_cast;
extern crate mime;

extern crate glib_sys as glib_ffi;
extern crate gstreamer_sys as gst_ffi;

#[macro_use]
extern crate glib;
#[macro_use]
extern crate gstreamer as gst;
extern crate gstreamer_app as gst_app;
extern crate gstreamer_audio as gst_audio;
extern crate gstreamer_base as gst_base;
extern crate gstreamer_player as gst_player;
extern crate gstreamer_sdp as gst_sdp;
extern crate gstreamer_video as gst_video;
extern crate gstreamer_webrtc as gst_webrtc;
extern crate ipc_channel;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate log;

extern crate servo_media;
extern crate servo_media_audio;
extern crate servo_media_gstreamer_render;
extern crate servo_media_player;
extern crate servo_media_streams;
extern crate servo_media_traits;
extern crate servo_media_webrtc;
extern crate url;

pub mod audio_decoder;
pub mod audio_sink;
pub mod audio_stream_reader;
mod datachannel;
mod device_monitor;
pub mod media_capture;
pub mod media_stream;
mod media_stream_source;
pub mod player;
mod registry_scanner;
mod render;
mod source;
pub mod webrtc;

use device_monitor::GStreamerDeviceMonitor;
use gst::ClockExt;
use ipc_channel::ipc::IpcSender;
use media_stream::GStreamerMediaStream;
use mime::Mime;
use registry_scanner::GSTREAMER_REGISTRY_SCANNER;
use servo_media::{Backend, BackendInit, SupportsMediaType};
use servo_media_audio::context::{AudioContext, AudioContextOptions};
use servo_media_audio::decoder::AudioDecoder;
use servo_media_audio::sink::AudioSinkError;
use servo_media_audio::{AudioBackend, AudioStreamReader};
use servo_media_player::audio::AudioRenderer;
use servo_media_player::context::PlayerGLContext;
use servo_media_player::video::VideoFrameRenderer;
use servo_media_player::{Player, PlayerEvent, StreamType};
use servo_media_streams::capture::MediaTrackConstraintSet;
use servo_media_streams::device_monitor::MediaDeviceMonitor;
use servo_media_streams::registry::MediaStreamId;
use servo_media_streams::{MediaOutput, MediaSocket, MediaSource, MediaStreamType};
use servo_media_traits::{BackendMsg, ClientContextId, MediaInstance};
use servo_media_webrtc::{WebRtcBackend, WebRtcController, WebRtcSignaller};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::vec::Vec;

lazy_static! {
    static ref BACKEND_BASE_TIME: gst::ClockTime = gst::SystemClock::obtain().get_time();
}

pub struct GStreamerBackend {
    capture_mocking: AtomicBool,
    instances: Arc<Mutex<HashMap<ClientContextId, Vec<(usize, Weak<Mutex<dyn MediaInstance>>)>>>>,
    next_instance_id: AtomicUsize,
    /// Channel to communicate media instances with its owner Backend.
    backend_chan: Arc<Mutex<Sender<BackendMsg>>>,
}

#[derive(Debug)]
pub struct ErrorLoadingPlugins(Vec<&'static str>);

impl GStreamerBackend {
    pub fn init_with_plugins(
        plugin_dir: PathBuf,
        plugins: &[&'static str],
    ) -> Result<Box<dyn Backend>, ErrorLoadingPlugins> {
        gst::init().unwrap();

        let mut errors = vec![];
        for plugin in plugins {
            let mut path = plugin_dir.clone();
            path.push(plugin);
            let registry = gst::Registry::get();
            if let Ok(p) = gst::Plugin::load_file(&path) {
                if registry.add_plugin(&p).is_ok() {
                    continue;
                }
            }
            errors.push(*plugin);
        }

        if !errors.is_empty() {
            return Err(ErrorLoadingPlugins(errors));
        }

        let instances: Arc<
            Mutex<HashMap<ClientContextId, Vec<(usize, Weak<Mutex<dyn MediaInstance>>)>>>,
        > = Arc::new(Mutex::new(HashMap::new()));

        let instances_ = instances.clone();
        let (backend_chan, recvr) = mpsc::channel();
        thread::Builder::new()
            .name("GStreamerBackend ShutdownThread".to_owned())
            .spawn(move || {
                match recvr.recv().unwrap() {
                    BackendMsg::Shutdown(context_id, instance_id) => {
                        if let Some(vec) = instances_.lock().unwrap().get_mut(&context_id) {
                            vec.retain(|m| m.0 != instance_id);
                            if vec.is_empty() {
                                instances_.lock().unwrap().remove(&context_id);
                            }
                        }
                    }
                };
            })
            .unwrap();

        Ok(Box::new(GStreamerBackend {
            capture_mocking: AtomicBool::new(false),
            instances,
            next_instance_id: AtomicUsize::new(0),
            backend_chan: Arc::new(Mutex::new(backend_chan)),
        }))
    }

    fn media_instance_action(
        &self,
        id: &ClientContextId,
        cb: &dyn Fn(&dyn MediaInstance) -> Result<(), ()>,
    ) {
        let mut instances = self.instances.lock().unwrap();
        match instances.get_mut(id) {
            Some(vec) => vec.retain(|(_, weak)| {
                if let Some(instance) = weak.upgrade() {
                    if cb(&*(instance.lock().unwrap())).is_err() {
                        warn!("Error executing media instance action");
                    }
                    true
                } else {
                    false
                }
            }),
            None => {
                warn!("Trying to exec media action on an unknown client context");
            }
        }
    }
}

impl Backend for GStreamerBackend {
    fn create_player(
        &self,
        context_id: &ClientContextId,
        stream_type: StreamType,
        sender: IpcSender<PlayerEvent>,
        renderer: Option<Arc<Mutex<dyn VideoFrameRenderer>>>,
        audio_renderer: Option<Arc<Mutex<dyn AudioRenderer>>>,
        gl_context: Box<dyn PlayerGLContext>,
    ) -> Arc<Mutex<dyn Player>> {
        let id = self.next_instance_id.fetch_add(1, Ordering::Relaxed);
        let player = Arc::new(Mutex::new(player::GStreamerPlayer::new(
            id,
            context_id,
            self.backend_chan.clone(),
            stream_type,
            sender,
            renderer,
            audio_renderer,
            gl_context,
        )));
        let mut instances = self.instances.lock().unwrap();
        let entry = instances.entry(*context_id).or_insert(Vec::new());
        entry.push((id, Arc::downgrade(&player).clone()));
        player
    }

    fn create_audio_context(
        &self,
        client_context_id: &ClientContextId,
        options: AudioContextOptions,
    ) -> Arc<Mutex<AudioContext>> {
        let id = self.next_instance_id.fetch_add(1, Ordering::Relaxed);
        let context = Arc::new(Mutex::new(AudioContext::new::<Self>(
            id,
            client_context_id,
            self.backend_chan.clone(),
            options,
        )));
        let mut instances = self.instances.lock().unwrap();
        let entry = instances.entry(*client_context_id).or_insert(Vec::new());
        entry.push((id, Arc::downgrade(&context).clone()));
        context
    }

    fn create_webrtc(&self, signaller: Box<dyn WebRtcSignaller>) -> WebRtcController {
        WebRtcController::new::<Self>(signaller)
    }

    fn create_audiostream(&self) -> MediaStreamId {
        GStreamerMediaStream::create_audio()
    }

    fn create_videostream(&self) -> MediaStreamId {
        GStreamerMediaStream::create_video()
    }

    fn create_stream_output(&self) -> Box<dyn MediaOutput> {
        Box::new(media_stream::MediaSink::new())
    }

    fn create_stream_and_socket(
        &self,
        ty: MediaStreamType,
    ) -> (Box<dyn MediaSocket>, MediaStreamId) {
        let (id, socket) = GStreamerMediaStream::create_proxy(ty);
        (Box::new(socket), id)
    }

    fn create_audioinput_stream(&self, set: MediaTrackConstraintSet) -> Option<MediaStreamId> {
        if self.capture_mocking.load(Ordering::Acquire) {
            // XXXManishearth we should caps filter this
            return Some(self.create_audiostream());
        }
        media_capture::create_audioinput_stream(set)
    }

    fn create_videoinput_stream(
        &self,
        set: MediaTrackConstraintSet,
        source: MediaSource,
    ) -> Option<MediaStreamId> {
        if self.capture_mocking.load(Ordering::Acquire) {
            // XXXManishearth we should caps filter this
            return Some(self.create_videostream());
        }
        media_capture::create_videoinput_stream(set, source)
    }

    fn push_stream_data(&self, stream: &MediaStreamId, data: Vec<u8>) {
        println!("push_stream_data {:?} bytes to {:?}", stream, data.len());
        GStreamerMediaStream::push_data(stream, data);
    }

    fn can_play_type(&self, media_type: &str) -> SupportsMediaType {
        if let Ok(mime) = media_type.parse::<Mime>() {
            // XXX GStreamer is currently not very reliable playing OGG and most of
            //     the media related WPTs uses OGG if we report that we are able to
            //     play this type. So we report that we are unable to play it to force
            //     the usage of other types.
            //     https://gitlab.freedesktop.org/gstreamer/gst-plugins-base/issues/520
            if mime.subtype() == mime::OGG {
                return SupportsMediaType::No;
            }

            let mime_type = mime.type_().as_str().to_owned() + "/" + mime.subtype().as_str();
            let codecs = match mime.get_param("codecs") {
                Some(codecs) => codecs
                    .as_str()
                    .split(',')
                    .map(|codec| codec.trim())
                    .collect(),
                None => vec![],
            };

            if GSTREAMER_REGISTRY_SCANNER.is_container_type_supported(&mime_type) {
                if codecs.is_empty() {
                    return SupportsMediaType::Maybe;
                } else if GSTREAMER_REGISTRY_SCANNER.are_all_codecs_supported(&codecs) {
                    return SupportsMediaType::Probably;
                } else {
                    return SupportsMediaType::No;
                }
            }
        }
        SupportsMediaType::No
    }

    fn set_capture_mocking(&self, mock: bool) {
        self.capture_mocking.store(mock, Ordering::Release)
    }

    fn mute(&self, id: &ClientContextId, val: bool) {
        self.media_instance_action(
            id,
            &(move |instance: &dyn MediaInstance| instance.mute(val)),
        );
    }

    fn suspend(&self, id: &ClientContextId) {
        self.media_instance_action(id, &|instance: &dyn MediaInstance| instance.suspend());
    }

    fn resume(&self, id: &ClientContextId) {
        self.media_instance_action(id, &|instance: &dyn MediaInstance| instance.resume());
    }

    fn get_device_monitor(&self) -> Box<dyn MediaDeviceMonitor> {
        Box::new(GStreamerDeviceMonitor::new())
    }
}

impl AudioBackend for GStreamerBackend {
    type Sink = audio_sink::GStreamerAudioSink;
    fn make_decoder() -> Box<dyn AudioDecoder> {
        Box::new(audio_decoder::GStreamerAudioDecoder::new())
    }
    fn make_sink() -> Result<Self::Sink, AudioSinkError> {
        audio_sink::GStreamerAudioSink::new()
    }

    fn make_streamreader(id: MediaStreamId, sample_rate: f32) -> Box<dyn AudioStreamReader + Send> {
        Box::new(audio_stream_reader::GStreamerAudioStreamReader::new(id, sample_rate).unwrap())
    }
}

impl WebRtcBackend for GStreamerBackend {
    type Controller = webrtc::GStreamerWebRtcController;

    fn construct_webrtc_controller(
        signaller: Box<dyn WebRtcSignaller>,
        thread: WebRtcController,
    ) -> Self::Controller {
        webrtc::construct(signaller, thread).expect("WebRTC creation failed")
    }
}

impl BackendInit for GStreamerBackend {
    fn init() -> Box<dyn Backend> {
        Self::init_with_plugins(PathBuf::new(), &[]).unwrap()
    }
}

pub fn set_element_flags<T: glib::IsA<gst::Object> + glib::IsA<gst::Element>>(
    element: &T,
    flags: gst::ElementFlags,
) {
    unsafe {
        use glib::translate::ToGlib;

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
