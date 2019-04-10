#[macro_use]
extern crate serde_derive;
extern crate servo_media_streams as streams;

pub mod frame;
pub mod metadata;

pub use frame::{Frame, FrameRenderer};
pub use metadata::Metadata;
use std::ops::Range;
use streams::registry::MediaStreamId;

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
    /// Setting an audio or video stream failed.
    /// Possibly because the type of source is not PlayerSource::Stream.
    SetStreamFailed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum PlayerEvent {
    EndOfStream,
    /// The player has enough data. The client should stop pushing data into.
    EnoughData,
    Error,
    FrameUpdated,
    MetadataUpdated(Metadata),
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

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub enum StreamType {
    /// No seeking is supported in the stream, such as a live stream.
    Stream,
    /// The stream is seekable.
    Seekable,
}

pub enum GlContext {
    // The EGL platform used primarily with the X11, Wayland and
    // Android window systems as well as on embedded Linux.
    Egl(usize),
    // The GLX platform used primarily with the X11 window system.
    Glx(usize),
    Unknown,
}

pub trait Player: Send {
    fn play(&self) -> Result<(), PlayerError>;
    fn pause(&self) -> Result<(), PlayerError>;
    fn stop(&self) -> Result<(), PlayerError>;
    fn seek(&self, time: f64) -> Result<(), PlayerError>;
    fn set_mute(&self, val: bool) -> Result<(), PlayerError>;
    fn set_volume(&self, value: f64) -> Result<(), PlayerError>;
    fn set_input_size(&self, size: u64) -> Result<(), PlayerError>;
    fn set_rate(&self, rate: f64) -> Result<(), PlayerError>;
    fn push_data(&self, data: Vec<u8>) -> Result<(), PlayerError>;
    fn end_of_stream(&self) -> Result<(), PlayerError>;
    /// Get the list of time ranges in seconds that have been buffered.
    fn buffered(&self) -> Result<Vec<Range<f64>>, PlayerError>;
    fn set_gl_params(&self, gl_context: GlContext, gl_display: usize) -> Result<(), ()>;
    /// Shut the player down. Stops playback and free up resources.
    fn shutdown(&self) -> Result<(), PlayerError>;
    /// Set the stream to be played by the player.
    /// This method requires the player to be constructed with StreamType::Stream.
    fn set_stream(&self, stream: &MediaStreamId) -> Result<(), PlayerError>;
}
