#![feature(extern_prelude)]

extern crate byte_slice_cast;
extern crate glib;
extern crate gstreamer as gst;
extern crate gstreamer_app as gst_app;
extern crate gstreamer_audio as gst_audio;
extern crate gstreamer_player as gst_player;
extern crate ipc_channel;
extern crate num_traits;
extern crate servo_media_audio;
extern crate servo_media_player;

use servo_media_audio::AudioBackend;
use servo_media_player::PlayerBackend;

pub mod audio_decoder;
pub mod audio_sink;
pub mod player;

pub struct GStreamerBackend;

impl AudioBackend for GStreamerBackend {
    type Decoder = audio_decoder::GStreamerAudioDecoder;
    type Sink = audio_sink::GStreamerAudioSink;
    fn make_decoder() -> Self::Decoder {
        audio_decoder::GStreamerAudioDecoder::new()
    }
    fn make_sink() -> Result<Self::Sink, ()> {
        audio_sink::GStreamerAudioSink::new()
    }
    fn init() {
        gst::init().unwrap();
    }
}

impl PlayerBackend for GStreamerBackend {
    type Player = player::GStreamerPlayer;
    fn make_player() -> Self::Player {
        player::GStreamerPlayer::new()
    }
}
