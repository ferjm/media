/// This module contains the implementation of custom GStreamer source elements.

/// ServoMedia client source element.
/// Allows ServoMedia clients to feed seekable a/v data into the player pipeline.
pub mod client;

/// ServoMedia stream source element.
/// Allows ServoMedia clients to feed stream data into the player pipeline.
pub mod media_stream;
