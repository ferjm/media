extern crate ipc_channel;
#[macro_use]
extern crate serde_derive;

pub mod frame;
pub mod metadata;

use ipc_channel::ipc::IpcSender;
use std::ops::Range;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum PlaybackState {
    Stopped,
    Buffering,
    Paused,
    Playing,
}

#[derive(Debug, PartialEq)]
pub enum PlayerError {
    /// Backend specific error.
    Backend(String),
    /// Could not push buffer contents to the player.
    BufferPushFailed,
    /// The player cannot consume more data.
    EnoughData,
    /// Setting End Of Stream failed.
    EOSFailed,
    /// The media stream is not seekable.
    NonSeekableStream,
    /// Tried to seek out of range.
    SeekOutOfRange,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum PlayerEvent {
    EndOfStream,
    /// The player has enough data. The client should stop pushing data into.
    EnoughData,
    Error,
    FrameUpdated,
    MetadataUpdated(metadata::Metadata),
    /// The internal player queue is running out of data. The client should start
    /// pushing more data.
    NeedData,
    PositionChanged(u64),
    /// The player needs the data to perform a seek to the given offset.
    /// The next push_data should get the buffers from the new offset.
    /// This event is only received for seekable stream types.
    SeekData(u64),
    /// The player has performed a seek to the given offset.
    SeekDone(u64),
    StateChanged(PlaybackState),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum StreamType {
    /// No seeking is supported in the stream, such as a live stream.
    Stream,
    /// The stream is seekable but seeking might not be very fast, such as data from a webserver.
    Seekable,
    /// The stream is seekable and seeking is fast, such as in a local file.
    RandomAccess,
}

pub enum GlContext {
    // the EGL platform used primarily with the X11, wayland and
    // android window systems as well as on embedded Linux
    Egl(usize),
    // the GLX platform used primarily with the X11 window system
    Glx(usize),
    Unknown,
}

pub trait Player: Send {
    fn register_event_handler(&self, sender: IpcSender<PlayerEvent>);
    fn register_frame_renderer(&self, renderer: Arc<Mutex<frame::FrameRenderer>>);
    fn play(&self) -> Result<(), PlayerError>;
    fn pause(&self) -> Result<(), PlayerError>;
    fn stop(&self) -> Result<(), PlayerError>;
    fn seek(&self, time: f64) -> Result<(), PlayerError>;
    fn set_mute(&self, val: bool) -> Result<(), PlayerError>;
    fn set_volume(&self, value: f64) -> Result<(), PlayerError>;
    fn set_input_size(&self, size: u64) -> Result<(), PlayerError>;
    fn set_rate(&self, rate: f64) -> Result<(), PlayerError>;
    fn set_stream_type(&self, type_: StreamType) -> Result<(), PlayerError>;
    fn push_data(&self, data: Vec<u8>) -> Result<(), PlayerError>;
    fn end_of_stream(&self) -> Result<(), PlayerError>;
    /// Get the list of time ranges in seconds that have been buffered.
    fn buffered(&self) -> Result<Vec<Range<f64>>, PlayerError>;
    fn set_gl_params(&self, gl_context: GlContext, gl_display: usize);
}

pub struct DummyPlayer {}

impl Player for DummyPlayer {
    fn register_event_handler(&self, _: IpcSender<PlayerEvent>) {}
    fn register_frame_renderer(&self, _: Arc<Mutex<frame::FrameRenderer>>) {}

    fn play(&self) -> Result<(), PlayerError> {
        Ok(())
    }
    fn pause(&self) -> Result<(), PlayerError> {
        Ok(())
    }
    fn stop(&self) -> Result<(), PlayerError> {
        Ok(())
    }
    fn seek(&self, _: f64) -> Result<(), PlayerError> {
        Ok(())
    }

    fn set_mute(&self, _: bool) -> Result<(), PlayerError> {
        Ok(())
    }

    fn set_volume(&self, _: f64) -> Result<(), PlayerError> {
        Ok(())
    }

    fn set_input_size(&self, _: u64) -> Result<(), PlayerError> {
        Ok(())
    }
    fn set_rate(&self, _: f64) -> Result<(), PlayerError> {
        Ok(())
    }
    fn set_stream_type(&self, _: StreamType) -> Result<(), PlayerError> {
        Ok(())
    }
    fn push_data(&self, _: Vec<u8>) -> Result<(), PlayerError> {
        Ok(())
    }
    fn end_of_stream(&self) -> Result<(), PlayerError> {
        Ok(())
    }
    fn buffered(&self) -> Result<Vec<Range<f64>>, PlayerError> {
        Ok(vec![])
    }
    fn set_gl_params(&self, _: GlContext, _: usize) {}
}

pub trait PlayerBackend {
    type Player: Player;
    fn make_player() -> Self::Player;
}
