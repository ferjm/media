use super::BACKEND_BASE_TIME;
use glib;
use glib::prelude::*;
use gst;
use gst::prelude::*;
use gst_app;
use gst_player;
use gst_player::prelude::*;
use ipc_channel::ipc::IpcSender;
use media_stream::GStreamerMediaStream;
use media_stream_source::{register_servo_media_stream_src, ServoMediaStreamSrc};
use render::GStreamerRender;
use servo_media_player::context::PlayerGLContext;
use servo_media_player::frame::{Frame, FrameRenderer};
use servo_media_player::metadata::Metadata;
use servo_media_player::{PlaybackState, Player, PlayerError, PlayerEvent, StreamType};
use servo_media_streams::registry::{get_stream, MediaStreamId};
use source::{register_servo_src, ServoSrc};
use std::cell::RefCell;
use std::error::Error;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, Once};
use std::time;
use std::u64;

const MAX_BUFFER_SIZE: i32 = 500 * 1024 * 1024;

fn metadata_from_media_info(media_info: &gst_player::PlayerMediaInfo) -> Result<Metadata, ()> {
    let dur = media_info.get_duration();
    let duration = if dur != gst::ClockTime::none() {
        let mut nanos = dur.nanoseconds().ok_or_else(|| ())?;
        nanos = nanos % 1_000_000_000;
        let seconds = dur.seconds().ok_or_else(|| ())?;
        Some(time::Duration::new(seconds, nanos as u32))
    } else {
        None
    };

    let mut audio_tracks = Vec::new();
    let mut video_tracks = Vec::new();

    let format = media_info
        .get_container_format()
        .unwrap_or_else(|| glib::GString::from(""))
        .to_string();

    for stream_info in media_info.get_stream_list() {
        let stream_type = stream_info.get_stream_type();
        match stream_type.as_str() {
            "audio" => {
                let codec = stream_info
                    .get_codec()
                    .unwrap_or_else(|| glib::GString::from(""))
                    .to_string();
                audio_tracks.push(codec);
            }
            "video" => {
                let codec = stream_info
                    .get_codec()
                    .unwrap_or_else(|| glib::GString::from(""))
                    .to_string();
                video_tracks.push(codec);
            }
            _ => {}
        }
    }

    let mut width: u32 = 0;
    let height: u32 = if media_info.get_number_of_video_streams() > 0 {
        let first_video_stream = &media_info.get_video_streams()[0];
        width = first_video_stream.get_width() as u32;
        first_video_stream.get_height() as u32
    } else {
        0
    };

    let is_seekable = media_info.is_seekable();
    let is_live = media_info.is_live();

    Ok(Metadata {
        duration,
        width,
        height,
        format,
        is_seekable,
        audio_tracks,
        video_tracks,
        is_live,
    })
}

#[derive(PartialEq)]
enum PlayerSource {
    Seekable(ServoSrc),
    Stream(ServoMediaStreamSrc),
}

struct PlayerInner {
    player: gst_player::Player,
    source: Option<PlayerSource>,
    appsink: gst_app::AppSink,
    input_size: u64,
    rate: f64,
    stream_type: StreamType,
    last_metadata: Option<Metadata>,
    cat: gst::DebugCategory,
    enough_data: Arc<AtomicBool>,
}

impl PlayerInner {
    pub fn set_input_size(&mut self, size: u64) -> Result<(), PlayerError> {
        // Set input_size to proxy its value, since it
        // could be set by the user before calling .setup().
        self.input_size = size;
        match self.source {
            // The input size is only useful for seekable streams.
            Some(ref mut source) => {
                if let PlayerSource::Seekable(source) = source {
                    source.set_size(if size > 0 {
                        size as i64
                    } else {
                        -1 // live source
                    });
                }
            }
            _ => (),
        }
        Ok(())
    }

    pub fn set_mute(&mut self, val: bool) -> Result<(), PlayerError> {
        self.player.set_mute(val);
        Ok(())
    }

    pub fn set_rate(&mut self, rate: f64) -> Result<(), PlayerError> {
        // This method may be called before the player setup is done, so we safe the rate value
        // and set it once the player is ready and after getting the media info
        self.rate = rate;
        if let Some(ref metadata) = self.last_metadata {
            if !metadata.is_seekable {
                gst_warning!(self.cat, obj: &self.player,
                             "Player must be seekable in order to set the playback rate");
                return Err(PlayerError::NonSeekableStream);
            }
            self.player.set_rate(rate);
        }
        Ok(())
    }

    pub fn play(&mut self) -> Result<(), PlayerError> {
        self.player.play();
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), PlayerError> {
        self.player.stop();
        self.last_metadata = None;
        self.source = None;
        Ok(())
    }

    pub fn pause(&mut self) -> Result<(), PlayerError> {
        self.player.pause();
        Ok(())
    }

    pub fn end_of_stream(&mut self) -> Result<(), PlayerError> {
        match self.source {
            Some(ref mut source) => {
                if let PlayerSource::Seekable(source) = source {
                    source
                        .end_of_stream()
                        .map(|_| ())
                        .map_err(|_| PlayerError::EOSFailed)
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        }
    }

    pub fn seek(&mut self, time: f64) -> Result<(), PlayerError> {
        if self.stream_type != StreamType::Seekable {
            return Err(PlayerError::NonSeekableStream);
        }
        if let Some(ref metadata) = self.last_metadata {
            if let Some(ref duration) = metadata.duration {
                if duration < &time::Duration::new(time as u64, 0) {
                    gst_warning!(self.cat, obj: &self.player, "Trying to seek out of range");
                    return Err(PlayerError::SeekOutOfRange);
                }
            }
        }

        let time = time * 1_000_000_000.;
        self.player.seek(gst::ClockTime::from_nseconds(time as u64));
        Ok(())
    }

    pub fn set_volume(&mut self, value: f64) -> Result<(), PlayerError> {
        self.player.set_volume(value);
        Ok(())
    }

    pub fn push_data(&mut self, data: Vec<u8>) -> Result<(), PlayerError> {
        if let Some(ref mut source) = self.source {
            if let PlayerSource::Seekable(source) = source {
                if self.enough_data.load(Ordering::Relaxed) {
                    return Err(PlayerError::EnoughData);
                }
                return source
                    .push_buffer(data)
                    .map(|_| ())
                    .map_err(|_| PlayerError::BufferPushFailed);
            }
        }
        Err(PlayerError::BufferPushFailed)
    }

    pub fn set_src(&mut self, source: PlayerSource) {
        self.source = Some(source);
    }

    pub fn buffered(&mut self) -> Result<Vec<Range<f64>>, PlayerError> {
        let mut result = vec![];
        if let Some(ref metadata) = self.last_metadata {
            if let Some(ref duration) = metadata.duration {
                let pipeline = self.player.get_pipeline();
                let mut buffering = gst::Query::new_buffering(gst::Format::Percent);
                if pipeline.query(&mut buffering) {
                    let ranges = buffering.get_ranges();
                    for i in 0..ranges.len() {
                        let start = ranges[i].0;
                        let end = ranges[i].1;
                        let start = (if let gst::GenericFormattedValue::Percent(start) = start {
                            start.unwrap()
                        } else {
                            0
                        } * duration.as_secs() as u32
                            / (gst::FORMAT_PERCENT_MAX)) as f64;
                        let end = (if let gst::GenericFormattedValue::Percent(end) = end {
                            end.unwrap()
                        } else {
                            0
                        } * duration.as_secs() as u32
                            / (gst::FORMAT_PERCENT_MAX)) as f64;
                        result.push(Range { start, end });
                    }
                }
            }
        }
        Ok(result)
    }

    fn set_stream(&mut self, stream: &MediaStreamId) -> Result<(), PlayerError> {
        debug_assert!(self.stream_type == StreamType::Stream);
        if let Some(ref source) = self.source {
            if let PlayerSource::Stream(source) = source {
                let stream =
                    get_stream(stream).expect("Media streams registry does not contain such ID");
                let mut stream = stream.lock().unwrap();
                if let Some(mut stream) = stream.as_mut_any().downcast_mut::<GStreamerMediaStream>()
                {
                    let playbin = self
                        .player
                        .get_pipeline()
                        .dynamic_cast::<gst::Pipeline>()
                        .unwrap();
                    let clock = gst::SystemClock::obtain();
                    playbin.set_base_time(*BACKEND_BASE_TIME);
                    playbin.set_start_time(gst::ClockTime::none());
                    playbin.use_clock(Some(&clock));

                    source.set_stream(&mut stream);
                    return Ok(());
                }
            }
        }
        Err(PlayerError::SetStreamFailed)
    }

    fn disable_video(&self) {
        self.player.set_video_track_enabled(false);
    }
}

type PlayerEventObserver = IpcSender<PlayerEvent>;
struct PlayerEventObserverList {
    observers: Vec<PlayerEventObserver>,
}

impl PlayerEventObserverList {
    fn new() -> Self {
        Self {
            observers: Vec::new(),
        }
    }

    fn register(&mut self, observer: PlayerEventObserver) {
        self.observers.push(observer);
    }

    fn notify(&self, event: PlayerEvent) {
        for observer in &self.observers {
            observer.send(event.clone()).unwrap();
        }
    }

    fn clear(&mut self) {
        self.observers.clear();
    }
}

struct FrameRendererList {
    renderers: Vec<Arc<Mutex<FrameRenderer>>>,
}

impl FrameRendererList {
    fn new() -> Self {
        Self {
            renderers: Vec::new(),
        }
    }

    fn register(&mut self, renderer: Arc<Mutex<FrameRenderer>>) {
        self.renderers.push(renderer);
    }

    fn render(&self, frame: &Frame) -> Result<(), ()> {
        for renderer in &self.renderers {
            renderer.lock().unwrap().render(frame.clone());
        }
        Ok(())
    }

    fn clear(&mut self) {
        self.renderers.clear();
    }
}

pub struct GStreamerPlayer {
    inner: RefCell<Option<Arc<Mutex<PlayerInner>>>>,
    observers: Arc<Mutex<PlayerEventObserverList>>,
    renderers: Arc<Mutex<FrameRendererList>>,
    /// Indicates whether the setup was succesfully performed and
    /// we are ready to consume a/v data.
    is_ready: Arc<Once>,
    /// Indicates whether the type of media stream to be played is a live stream.
    stream_type: StreamType,
    /// Decorator used to setup the video sink and process the produced frames
    render: Arc<Mutex<GStreamerRender>>,
}

impl GStreamerPlayer {
    pub fn new(stream_type: StreamType, gl_context: Box<PlayerGLContext>) -> GStreamerPlayer {
        Self {
            inner: RefCell::new(None),
            observers: Arc::new(Mutex::new(PlayerEventObserverList::new())),
            renderers: Arc::new(Mutex::new(FrameRendererList::new())),
            is_ready: Arc::new(Once::new()),
            stream_type,
            render: Arc::new(Mutex::new(GStreamerRender::new(gl_context))),
        }
    }

    fn setup(&self) -> Result<(), PlayerError> {
        if self.inner.borrow().is_some() {
            return Ok(());
        }

        // Check that we actually have the elements that we
        // need to make this work.
        for element in vec!["playbin", "queue"].iter() {
            if gst::ElementFactory::find(element).is_none() {
                return Err(PlayerError::Backend(format!(
                    "Missing dependency: {}",
                    element
                )));
            }
        }

        let player = gst_player::Player::new(
            /* video renderer */ None, /* signal dispatcher */ None,
        );

        let pipeline = player.get_pipeline();

        // Set player to perform progressive downloading. This will make the
        // player store the downloaded media in a local temporary file for
        // faster playback of already-downloaded chunks.
        let flags = pipeline
            .get_property("flags")
            .expect("playbin doesn't have expected 'flags' property");
        let flags_class = match glib::FlagsClass::new(flags.type_()) {
            Some(flags) => flags,
            None => {
                return Err(PlayerError::Backend(
                    "FlagsClass creation failed".to_owned(),
                ));
            }
        };
        let flags_class = match flags_class.builder_with_value(flags) {
            Some(class) => class,
            None => {
                return Err(PlayerError::Backend(
                    "FlagsClass creation failed".to_owned(),
                ));
            }
        };
        let flags = match flags_class.set_by_nick("download").build() {
            Some(flags) => flags,
            None => {
                return Err(PlayerError::Backend(
                    "FlagsClass creation failed".to_owned(),
                ));
            }
        };
        pipeline
            .set_property("flags", &flags)
            .expect("playbin doesn't have expected 'flags' property");

        // Set max size for the player buffer.
        pipeline
            .set_property("buffer-size", &MAX_BUFFER_SIZE)
            .expect("playbin doesn't have expected 'buffer-size' property");

        // Set player position interval update to 0.5 seconds.
        let mut config = player.get_config();
        config.set_position_update_interval(500u32);
        player
            .set_config(config)
            .map_err(|e| PlayerError::Backend(e.to_string()))?;

        let appsink = self.render.lock().unwrap().setup_video_sink(&pipeline)?;

        // There's a known bug in gstreamer that may cause a wrong transition
        // to the ready state while setting the uri property:
        // http://cgit.freedesktop.org/gstreamer/gst-plugins-bad/commit/?id=afbbc3a97ec391c6a582f3c746965fdc3eb3e1f3
        // This may affect things like setting the config, so until the bug is
        // fixed, make sure that state dependent code happens before this line.
        // The estimated version for the fix is 1.14.5 / 1.15.1.
        // https://github.com/servo/servo/issues/22010#issuecomment-432599657
        let uri = match self.stream_type {
            StreamType::Stream => {
                register_servo_media_stream_src().map_err(|_| {
                    PlayerError::Backend("servomediastreamsrc registration error".to_owned())
                })?;
                "mediastream://".to_value()
            }
            StreamType::Seekable => {
                register_servo_src()
                    .map_err(|_| PlayerError::Backend("servosrc registration error".to_owned()))?;
                "servosrc://".to_value()
            }
        };
        player
            .set_property("uri", &uri)
            .expect("playbin doesn't have expected 'uri' property");

        *self.inner.borrow_mut() = Some(Arc::new(Mutex::new(PlayerInner {
            player,
            source: None,
            appsink,
            input_size: 0,
            rate: 1.0,
            stream_type: self.stream_type,
            last_metadata: None,
            cat: gst::DebugCategory::new(
                "servoplayer",
                gst::DebugColorFlags::empty(),
                "Servo player",
            ),
            enough_data: Arc::new(AtomicBool::new(false)),
        })));

        let inner = self.inner.borrow();
        let inner = inner.as_ref().unwrap();
        let observers = self.observers.clone();
        // Handle `end-of-stream` signal.
        inner
            .lock()
            .unwrap()
            .player
            .connect_end_of_stream(move |_| {
                observers.lock().unwrap().notify(PlayerEvent::EndOfStream);
            });

        let observers = self.observers.clone();
        // Handle `error` signal
        inner.lock().unwrap().player.connect_error(move |_, error| {
            observers
                .lock()
                .unwrap()
                .notify(PlayerEvent::Error(error.to_string()));
        });

        let observers = self.observers.clone();
        // Handle `state-changed` signal.
        inner
            .lock()
            .unwrap()
            .player
            .connect_state_changed(move |_, player_state| {
                let state = match player_state {
                    gst_player::PlayerState::Buffering => Some(PlaybackState::Buffering),
                    gst_player::PlayerState::Stopped => Some(PlaybackState::Stopped),
                    gst_player::PlayerState::Paused => Some(PlaybackState::Paused),
                    gst_player::PlayerState::Playing => Some(PlaybackState::Playing),
                    _ => None,
                };
                if let Some(v) = state {
                    observers
                        .lock()
                        .unwrap()
                        .notify(PlayerEvent::StateChanged(v));
                }
            });

        let observers = self.observers.clone();
        // Handle `position-update` signal.
        inner
            .lock()
            .unwrap()
            .player
            .connect_position_updated(move |_, position| {
                if let Some(seconds) = position.seconds() {
                    observers
                        .lock()
                        .unwrap()
                        .notify(PlayerEvent::PositionChanged(seconds));
                }
            });

        let observers = self.observers.clone();
        // Handle `seek-done` signal.
        inner
            .lock()
            .unwrap()
            .player
            .connect_seek_done(move |_, position| {
                if let Some(seconds) = position.seconds() {
                    observers
                        .lock()
                        .unwrap()
                        .notify(PlayerEvent::SeekDone(seconds));
                }
            });

        // Handle `media-info-updated` signal.
        let inner_clone = inner.clone();
        let observers = self.observers.clone();
        inner
            .lock()
            .unwrap()
            .player
            .connect_media_info_updated(move |_, info| {
                let mut inner = inner_clone.lock().unwrap();
                if let Ok(metadata) = metadata_from_media_info(info) {
                    if inner.last_metadata.as_ref() != Some(&metadata) {
                        inner.last_metadata = Some(metadata.clone());
                        if metadata.is_seekable {
                            inner.player.set_rate(inner.rate);
                        }
                        gst_info!(inner.cat, obj: &inner.player, "Metadata updated: {:?}", metadata);
                        observers
                            .lock()
                            .unwrap()
                            .notify(PlayerEvent::MetadataUpdated(metadata));
                    }
                }
            });

        // Handle `duration-changed` signal.
        let inner_clone = inner.clone();
        let observers = self.observers.clone();
        inner
            .lock()
            .unwrap()
            .player
            .connect_duration_changed(move |_, duration| {
                let mut inner = inner_clone.lock().unwrap();
                let duration = if duration != gst::ClockTime::none() {
                    let nanos = duration.nanoseconds();
                    if nanos.is_none() {
                        gst_info!(inner.cat, obj: &inner.player, "Could not get duration nanoseconds");
                        return;
                    }
                    let seconds = duration.seconds();
                    if seconds.is_none() {
                        gst_info!(inner.cat, obj: &inner.player, "Could not get duration seconds");
                        return;
                    }
                    Some(time::Duration::new(
                        seconds.unwrap(),
                        (nanos.unwrap() % 1_000_000_000) as u32,
                    ))
                } else {
                    None
                };
                let mut updated_metadata = None;
                if let Some(ref mut metadata) = inner.last_metadata {
                    metadata.duration = duration;
                    updated_metadata = Some(metadata.clone());
                }
                if updated_metadata.is_some() {
                    gst_info!(inner.cat, obj: &inner.player, "Duration updated: {:?}",
                              updated_metadata);
                    observers
                        .lock()
                        .unwrap()
                        .notify(PlayerEvent::MetadataUpdated(updated_metadata.unwrap()));
                }
            });

        let render = self.render.clone();
        let observers = self.observers.clone();
        let renderers = self.renderers.clone();
        // Set appsink callbacks.
        inner.lock().unwrap().appsink.set_callbacks(
            gst_app::AppSinkCallbacks::new()
                .new_preroll(|_| Ok(gst::FlowSuccess::Ok))
                .new_sample(move |appsink| {
                    let sample = appsink.pull_sample().ok_or(gst::FlowError::Eos)?;
                    let frame = render
                        .lock()
                        .unwrap()
                        .get_frame_from_sample(&sample)
                        .or_else(|_| Err(gst::FlowError::Error))?;
                    renderers
                        .lock()
                        .unwrap()
                        .render(&frame)
                        .map(|_| {
                            observers.lock().unwrap().notify(PlayerEvent::FrameUpdated);
                        })
                        .map_err(|_| gst::FlowError::Error)?;

                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        let (receiver, error_handler_id) = {
            let inner_clone = inner.clone();
            let mut inner = inner.lock().unwrap();
            let pipeline = inner.player.get_pipeline();

            let (sender, receiver) = mpsc::channel();

            let sender = Arc::new(Mutex::new(sender));
            let sender_clone = sender.clone();
            let is_ready_clone = self.is_ready.clone();
            let observers = self.observers.clone();
            let connect_result = pipeline.connect("source-setup", false, move |args| {
                let source = match args[1].get::<gst::Element>() {
                    Some(source) => source,
                    None => {
                        let _ = sender
                            .lock()
                            .unwrap()
                            .send(Err(PlayerError::Backend("Source setup failed".to_owned())));
                        return None;
                    }
                };

                let mut inner = inner_clone.lock().unwrap();
                let source = match inner.stream_type {
                    StreamType::Seekable => {
                        let servosrc = source
                            .clone()
                            .dynamic_cast::<ServoSrc>()
                            .expect("Source element is expected to be a ServoSrc!");

                        if inner.input_size > 0 {
                            servosrc.set_size(inner.input_size as i64);
                        }

                        let sender_clone = sender.clone();
                        let is_ready = is_ready_clone.clone();
                        let observers_ = observers.clone();
                        let observers__ = observers.clone();
                        let observers___ = observers.clone();
                        let servosrc_ = servosrc.clone();
                        let enough_data_ = inner.enough_data.clone();
                        let enough_data__ = inner.enough_data.clone();
                        servosrc.set_callbacks(
                            gst_app::AppSrcCallbacks::new()
                                .need_data(move |_, _| {
                                    enough_data_.store(false, Ordering::Relaxed);
                                    observers_.lock().unwrap().notify(PlayerEvent::NeedData);
                                })
                                .enough_data(move |_| {
                                    enough_data__.store(true, Ordering::Relaxed);
                                    observers__.lock().unwrap().notify(PlayerEvent::EnoughData);
                                })
                                .seek_data(move |_, offset| {
                                    observers___
                                        .lock()
                                        .unwrap()
                                        .notify(PlayerEvent::SeekData(offset));
                                    let _ = servosrc_.set_seek_offset(offset);
                                    true
                                })
                                .build(),
                        );

                        is_ready.call_once(|| {
                            let _ = sender_clone.lock().unwrap().send(Ok(()));
                        });
                        PlayerSource::Seekable(servosrc)
                    }
                    StreamType::Stream => {
                        let media_stream_src = source
                            .clone()
                            .dynamic_cast::<ServoMediaStreamSrc>()
                            .expect("Source element is expected to be a ServoMediaStreamSrc!");
                        let sender_clone = sender.clone();
                        is_ready_clone.call_once(|| {
                            let _ = sender_clone.lock().unwrap().send(Ok(()));
                        });
                        PlayerSource::Stream(media_stream_src)
                    }
                };

                inner.set_src(source);

                None
            });

            if connect_result.is_err() {
                let _ = sender_clone
                    .lock()
                    .unwrap()
                    .send(Err(PlayerError::Backend("Source setup failed".to_owned())));
            }

            let error_handler_id = inner.player.connect_error(move |player, error| {
                let _ = sender_clone
                    .lock()
                    .unwrap()
                    .send(Err(PlayerError::Backend(error.description().to_string())));
                player.stop();
            });

            let _ = inner.pause();

            (receiver, error_handler_id)
        };

        let result = receiver.recv().unwrap();
        glib::signal::signal_handler_disconnect(&inner.lock().unwrap().player, error_handler_id);
        result
    }
}

macro_rules! inner_player_proxy {
    ($fn_name:ident, $return_type:ty) => (
        fn $fn_name(&self) -> Result<$return_type, PlayerError> {
            self.setup()?;
            let inner = self.inner.borrow();
            let mut inner = inner.as_ref().unwrap().lock().unwrap();
            inner.$fn_name()
        }
    );

    ($fn_name:ident, $arg1:ident, $arg1_type:ty) => (
        fn $fn_name(&self, $arg1: $arg1_type) -> Result<(), PlayerError> {
            self.setup()?;
            let inner = self.inner.borrow();
            let mut inner = inner.as_ref().unwrap().lock().unwrap();
            inner.$fn_name($arg1)
        }
    )
}

impl Player for GStreamerPlayer {
    inner_player_proxy!(play, ());
    inner_player_proxy!(pause, ());
    inner_player_proxy!(stop, ());
    inner_player_proxy!(end_of_stream, ());
    inner_player_proxy!(set_input_size, size, u64);
    inner_player_proxy!(set_mute, val, bool);
    inner_player_proxy!(set_rate, rate, f64);
    inner_player_proxy!(push_data, data, Vec<u8>);
    inner_player_proxy!(seek, time, f64);
    inner_player_proxy!(set_volume, value, f64);
    inner_player_proxy!(buffered, Vec<Range<f64>>);
    inner_player_proxy!(set_stream, stream, &MediaStreamId);

    fn register_event_handler(&self, sender: IpcSender<PlayerEvent>) {
        self.observers.lock().unwrap().register(sender);
    }

    fn register_frame_renderer(&self, renderer: Arc<Mutex<FrameRenderer>>) {
        self.renderers.lock().unwrap().register(renderer);
    }

    fn shutdown(&self) -> Result<(), PlayerError> {
        self.observers.lock().unwrap().clear();
        self.renderers.lock().unwrap().clear();
        self.stop()
    }

    fn render_use_gl(&self) -> bool {
        self.render.lock().unwrap().is_gl()
    }

    fn disable_video(&self) -> Result<(), PlayerError> {
        self.setup()?;
        let inner = self.inner.borrow();
        let inner = inner.as_ref().unwrap().lock().unwrap();
        inner.disable_video();
        Ok(())
    }
}
