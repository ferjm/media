// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use ipc_channel::ipc;
use servo_media::player::context::PlayerGLContext;
use servo_media::player::frame::{Frame, FrameRenderer};
use servo_media::player::{Player, PlayerError, PlayerEvent, StreamType};
use servo_media::ServoMedia;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::Builder;

pub struct PlayerWrapper {
    player: Arc<Mutex<Box<dyn Player>>>,
    shutdown: Arc<AtomicBool>,
}

impl PlayerWrapper {
    pub fn new(path: &Path, gl_context: Box<PlayerGLContext>) -> Self {
        let servo_media = ServoMedia::get().unwrap();
        let player = Arc::new(Mutex::new(
            servo_media.create_player(StreamType::Seekable, gl_context),
        ));

        let file = File::open(&path).unwrap();
        let metadata = file.metadata().unwrap();

        player
            .lock()
            .unwrap()
            .set_input_size(metadata.len())
            .unwrap();

        let (sender, receiver) = ipc::channel().unwrap();
        player.lock().unwrap().register_event_handler(sender);

        let player_ = player.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_ = shutdown.clone();

        Builder::new()
            .name("Player event loop".to_owned())
            .spawn(move || {
                let player = &player_;
                let mut buf_reader = BufReader::new(file);
                let mut buffer = [0; 8192];
                let mut input_eos = false;
                let shutdown = shutdown_;

                while let Ok(event) = receiver.recv() {
                    match event {
                        PlayerEvent::EndOfStream => {
                            println!("EOF");
                            break;
                        }
                        PlayerEvent::Error(ref s) => {
                            println!("Player's Error {:?}", s);
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
                        PlayerEvent::SeekData(offset) => {
                            println!("Seek requested to position {:?}", offset);
                            input_eos = false;
                            if buf_reader.seek(SeekFrom::Start(offset)).is_err() {
                                eprintln!("BufReader - Could not seek to {:?}", offset);
                                break;
                            }
                        }
                        PlayerEvent::SeekDone(offset) => {
                            println!("Seek done to position {:?}", offset);
                        }
                        PlayerEvent::NeedData => {
                            if !input_eos {
                                match buf_reader.read(&mut buffer[..]) {
                                    Ok(0) => {
                                        if player.lock().unwrap().end_of_stream().is_err() {
                                            eprintln!("Error at setting end of stream");
                                            break;
                                        } else {
                                            input_eos = true;
                                        }
                                    }
                                    Ok(size) => {
                                        match player
                                            .lock()
                                            .unwrap()
                                            .push_data(Vec::from(&buffer[0..size]))
                                        {
                                            Ok(_) => (),
                                            Err(PlayerError::EnoughData) => {
                                                print!("!");
                                            }
                                            Err(e) => {
                                                eprintln!("Error at pushing data {:?}", e);
                                                break;
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        eprintln!("Error reading file: {}", e);
                                        break;
                                    }
                                }
                            }
                        }
                        PlayerEvent::EnoughData => {
                            println!("Player has enough data");
                        }
                    }

                    if shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                }

                player.lock().unwrap().shutdown().unwrap();
            })
            .unwrap();

        player.lock().unwrap().play().unwrap();

        PlayerWrapper { player, shutdown }
    }

    pub fn shutdown(&self) {
        self.player.lock().unwrap().shutdown().unwrap();
        self.shutdown.store(true, Ordering::Relaxed);
    }

    pub fn use_gl(&self) -> bool {
        self.player.lock().unwrap().render_use_gl()
    }

    pub fn disable_video(&self) {
        self.player.lock().unwrap().disable_video().unwrap();
    }

    pub fn register_frame_renderer(&self, renderer: Arc<Mutex<FrameRenderer>>) {
        self.player
            .lock()
            .unwrap()
            .register_frame_renderer(renderer);
    }
}
