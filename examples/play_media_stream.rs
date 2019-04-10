extern crate ipc_channel;
extern crate servo_media;
extern crate servo_media_auto;

use ipc_channel::ipc;
use servo_media::player::{Frame, FrameRenderer, PlayerEvent, StreamType};
use servo_media::ServoMedia;
use std::sync::{Arc, Mutex};

struct DummyRenderer {}
impl FrameRenderer for DummyRenderer {
    fn render(&mut self, _: Frame) {}
}

fn run_example(servo_media: Arc<ServoMedia>) {
    let (sender, receiver) = ipc::channel().unwrap();
    let renderer = Arc::new(Mutex::new(DummyRenderer {}));
    let player = Arc::new(Mutex::new(servo_media.create_player(
        StreamType::Stream,
        sender,
        renderer,
    )));

    let audio_stream = servo_media.create_audiostream();
    player.lock().unwrap().set_stream(&audio_stream).unwrap();

    player.lock().unwrap().play().unwrap();

    while let Ok(event) = receiver.recv() {
        match event {
            PlayerEvent::EndOfStream => {
                println!("\nEOF");
                break;
            }
            PlayerEvent::Error => {
                println!("\nError");
                break;
            }
            PlayerEvent::MetadataUpdated(ref m) => {
                println!("\nMetadata updated! {:?}", m);
            }
            PlayerEvent::StateChanged(ref s) => {
                println!("\nPlayer state changed to {:?}", s);
            }
            PlayerEvent::FrameUpdated => eprint!("."),
            PlayerEvent::PositionChanged(p) => {
                if p == 4 {
                    break;
                }
                println!("Position changed {:?}", p)
            }
            PlayerEvent::SeekData(_) => {
                println!("\nERROR: Should not receive SeekData for streams")
            }
            PlayerEvent::SeekDone(_) => {
                println!("\nERROR: Should not receive SeekDone for streams")
            }
            PlayerEvent::NeedData => println!("\nERROR: Should not receive NeedData for streams"),
            PlayerEvent::EnoughData => {
                println!("\nERROR: Should not receive EnoughData for streams")
            }
        }
    }
    player.lock().unwrap().shutdown().unwrap();
}

fn main() {
    ServoMedia::init::<servo_media_auto::Backend>();
    if let Ok(servo_media) = ServoMedia::get() {
        run_example(servo_media);
    }
}
