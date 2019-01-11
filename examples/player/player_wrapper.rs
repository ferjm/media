// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use glutin::os::{unix::RawHandle, GlContextExt};
use ipc_channel::ipc;
use servo_media::player::frame::{Frame, FrameRenderer};
use servo_media::player::{GlContext, Player, PlayerEvent};
use servo_media::ServoMedia;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::Builder;

pub struct PlayerWrapper {
    player: Arc<Mutex<Box<Player>>>,
    shutdown: Arc<AtomicBool>,
}

impl PlayerWrapper {
    pub fn new(path: &Path, context: &glutin::Context) -> Self {
        let servo_media = ServoMedia::get().unwrap();
        let player = Arc::new(Mutex::new(servo_media.create_player()));

        let file = File::open(&path).unwrap();
        let metadata = file.metadata().unwrap();

        match unsafe { context.raw_handle() } {
            RawHandle::Egl(egl_context) => {
                let gl_context = GlContext::Egl(egl_context as usize);
                if let Some(gl_display) = unsafe { context.get_egl_display() } {
                    player
                        .lock()
                        .unwrap()
                        .set_gl_params(gl_context, gl_display as usize);
                }
            }
            RawHandle::Glx(_) => {}
        };

        player
            .lock()
            .unwrap()
            .set_input_size(metadata.len())
            .unwrap();

        let (sender, receiver) = ipc::channel().unwrap();
        player.lock().unwrap().register_event_handler(sender);
        let player_ = player.clone();
        let player__ = player.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_ = shutdown.clone();
        let shutdown__ = shutdown.clone();
        Builder::new()
            .name("File reader".to_owned())
            .spawn(move || {
                let player = &player_;
                let shutdown = &shutdown_;
                let mut buf_reader = BufReader::new(file);
                let mut buffer = [0; 8192];
                while !shutdown.load(Ordering::Relaxed) {
                    match buf_reader.read(&mut buffer[..]) {
                        Ok(0) => {
                            println!("finished pushing data");
                            break;
                        }
                        Ok(size) => {
                            if let Err(_) = player
                                .lock()
                                .unwrap()
                                .push_data(Vec::from(&buffer[0..size]))
                            {
                                break;
                            }
                        }
                        Err(e) => {
                            eprintln!("Error: {}", e);
                            break;
                        }
                    }
                }
            })
            .unwrap();

        Builder::new()
            .name("Player event loop".to_owned())
            .spawn(move || {
                let player = &player__;
                let shutdown = &shutdown__;
                while let Ok(event) = receiver.recv() {
                    match event {
                        PlayerEvent::EndOfStream => {
                            println!("EOF");
                            break;
                        }
                        PlayerEvent::Error => {
                            println!("Error");
                            break;
                        }
                        PlayerEvent::MetadataUpdated(ref m) => {
                            println!("Metadata updated! {:?}", m);
                        }
                        PlayerEvent::StateChanged(ref s) => {
                            println!("Player state changed to {:?}", s);
                        }
                        PlayerEvent::FrameUpdated => eprint!("."),
                        PlayerEvent::PositionChanged(_) => (),
                        PlayerEvent::SeekData(_) => (),
                        PlayerEvent::SeekDone(_) => (),
                        PlayerEvent::NeedData => (),
                        PlayerEvent::EnoughData => (),
                    }
                }
                player.lock().unwrap().stop().unwrap();
                shutdown.store(true, Ordering::Relaxed);
            })
            .unwrap();

        player.lock().unwrap().play().unwrap();

        PlayerWrapper { player, shutdown }
    }

    pub fn shutdown(&self) {
        self.player.lock().unwrap().stop().unwrap();
        self.shutdown.store(true, Ordering::Relaxed);
    }

    pub fn register_frame_renderer(&self, renderer: Arc<Mutex<FrameRenderer>>) {
        self.player
            .lock()
            .unwrap()
            .register_frame_renderer(renderer);
    }
}
