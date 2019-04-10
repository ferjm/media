use super::BACKEND_BASE_TIME;
use glib;
use glib::prelude::*;
use gst;
use gst::prelude::*;
use gst_app;
use gst_gl;
use gst_gl::prelude::*;
use gst_player;
use gst_player::prelude::*;
use gst_video;
use ipc_channel::ipc::IpcSender;
use media_stream::GStreamerMediaStream;
use media_stream_source::{register_servo_media_stream_src, ServoMediaStreamSrc};
use servo_media_player::frame::{Buffer, Frame, FrameData, FrameRenderer};
use servo_media_player::metadata::Metadata;
use servo_media_player::{GlContext, PlaybackState, Player, PlayerError, PlayerEvent, StreamType};
use servo_media_streams::registry::{get_stream, MediaStreamId};
use source::{register_servo_src, ServoSrc};
use std::cell::RefCell;
use std::error::Error;
use std::ops::Range;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, Once};
use std::time;
use std::u64;

const MAX_BUFFER_SIZE: i32 = 500 * 1024 * 1024;

struct GStreamerBuffer {
    is_gl: bool,
    frame: gst_video::VideoFrame<gst_video::video_frame::Readable>,
}

impl Buffer for GStreamerBuffer {
    fn to_vec(&self) -> Result<FrameData, ()> {
        // packed formats are guaranteed to be in a single plane
        if self.is_gl && self.frame.format() == gst_video::VideoFormat::Rgba {
            let texid = self.frame.get_texture_id(0).ok_or_else(|| ())?;
            Ok(FrameData::Texture(texid))
        } else if !self.is_gl && self.frame.format() == gst_video::VideoFormat::Bgra {
            let data = self.frame.plane_data(0).ok_or_else(|| ())?;
            Ok(FrameData::Raw(Arc::new(data.to_vec())))
        } else {
            Err(())
        }
    }
}

fn frame_from_sample(sample: &gst::Sample) -> Result<Frame, ()> {
    let buffer = sample.get_buffer().ok_or_else(|| ())?;
    let caps = sample.get_caps().ok_or_else(|| ())?;
    let info = gst_video::VideoInfo::from_caps(caps.as_ref()).ok_or_else(|| ())?;
    let is_gl = caps
        .get_features(0)
        .and_then(|features| Some(features.contains(&gst_gl::CAPS_FEATURE_MEMORY_GL_MEMORY)))
        .ok_or_else(|| ())?;
    let frame = if !is_gl {
        gst_video::VideoFrame::from_buffer_readable(buffer, &info).or_else(|_| Err(()))?
    } else {
        gst_video::VideoFrame::from_buffer_readable_gl(buffer, &info).or_else(|_| Err(()))?
    };
    let buffer = GStreamerBuffer { is_gl, frame };

    Frame::new(info.width() as i32, info.height() as i32, Arc::new(buffer))
}

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
                if source.get_current_level_bytes() + data.len() as u64 > source.get_max_bytes() {
                    return Err(PlayerError::EnoughData);
                }
                let buffer = gst::Buffer::from_slice(data);
                return source
                    .push_buffer(buffer)
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
}

pub struct GStreamerPlayer {
    inner: RefCell<Option<Arc<Mutex<PlayerInner>>>>,
    observer: Arc<Mutex<IpcSender<PlayerEvent>>>,
    renderer: Arc<Mutex<FrameRenderer>>,
    /// Indicates whether the setup was succesfully performed and
    /// we are ready to consume a/v data.
    is_ready: Arc<Once>,
    gl_context: RefCell<Option<gst_gl::GLContext>>,
    gl_display: RefCell<Option<gst_gl::GLDisplay>>,
    /// Indicates whether the type of media stream to be played is a live stream.
    stream_type: StreamType,
}

impl GStreamerPlayer {
    pub fn new(
        stream_type: StreamType,
        event_handler: IpcSender<PlayerEvent>,
        renderer: Arc<Mutex<FrameRenderer>>,
    ) -> GStreamerPlayer {
        Self {
            inner: RefCell::new(None),
            observer: Arc::new(Mutex::new(event_handler)),
            renderer,
            is_ready: Arc::new(Once::new()),
            gl_context: RefCell::new(None),
            gl_display: RefCell::new(None),
            stream_type,
        }
    }

    fn set_bus_sync_handler(&self, pipeline: &gst::Element) -> Result<(), PlayerError> {
        let bus = pipeline.get_bus().expect("pipeline without bus");

        let gl_context = self.gl_context.replace(None).unwrap();
        let gl_display = self.gl_display.replace(None).unwrap();

        bus.set_sync_handler(move |_, msg| {
            match msg.view() {
                gst::MessageView::NeedContext(ctxt) => {
                    if let Some(el) = msg.get_src().map(|s| s.downcast::<gst::Element>().unwrap()) {
                        let context_type = ctxt.get_context_type();
                        if context_type == *gst_gl::GL_DISPLAY_CONTEXT_TYPE {
                            let context = gst::Context::new(context_type, true);
                            context.set_gl_display(&gl_display);
                            el.set_context(&context);
                        } else if context_type == "gst.gl.app_context" {
                            let mut context = gst::Context::new(context_type, true);
                            let s = context.get_mut().unwrap().get_mut_structure();
                            s.set_value("context", gl_context.to_send_value());
                            el.set_context(&context);
                        }
                    }
                }
                _ => (),
            }

            gst::BusSyncReply::Pass
        });

        Ok(())
    }

    fn setup_video_sink(
        &self,
        player: &gst_player::Player,
    ) -> Result<gst_app::AppSink, PlayerError> {
        let pipeline = player.get_pipeline();

        let appsink = gst::ElementFactory::make("appsink", None)
            .ok_or(PlayerError::Backend("appsink creation failed".to_owned()))?
            .dynamic_cast::<gst_app::AppSink>()
            .unwrap();

        if self.gl_context.borrow().is_some() {
            let glsink = gst::ElementFactory::make("glsinkbin", None)
                .ok_or(PlayerError::Backend("glsinkbin creation failed".to_owned()))?;

            let caps = gst::Caps::builder("video/x-raw")
                .features(&[&gst_gl::CAPS_FEATURE_MEMORY_GL_MEMORY])
                .field("format", &gst_video::VideoFormat::Rgba.to_string())
                .field("texture-target", &"2D")
                .build();
            appsink.set_caps(&caps);

            glsink
                .set_property("sink", &appsink)
                .expect("glsinkbin doesn't have expected 'sink' property");

            pipeline
                .set_property("video-sink", &glsink)
                .expect("playbin doesn't have expected 'video-sink' property");

            self.set_bus_sync_handler(&pipeline)?;
        } else {
            let caps = gst::Caps::builder("video/x-raw")
                .field("format", &gst_video::VideoFormat::Bgra.to_string())
                .field("pixel-aspect-ratio", &gst::Fraction::from((1, 1)))
                .build();
            appsink.set_caps(&caps);

            pipeline
                .set_property("video-sink", &appsink)
                .expect("playbin doesn't have expected 'video-sink' property");
        };

        Ok(appsink)
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

        let video_sink = self.setup_video_sink(&player)?;

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
            appsink: video_sink,
            input_size: 0,
            rate: 1.0,
            stream_type: self.stream_type,
            last_metadata: None,
            cat: gst::DebugCategory::new(
                "servoplayer",
                gst::DebugColorFlags::empty(),
                "Servo player",
            ),
        })));

        let inner = self.inner.borrow();
        let inner = inner.as_ref().unwrap();
        let observer = self.observer.clone();
        // Handle `end-of-stream` signal.
        inner
            .lock()
            .unwrap()
            .player
            .connect_end_of_stream(move |_| {
                observer
                    .lock()
                    .unwrap()
                    .send(PlayerEvent::EndOfStream)
                    .unwrap();
            });

        let observer = self.observer.clone();
        // Handle `error` signal
        inner.lock().unwrap().player.connect_error(move |_, _| {
            observer.lock().unwrap().send(PlayerEvent::Error).unwrap();
        });

        let observer = self.observer.clone();
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
                    observer
                        .lock()
                        .unwrap()
                        .send(PlayerEvent::StateChanged(v))
                        .unwrap();
                }
            });

        let observer = self.observer.clone();
        // Handle `position-update` signal.
        inner
            .lock()
            .unwrap()
            .player
            .connect_position_updated(move |_, position| {
                if let Some(seconds) = position.seconds() {
                    observer
                        .lock()
                        .unwrap()
                        .send(PlayerEvent::PositionChanged(seconds))
                        .unwrap();
                }
            });

        let observer = self.observer.clone();
        // Handle `seek-done` signal.
        inner
            .lock()
            .unwrap()
            .player
            .connect_seek_done(move |_, position| {
                if let Some(seconds) = position.seconds() {
                    observer
                        .lock()
                        .unwrap()
                        .send(PlayerEvent::SeekDone(seconds))
                        .unwrap();
                }
            });

        // Handle `media-info-updated` signal.
        let inner_clone = inner.clone();
        let observer = self.observer.clone();
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
                        observer
                            .lock()
                            .unwrap()
                            .send(PlayerEvent::MetadataUpdated(metadata))
                            .unwrap();
                    }
                }
            });

        // Handle `duration-changed` signal.
        let inner_clone = inner.clone();
        let observer = self.observer.clone();
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
                    observer
                        .lock()
                        .unwrap()
                        .send(PlayerEvent::MetadataUpdated(updated_metadata.unwrap()))
                        .unwrap();
                }
            });

        let observer = self.observer.clone();
        let renderer = self.renderer.clone();
        // Set appsink callbacks.
        inner.lock().unwrap().appsink.set_callbacks(
            gst_app::AppSinkCallbacks::new()
                .new_preroll(|_| Ok(gst::FlowSuccess::Ok))
                .new_sample(move |appsink| {
                    let sample = appsink.pull_sample().ok_or(gst::FlowError::Eos)?;
                    let frame = frame_from_sample(&sample).map_err(|_| gst::FlowError::Error)?;
                    renderer.lock().unwrap().render(frame);
                    observer
                        .lock()
                        .unwrap()
                        .send(PlayerEvent::FrameUpdated)
                        .unwrap();
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
            let observer = self.observer.clone();
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
                        let is_ready_ = is_ready_clone.clone();
                        let observer_ = observer.clone();
                        let observer__ = observer.clone();
                        let observer___ = observer.clone();
                        servosrc.set_callbacks(
                            gst_app::AppSrcCallbacks::new()
                                .need_data(move |_, _| {
                                    // We block the caller of the setup method until we get
                                    // the first need-data signal, so we ensure that we
                                    // don't miss any data between the moment the client
                                    // calls setup and the player is actually ready to
                                    // get any data.
                                    is_ready_.call_once(|| {
                                        let _ = sender_clone.lock().unwrap().send(Ok(()));
                                    });
                                    observer_
                                        .lock()
                                        .unwrap()
                                        .send(PlayerEvent::NeedData)
                                        .unwrap();
                                })
                                .enough_data(move |_| {
                                    observer__
                                        .lock()
                                        .unwrap()
                                        .send(PlayerEvent::EnoughData)
                                        .unwrap();
                                })
                                .seek_data(move |_, offset| {
                                    observer___
                                        .lock()
                                        .unwrap()
                                        .send(PlayerEvent::SeekData(offset))
                                        .unwrap();
                                    true
                                })
                                .build(),
                        );

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

    fn set_gl_params(&self, _: GlContext, _: usize) -> Result<(), ()> {
        // XXX All GL functionality is temporarily disabled because of
        // https://github.com/servo/servo/pull/22944#issuecomment-468827665
        Err(())
    }

    fn shutdown(&self) -> Result<(), PlayerError> {
        self.stop()
    }
}
