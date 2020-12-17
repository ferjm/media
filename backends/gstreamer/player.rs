use super::BACKEND_BASE_TIME;
use byte_slice_cast::AsSliceOf;
use glib;
use glib::prelude::*;
use gst;
use gst::prelude::*;
use gst_app;
use gst_player;
use gst_player::prelude::*;
use ipc_channel::ipc::{channel, IpcReceiver, IpcSender};
use media_stream::GStreamerMediaStream;
use render::GStreamerRender;
use servo_media_player::audio::AudioRenderer;
use servo_media_player::context::PlayerGLContext;
use servo_media_player::metadata::Metadata;
use servo_media_player::video::VideoFrameRenderer;
use servo_media_player::{
    PlaybackState, Player, PlayerError, PlayerEvent, SeekLock, SeekLockMsg, StreamType,
};
use servo_media_streams::registry::{get_stream, MediaStreamId};
use servo_media_traits::{BackendMsg, ClientContextId, MediaInstance};
use source::client::{register_servo_media_client_src, ServoMediaClientSrc};
use source::media_stream::{register_servo_media_stream_src, ServoMediaStreamSrc};
use std::cell::RefCell;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
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
    let title = media_info.get_title().map(|s| s.as_str().to_string());

    Ok(Metadata {
        duration,
        width,
        height,
        format,
        is_seekable,
        audio_tracks,
        video_tracks,
        is_live,
        title,
    })
}

pub struct GStreamerAudioChunk(gst::buffer::MappedBuffer<gst::buffer::Readable>);
impl AsRef<[f32]> for GStreamerAudioChunk {
    fn as_ref(&self) -> &[f32] {
        self.0.as_ref().as_slice_of::<f32>().unwrap()
    }
}

#[derive(PartialEq)]
enum PlayerSource {
    Seekable(ServoMediaClientSrc),
    Stream(ServoMediaStreamSrc),
}

struct PlayerInner {
    player: gst_player::Player,
    source: Option<PlayerSource>,
    video_sink: gst_app::AppSink,
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

    fn set_stream(&mut self, stream: &MediaStreamId, only_stream: bool) -> Result<(), PlayerError> {
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

                    source.set_stream(&mut stream, only_stream);
                    return Ok(());
                }
            }
        }
        Err(PlayerError::SetStreamFailed)
    }

    fn set_audio_track(&mut self, stream_index: i32, enabled: bool) -> Result<(), PlayerError> {
        self.player
            .set_audio_track(stream_index)
            .map_err(|_| PlayerError::SetTrackFailed)?;
        self.player.set_audio_track_enabled(enabled);
        Ok(())
    }

    fn set_video_track(&mut self, stream_index: i32, enabled: bool) -> Result<(), PlayerError> {
        self.player
            .set_video_track(stream_index)
            .map_err(|_| PlayerError::SetTrackFailed)?;
        self.player.set_video_track_enabled(enabled);
        Ok(())
    }
}

macro_rules! notify(
    ($observer:expr, $event:expr) => {
        $observer.lock().unwrap().send($event).unwrap();
    };
);

macro_rules! player(
    ($inner:expr) => {
        $inner.lock().unwrap().player
    }
);

struct SeekChannel {
    sender: SeekLock,
    recv: IpcReceiver<SeekLockMsg>,
}

impl SeekChannel {
    fn new() -> Self {
        let (sender, recv) = channel::<SeekLockMsg>().expect("Couldn't create IPC channel");
        Self {
            sender: SeekLock {
                lock_channel: sender,
            },
            recv,
        }
    }

    fn sender(&self) -> SeekLock {
        self.sender.clone()
    }

    fn _await(&self) -> SeekLockMsg {
        self.recv.recv().unwrap()
    }
}

pub struct GStreamerPlayer {
    /// The player unique ID.
    id: usize,
    /// The ID of the client context this player belongs to.
    context_id: ClientContextId,
    /// Channel to communicate with the owner GStreamerBackend instance.
    backend_chan: Arc<Mutex<Sender<BackendMsg>>>,
    inner: RefCell<Option<Arc<Mutex<PlayerInner>>>>,
    observer: Arc<Mutex<IpcSender<PlayerEvent>>>,
    audio_renderer: Option<Arc<Mutex<dyn AudioRenderer>>>,
    video_renderer: Option<Arc<Mutex<dyn VideoFrameRenderer>>>,
    /// Indicates whether the setup was succesfully performed and
    /// we are ready to consume a/v data.
    is_ready: Arc<Once>,
    /// Indicates whether the type of media stream to be played is a live stream.
    stream_type: StreamType,
    /// Decorator used to setup the video sink and process the produced frames.
    render: Arc<Mutex<GStreamerRender>>,
}

impl GStreamerPlayer {
    pub fn new(
        id: usize,
        context_id: &ClientContextId,
        backend_chan: Arc<Mutex<Sender<BackendMsg>>>,
        stream_type: StreamType,
        observer: IpcSender<PlayerEvent>,
        video_renderer: Option<Arc<Mutex<dyn VideoFrameRenderer>>>,
        audio_renderer: Option<Arc<Mutex<dyn AudioRenderer>>>,
        gl_context: Box<dyn PlayerGLContext>,
    ) -> GStreamerPlayer {
        let _ = gst::DebugCategory::new(
            "servoplayer",
            gst::DebugColorFlags::empty(),
            Some("Servo player"),
        );

        Self {
            id,
            context_id: *context_id,
            backend_chan,
            inner: RefCell::new(None),
            observer: Arc::new(Mutex::new(observer)),
            audio_renderer,
            video_renderer,
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

        // FIXME(#282): The progressive downloading breaks playback on Windows and Android.
        if !cfg!(any(target_os = "windows", target_os = "android")) {
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
        }

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

        if let Some(ref audio_renderer) = self.audio_renderer {
            let audio_sink = gst::ElementFactory::make("appsink", None)
                .map_err(|_| PlayerError::Backend("appsink creation failed".to_owned()))?;

            pipeline
                .set_property("audio-sink", &audio_sink)
                .expect("playbin doesn't have expected 'audio-sink' property");

            let audio_sink = audio_sink.dynamic_cast::<gst_app::AppSink>().unwrap();
            let audio_renderer_ = audio_renderer.clone();
            audio_sink.set_callbacks(
                gst_app::AppSinkCallbacks::new()
                    .new_preroll(|_| Ok(gst::FlowSuccess::Ok))
                    .new_sample(move |audio_sink| {
                        let sample = audio_sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        let buffer = sample.get_buffer_owned().ok_or(gst::FlowError::Error)?;
                        let audio_info = sample
                            .get_caps()
                            .and_then(|caps| gst_audio::AudioInfo::from_caps(caps).ok())
                            .ok_or(gst::FlowError::Error)?;
                        let positions = audio_info.positions().ok_or(gst::FlowError::Error)?;
                        for position in positions.iter() {
                            let buffer = buffer.clone();
                            let map = if let Ok(map) = buffer.into_mapped_buffer_readable() {
                                map
                            } else {
                                return Err(gst::FlowError::Error);
                            };
                            let chunk = Box::new(GStreamerAudioChunk(map));
                            let channel = position.to_mask() as u32;
                            audio_renderer_.lock().unwrap().render(chunk, channel);
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );
        }

        let video_sink = self.render.lock().unwrap().setup_video_sink(&pipeline)?;

        // There's a known bug in gstreamer that may cause a wrong transition
        // to the ready state while setting the uri property:
        // https://cgit.freedesktop.org/gstreamer/gst-plugins-bad/commit/?id=afbbc3a97ec391c6a582f3c746965fdc3eb3e1f3
        // This may affect things like setting the config, so until the bug is
        // fixed, make sure that state dependent code happens before this line.
        // The estimated version for the fix is 1.14.5 / 1.15.1.
        // https://github.com/servo/servo/issues/22010#issuecomment-432599657
        let uri = match self.stream_type {
            StreamType::Stream => {
                register_servo_media_stream_src().map_err(|_| {
                    PlayerError::Backend("ServoMediaStreamSrc registration error".to_owned())
                })?;
                "mediastream://".to_value()
            }
            StreamType::Seekable => {
                register_servo_media_client_src().map_err(|_| {
                    PlayerError::Backend("ServoMediaClientSrc registration error".to_owned())
                })?;
                "servosrc://".to_value()
            }
        };
        player
            .set_property("uri", &uri)
            .expect("playbin doesn't have expected 'uri' property");

        // No video_renderers no video
        if self.video_renderer.is_none() {
            player.set_video_track_enabled(false);
        }

        *self.inner.borrow_mut() = Some(Arc::new(Mutex::new(PlayerInner {
            player,
            source: None,
            video_sink,
            input_size: 0,
            rate: 1.0,
            stream_type: self.stream_type,
            last_metadata: None,
            cat: gst::DebugCategory::get("servoplayer").unwrap(),
            enough_data: Arc::new(AtomicBool::new(false)),
        })));

        let inner = self.inner.borrow();
        let inner = inner.as_ref().unwrap();
        let observer = self.observer.clone();
        // Handle `end-of-stream` signal.
        player!(inner).connect_end_of_stream(move |_| {
            notify!(observer, PlayerEvent::EndOfStream);
        });

        let observer = self.observer.clone();
        // Handle `error` signal
        player!(inner).connect_error(move |_, error| {
            notify!(observer, PlayerEvent::Error(error.to_string()));
        });

        let observer = self.observer.clone();
        // Handle `state-changed` signal.
        player!(inner).connect_state_changed(move |_, player_state| {
            let state = match player_state {
                gst_player::PlayerState::Buffering => Some(PlaybackState::Buffering),
                gst_player::PlayerState::Stopped => Some(PlaybackState::Stopped),
                gst_player::PlayerState::Paused => Some(PlaybackState::Paused),
                gst_player::PlayerState::Playing => Some(PlaybackState::Playing),
                _ => None,
            };
            if let Some(v) = state {
                notify!(observer, PlayerEvent::StateChanged(v));
            }
        });

        let observer = self.observer.clone();
        // Handle `position-update` signal.
        player!(inner).connect_position_updated(move |_, position| {
            if let Some(seconds) = position.seconds() {
                notify!(observer, PlayerEvent::PositionChanged(seconds));
            }
        });

        let observer = self.observer.clone();
        // Handle `seek-done` signal.
        player!(inner).connect_seek_done(move |_, position| {
            if let Some(seconds) = position.seconds() {
                notify!(observer, PlayerEvent::SeekDone(seconds));
            }
        });

        // Handle `media-info-updated` signal.
        let inner_clone = inner.clone();
        let observer = self.observer.clone();
        player!(inner).connect_media_info_updated(move |_, info| {
            let mut inner = inner_clone.lock().unwrap();
            if let Ok(metadata) = metadata_from_media_info(info) {
                if inner.last_metadata.as_ref() != Some(&metadata) {
                    inner.last_metadata = Some(metadata.clone());
                    if metadata.is_seekable {
                        inner.player.set_rate(inner.rate);
                    }
                    gst_info!(inner.cat, obj: &inner.player, "Metadata updated: {:?}", metadata);
                    notify!(observer, PlayerEvent::MetadataUpdated(metadata));
                }
            }
        });

        // Handle `duration-changed` signal.
        let inner_clone = inner.clone();
        let observer = self.observer.clone();
        player!(inner).connect_duration_changed(move |_, duration| {
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
                notify!(
                    observer,
                    PlayerEvent::MetadataUpdated(updated_metadata.unwrap())
                );
            }
        });

        self.video_renderer.clone().map(|video_renderer| {
            let render = self.render.clone();
            let observer = self.observer.clone();
            // Set video_sink callbacks.
            inner.lock().unwrap().video_sink.set_callbacks(
                gst_app::AppSinkCallbacks::new()
                    .new_preroll(|_| Ok(gst::FlowSuccess::Ok))
                    .new_sample(move |video_sink| {
                        let sample = video_sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        let frame = render
                            .lock()
                            .unwrap()
                            .get_frame_from_sample(sample)
                            .or_else(|_| Err(gst::FlowError::Error))?;
                        video_renderer.lock().unwrap().render(frame);
                        notify!(observer, PlayerEvent::VideoFrameUpdated);
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );
        });

        let (receiver, error_handler_id) = {
            let inner_clone = inner.clone();
            let mut inner = inner.lock().unwrap();
            let pipeline = inner.player.get_pipeline();

            let (sender, receiver) = mpsc::channel();

            let sender = Arc::new(Mutex::new(sender));
            let sender_clone = sender.clone();
            let is_ready_clone = self.is_ready.clone();
            let observer = self.observer.clone();
            let connect_result = pipeline.connect("source-setup", false, move |args| {
                let source = match args[1].get::<gst::Element>() {
                    Ok(Some(source)) => source,
                    _ => {
                        let _ = notify!(
                            sender,
                            Err(PlayerError::Backend("Source setup failed".to_owned()))
                        );
                        return None;
                    }
                };

                let mut inner = inner_clone.lock().unwrap();
                let source = match inner.stream_type {
                    StreamType::Seekable => {
                        let client_src = source
                            .clone()
                            .dynamic_cast::<ServoMediaClientSrc>()
                            .expect("Source element is expected to be a ServoMediaClientSrc!");

                        if inner.input_size > 0 {
                            client_src.set_size(inner.input_size as i64);
                        }

                        let sender_clone = sender.clone();
                        let is_ready = is_ready_clone.clone();
                        let observer_ = observer.clone();
                        let observer__ = observer.clone();
                        let observer___ = observer.clone();
                        let client_src_ = client_src.clone();
                        let enough_data_ = inner.enough_data.clone();
                        let enough_data__ = inner.enough_data.clone();
                        let seek_channel = Arc::new(Mutex::new(SeekChannel::new()));
                        client_src.set_callbacks(
                            gst_app::AppSrcCallbacks::new()
                                .need_data(move |_, _| {
                                    // We block the caller of the setup method until we get
                                    // the first need-data signal, so we ensure that we
                                    // don't miss any data between the moment the client
                                    // calls setup and the player is actually ready to
                                    // get any data.
                                    is_ready.call_once(|| {
                                        let _ = sender_clone.lock().unwrap().send(Ok(()));
                                    });

                                    enough_data_.store(false, Ordering::Relaxed);
                                    notify!(observer_, PlayerEvent::NeedData);
                                })
                                .enough_data(move |_| {
                                    enough_data__.store(true, Ordering::Relaxed);
                                    notify!(observer__, PlayerEvent::EnoughData);
                                })
                                .seek_data(move |_, offset| {
                                    let (ret, ack_channel) = if client_src_.set_seek_offset(offset)
                                    {
                                        notify!(
                                            observer___,
                                            PlayerEvent::SeekData(
                                                offset,
                                                seek_channel.lock().unwrap().sender()
                                            )
                                        );
                                        let (ret, ack_channel) =
                                            seek_channel.lock().unwrap()._await();
                                        (ret, Some(ack_channel))
                                    } else {
                                        (true, None)
                                    };

                                    client_src_.set_seek_done();
                                    if let Some(ack_channel) = ack_channel {
                                        ack_channel.send(()).unwrap();
                                    }
                                    ret
                                })
                                .build(),
                        );

                        PlayerSource::Seekable(client_src)
                    }
                    StreamType::Stream => {
                        let media_stream_src = source
                            .clone()
                            .dynamic_cast::<ServoMediaStreamSrc>()
                            .expect("Source element is expected to be a ServoMediaStreamSrc!");
                        let sender_clone = sender.clone();
                        is_ready_clone.call_once(|| {
                            let _ = notify!(sender_clone, Ok(()));
                        });
                        PlayerSource::Stream(media_stream_src)
                    }
                };

                inner.set_src(source);

                None
            });

            if connect_result.is_err() {
                let _ = notify!(
                    sender_clone,
                    Err(PlayerError::Backend("Source setup failed".to_owned()))
                );
            }

            let error_handler_id = inner.player.connect_error(move |player, error| {
                let _ = notify!(sender_clone, Err(PlayerError::Backend(error.to_string())));
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
    ($fn_name:ident, $return_type:ty) => {
        fn $fn_name(&self) -> Result<$return_type, PlayerError> {
            self.setup()?;
            let inner = self.inner.borrow();
            let mut inner = inner.as_ref().unwrap().lock().unwrap();
            inner.$fn_name()
        }
    };

    ($fn_name:ident, $arg1:ident, $arg1_type:ty) => {
        fn $fn_name(&self, $arg1: $arg1_type) -> Result<(), PlayerError> {
            self.setup()?;
            let inner = self.inner.borrow();
            let mut inner = inner.as_ref().unwrap().lock().unwrap();
            inner.$fn_name($arg1)
        }
    };
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

    fn render_use_gl(&self) -> bool {
        self.render.lock().unwrap().is_gl()
    }

    fn set_stream(&self, stream: &MediaStreamId, only_stream: bool) -> Result<(), PlayerError> {
        self.setup()?;
        let inner = self.inner.borrow();
        let mut inner = inner.as_ref().unwrap().lock().unwrap();
        inner.set_stream(stream, only_stream)
    }

    fn set_audio_track(&self, stream_index: i32, enabled: bool) -> Result<(), PlayerError> {
        self.setup()?;
        let inner = self.inner.borrow();
        let mut inner = inner.as_ref().unwrap().lock().unwrap();
        inner.set_audio_track(stream_index, enabled)
    }

    fn set_video_track(&self, stream_index: i32, enabled: bool) -> Result<(), PlayerError> {
        self.setup()?;
        let inner = self.inner.borrow();
        let mut inner = inner.as_ref().unwrap().lock().unwrap();
        inner.set_video_track(stream_index, enabled)
    }
}

impl MediaInstance for GStreamerPlayer {
    fn get_id(&self) -> usize {
        self.id
    }

    fn mute(&self, val: bool) -> Result<(), ()> {
        self.set_mute(val).map_err(|_| ())
    }

    fn suspend(&self) -> Result<(), ()> {
        self.pause().map_err(|_| ())
    }

    fn resume(&self) -> Result<(), ()> {
        self.play().map_err(|_| ())
    }
}

impl Drop for GStreamerPlayer {
    fn drop(&mut self) {
        let _ = self.stop();
        let _ = self
            .backend_chan
            .lock()
            .unwrap()
            .send(BackendMsg::Shutdown(self.context_id, self.id));
    }
}
