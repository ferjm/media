#![feature(fnbox)]

#[macro_use]
extern crate servo_media_derive;

extern crate byte_slice_cast;
extern crate num_traits;
extern crate petgraph;
extern crate smallvec;
#[macro_use]
pub mod macros;

pub mod block;
pub mod buffer_source_node;
pub mod channel_node;
pub mod context;
pub mod decoder;
pub mod destination_node;
pub mod gain_node;
pub mod graph;
pub mod node;
pub mod oscillator_node;
pub mod param;
pub mod render_thread;
pub mod sink;

pub trait AudioBackend {
    type Decoder: decoder::AudioDecoder;
    type Sink: sink::AudioSink;
    fn make_decoder() -> Self::Decoder;
    fn make_sink() -> Result<Self::Sink, ()>;
    fn init();
}

pub struct DummyBackend {}

impl AudioBackend for DummyBackend {
    type Decoder = decoder::DummyAudioDecoder;
    type Sink = sink::DummyAudioSink;
    fn make_decoder() -> Self::Decoder {
        decoder::DummyAudioDecoder
    }

    fn make_sink() -> Result<Self::Sink, ()> {
        Ok(sink::DummyAudioSink)
    }
    fn init() {}
}
