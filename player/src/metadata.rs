use std::{string, time};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Metadata {
    pub duration: Option<time::Duration>,
    pub width: u32,
    pub height: u32,
    pub format: string::String,
    // TODO: Might be nice to move width and height along with each video track.
    pub video_tracks: Vec<string::String>,
    pub audio_tracks: Vec<string::String>,
}
