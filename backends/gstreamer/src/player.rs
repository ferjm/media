use glib::*;
use gst;
use gst_app;
use gst_player;
use gst_player::PlayerStreamInfoExt;
use ipc_channel::ipc;
use servo_media_player::frame::{Frame, FrameRenderer};
use servo_media_player::metadata::Metadata;
use servo_media_player::{Player, PlaybackState, PlayerEvent};
use std::string;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time;
use std::u64;

struct GStreamerFrame {}

impl GStreamerFrame {
    fn new(sample: &gst::Sample) -> Frame {
        let caps = sample.get_caps().unwrap();
        let s = caps.get_structure(0).unwrap();
        let width = s.get("width").unwrap();
        let height = s.get("height").unwrap();
        let buffer = sample.get_buffer().unwrap();

        let map = buffer.map_readable().unwrap();
        let data = Vec::from(map.as_slice());

        Frame::new(width, height, Arc::new(data))
    }
}

struct GStreamerMetadata {}

impl GStreamerMetadata {
    fn new(media_info: &gst_player::PlayerMediaInfo) -> Metadata {
        let dur = media_info.get_duration();
        let duration = if dur != gst::ClockTime::none() {
            let nanos = dur.nanoseconds().unwrap() % 1_000_000_000;

            Some(time::Duration::new(dur.seconds().unwrap(), nanos as u32))
        } else {
            None
        };

        let mut format = string::String::from("");
        let mut audio_tracks = Vec::new();
        let mut video_tracks = Vec::new();
        if let Some(f) = media_info.get_container_format() {
            format = f;
        }

        for stream_info in media_info.get_stream_list() {
            let stream_type = stream_info.get_stream_type();
            match stream_type.as_str() {
                "audio" => {
                    audio_tracks.push(stream_info.get_codec().unwrap());
                }
                "video" => {
                    video_tracks.push(stream_info.get_codec().unwrap());
                }
                _ => {}
            }
        }

        let mut width = 0;
        let height = if media_info.get_number_of_video_streams() > 0 {
            let first_video_stream = &media_info.get_video_streams()[0];
            width = first_video_stream.get_width();
            first_video_stream.get_height()
        } else {
            0
        };

        Metadata {
            duration: duration,
            width: width as u32,
            height: height as u32,
            format: format,
            audio_tracks: audio_tracks,
            video_tracks: video_tracks,
        }
    }
}

struct PlayerInner {
    player: gst_player::Player,
    appsrc: Option<gst_app::AppSrc>,
    appsink: gst_app::AppSink,
    input_size: u64,
    subscribers: Vec<ipc::IpcSender<PlayerEvent>>,
    renderers: Vec<Box<FrameRenderer>>,
    last_metadata: Option<Metadata>,
}

impl PlayerInner {
    pub fn register_event_handler(&mut self, sender: ipc::IpcSender<PlayerEvent>) {
        self.subscribers.push(sender);
    }

    pub fn register_frame_renderer(&mut self, renderer: Box<FrameRenderer>) {
        self.renderers.push(renderer);
    }

    pub fn notify(&self, event: PlayerEvent) {
        for sender in &self.subscribers {
            sender.send(event.clone()).unwrap();
        }
    }

    pub fn render(&self, sample: &gst::Sample) {
        let frame = GStreamerFrame::new(&sample);

        for renderer in &self.renderers {
            renderer.render(frame.clone());
        }
        self.notify(PlayerEvent::FrameUpdated);
    }

    pub fn set_input_size(&mut self, size: u64) {
        self.input_size = size;
    }

    pub fn play(&mut self) {
        self.player.play();
    }

    pub fn stop(&mut self) {
        self.player.stop();
        self.last_metadata = None;
        self.appsrc = None;
    }

    pub fn start(&mut self) {
        self.player.pause();
    }

    pub fn set_app_src(&mut self, appsrc: gst_app::AppSrc) {
        self.appsrc = Some(appsrc);
    }
}

#[derive(Clone)]
pub struct GStreamerPlayer {
    inner: Arc<Mutex<PlayerInner>>,
}

impl GStreamerPlayer {
    pub fn new() -> GStreamerPlayer {
        let player = gst_player::Player::new(None, None);
        player
            .set_property("uri", &Value::from("appsrc://"))
            .expect("Can't set uri property");

        // Disable periodic positon updates for now.
        let mut config = player.get_config();
        config.set_position_update_interval(0u32);
        player.set_config(config).unwrap();

        let video_sink = gst::ElementFactory::make("appsink", None).unwrap();
        let pipeline = player.get_pipeline();
        pipeline
            .set_property("video-sink", &video_sink.to_value())
            .unwrap();
        let video_sink = video_sink.dynamic_cast::<gst_app::AppSink>().unwrap();
        video_sink.set_caps(&gst::Caps::new_simple(
            "video/x-raw",
            &[
                ("format", &"BGRA"),
                ("pixel-aspect-ratio", &gst::Fraction::from((1, 1))),
            ],
        ));

        GStreamerPlayer {
            inner: Arc::new(Mutex::new(PlayerInner {
                player: player,
                appsrc: None,
                appsink: video_sink,
                input_size: 0,
                subscribers: Vec::new(),
                renderers: Vec::new(),
                last_metadata: None,
            })),
        }
    }
}

impl Player for GStreamerPlayer {
    fn register_event_handler(&self, sender: ipc::IpcSender<PlayerEvent>) {
        self.inner.lock().unwrap().register_event_handler(sender);
    }

    fn register_frame_renderer(&self, renderer: Box<FrameRenderer>) {
        self.inner
            .lock()
            .unwrap()
            .register_frame_renderer(renderer);
    }

    fn set_input_size(&self, size: u64) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.set_input_size(size);
        }
    }

    fn setup(&self) -> bool {
        let inner_clone = self.inner.clone();
        self.inner
            .lock()
            .unwrap()
            .player
            .connect_end_of_stream(move |_| {
                let inner = &inner_clone;
                let guard = inner.lock().unwrap();

                guard.notify(PlayerEvent::EndOfStream);
            });

        let inner_clone = self.inner.clone();
        self.inner
            .lock()
            .unwrap()
            .player
            .connect_error(move |_, _| {
                let inner = &inner_clone;
                let guard = inner.lock().unwrap();

                guard.notify(PlayerEvent::Error);
            });

        let inner_clone = self.inner.clone();
        self.inner
            .lock()
            .unwrap()
            .player
            .connect_state_changed(move |_, player_state| {
                let state = match player_state {
                    gst_player::PlayerState::Stopped => Some(PlaybackState::Stopped),
                    gst_player::PlayerState::Paused => Some(PlaybackState::Paused),
                    gst_player::PlayerState::Playing => Some(PlaybackState::Playing),
                    _ => None,
                };
                if let Some(v) = state {
                    let inner = &inner_clone;
                    let guard = inner.lock().unwrap();

                    guard.notify(PlayerEvent::StateChanged(v));
                }
            });

        let inner_clone = self.inner.clone();
        self.inner
            .lock()
            .unwrap()
            .player
            .connect_media_info_updated(move |_, info| {
                let inner = &inner_clone;
                let mut guard = inner.lock().unwrap();

                let metadata = GStreamerMetadata::new(info);
                if guard.last_metadata.as_ref() != Some(&metadata) {
                    guard.last_metadata = Some(metadata.clone());
                    guard.notify(PlayerEvent::MetadataUpdated(metadata));
                }
            });

        self.inner
            .lock()
            .unwrap()
            .player
            .connect_duration_changed(move |_, duration| {
                let mut seconds = duration / 1_000_000_000;
                let mut minutes = seconds / 60;
                let hours = minutes / 60;

                seconds %= 60;
                minutes %= 60;

                // for developing purposes
                println!(
                    "Duration changed to: {:02}:{:02}:{:02}",
                    hours, minutes, seconds
                );
            });

        let inner_clone = self.inner.clone();
        self.inner.lock().unwrap().appsink.set_callbacks(
            gst_app::AppSinkCallbacks::new()
                .new_preroll(|_| gst::FlowReturn::Ok)
                .new_sample(move |appsink| {
                    let sample = match appsink.pull_sample() {
                        None => return gst::FlowReturn::Eos,
                        Some(sample) => sample,
                    };

                    inner_clone.lock().unwrap().render(&sample);

                    gst::FlowReturn::Ok
                })
                .build(),
        );

        let inner_clone = self.inner.clone();
        let (receiver, error_id) = {
            let mut inner = self.inner.lock().unwrap();
            let pipeline = inner.player.get_pipeline();

            let (sender, receiver) = mpsc::channel();

            let sender = Arc::new(Mutex::new(sender));
            let sender_clone = sender.clone();
            pipeline
                .connect("source-setup", false, move |args| {
                    let mut inner = inner_clone.lock().unwrap();

                    if let Some(source) = args[1].get::<gst::Element>() {
                        let appsrc = source
                            .clone()
                            .dynamic_cast::<gst_app::AppSrc>()
                            .expect("Source element is expected to be an appsrc!");

                        appsrc.set_property_format(gst::Format::Bytes);
                        // appsrc.set_property_block(true);
                        if inner.input_size > 0 {
                            appsrc.set_size(inner.input_size as i64);
                        }

                        let sender_clone = sender.clone();

                        let need_data_id = Arc::new(Mutex::new(None));
                        let need_data_id_clone = need_data_id.clone();
                        *need_data_id.lock().unwrap() = Some(
                            appsrc
                                .connect("need-data", false, move |args| {
                                    let _ = sender_clone.lock().unwrap().send(Ok(()));
                                    if let Some(id) = need_data_id_clone.lock().unwrap().take() {
                                        glib::signal::signal_handler_disconnect(
                                            &args[0].get::<gst::Element>().unwrap(),
                                            id,
                                        );
                                    }
                                    None
                                })
                                .unwrap(),
                        );

                        inner.set_app_src(appsrc);
                    } else {
                        let _ = sender.lock().unwrap().send(Err(()));
                    }

                    None
                })
                .unwrap();

            let error_id = inner.player.connect_error(move |_, _| {
                let _ = sender_clone.lock().unwrap().send(Err(()));
            });

            inner.start();

            (receiver, error_id)
        };

        let res = match receiver.recv().unwrap() {
            Ok(_) => true,
            Err(_) => false,
        };

        glib::signal::signal_handler_disconnect(&self.inner.lock().unwrap().player, error_id);

        res
    }

    fn play(&self) {
        self.inner.lock().unwrap().play();
    }

    fn stop(&self) {
        self.inner.lock().unwrap().stop();
    }

    fn push_data(&self, data: Vec<u8>) -> bool {
        if let Some(ref mut appsrc) = self.inner.lock().unwrap().appsrc {
            let buffer = gst::Buffer::from_slice(data).expect("Unable to create a buffer");
            return appsrc.push_buffer(buffer) == gst::FlowReturn::Ok;
        }
        return false;
    }

    fn end_of_stream(&self) -> bool {
        if let Some(ref mut appsrc) = self.inner.lock().unwrap().appsrc {
            return appsrc.end_of_stream() == gst::FlowReturn::Ok;
        }
        return false;
    }
}
