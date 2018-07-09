extern crate ipc_channel;
#[macro_use]
extern crate serde_derive;

pub mod frame;
pub mod metadata;

use ipc_channel::ipc;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum PlaybackState {
    Stopped,
    // Buffering,
    Paused,
    Playing,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum PlayerEvent {
    EndOfStream,
    MetadataUpdated(metadata::Metadata),
    StateChanged(PlaybackState),
    FrameUpdated,
    Error,
}

pub trait Player {
    fn register_event_handler(&self, sender: ipc::IpcSender<PlayerEvent>);
    fn register_frame_renderer(&self, renderer: Box<frame::FrameRenderer>);

    fn setup(&self) -> bool;
    fn play(&self);
    fn stop(&self);

    fn set_input_size(&self, size: u64);
    fn push_data(&self, data: Vec<u8>) -> bool;
    fn end_of_stream(&self) -> bool;
}

pub struct DummyPlayer {}

impl Player for DummyPlayer {
    fn register_event_handler(&self, _: ipc::IpcSender<PlayerEvent>) {}
    fn register_frame_renderer(&self, _: Box<frame::FrameRenderer>) {}

    fn setup(&self) -> bool {
        false
    }
    fn play(&self) {}
    fn stop(&self) {}

    fn set_input_size(&self, _: u64) {}
    fn push_data(&self, _: Vec<u8>) -> bool {
        false
    }
    fn end_of_stream(&self) -> bool {
        false
    }
}

pub trait PlayerBackend {
    type Player: Player;
    fn make_player() -> Self::Player;
}
