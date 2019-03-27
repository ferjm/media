extern crate servo_media;
extern crate servo_media_auto;

use servo_media::ServoMedia;
use servo_media::streams::MediaStreamCallbacks;
use std::sync::{Arc, mpsc};

fn run_example(servo_media: Arc<ServoMedia>) {
    let (sender, receiver) = mpsc::channel();
    let callbacks = MediaStreamCallbacks::new()
        .eos(move || {
            sender.send(()).unwrap();
        })
        .error(|| {
            println!("Oh, bummer");
        })
        .progress(move |buffer| {
            println!("Got buffer of length {:?}", (*(*buffer).as_ref()).len());
        })
        .build();
    servo_media.play_auto_media_stream(callbacks);
    println!("Playing auto stream");
    receiver.recv().unwrap();
    println!("Done");
}

fn main() {
    ServoMedia::init::<servo_media_auto::Backend>();
    if let Ok(servo_media) = ServoMedia::get() {
        run_example(servo_media);
    } else {
        unreachable!();
    }
}
