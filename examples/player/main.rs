#![allow(unused_imports)]
#![allow(dead_code)]

// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

extern crate gleam;
#[cfg(not(target_os = "android"))]
extern crate glutin;
extern crate ipc_channel;
extern crate servo_media;
extern crate servo_media_auto;
extern crate time;
extern crate webrender;
#[cfg(not(target_os = "android"))]
extern crate winit;

use gleam::gl;
use servo_media::player::{Frame, FrameRenderer};
use servo_media::ServoMedia;
use std::env;
use std::path::Path;
use std::sync::{Arc, Mutex};
#[cfg(not(target_os = "android"))]
use ui::HandyDandyRectBuilder;
use webrender::api::*;

#[cfg(not(target_os = "android"))]
#[path = "ui.rs"]
mod ui;

#[path = "player_wrapper.rs"]
mod player_wrapper;

struct FrameProvider {
    frame_queue: Arc<Mutex<FrameQueue>>,
    curr_frame: Option<Frame>,
}

impl webrender::ExternalImageHandler for FrameProvider {
    fn lock(
        &mut self,
        _key: ExternalImageId,
        _channel_index: u8,
        _rendering: ImageRendering,
    ) -> webrender::ExternalImage {
        self.curr_frame = self.frame_queue.lock().unwrap().get();

        let (id, height, width) = self
            .curr_frame
            .clone() // clone it because we want to keep the current texture alive
            .and_then(|frame| {
                Some((
                    frame.get_texture_id(),
                    frame.get_height(),
                    frame.get_width(),
                ))
            })
            .unwrap();

        webrender::ExternalImage {
            uv: TexelRect::new(0.0, 0.0, width as f32, height as f32),
            source: webrender::ExternalImageSource::NativeTexture(id),
        }
    }
    fn unlock(&mut self, _key: ExternalImageId, _channel_index: u8) {}
}

struct FrameQueue {
    next_frame: Option<Frame>,
    curr_frame: Option<Frame>,
    prev_frame: Option<Frame>,
    repaint: bool,
}

impl FrameQueue {
    fn new() -> Self {
        Self {
            next_frame: None,
            curr_frame: None,
            prev_frame: None,
            repaint: false,
        }
    }

    fn add(&mut self, frame: Frame) {
        let _ = self.next_frame.replace(frame);
        self.repaint = true;
    }

    fn get(&mut self) -> Option<Frame> {
        if self.is_empty() {
            return None;
        }

        if self.next_frame.is_some() {
            self.prev_frame = self.curr_frame.take();
            self.curr_frame = self.next_frame.take();
        } else {
            if self.curr_frame.is_none() {
                self.repaint = true;
                return self.prev_frame.clone();
            } else {
                self.repaint = false;
            }
        }

        self.curr_frame.clone()
    }

    fn prev(&mut self) -> Option<Frame> {
        self.prev_frame.clone()
    }

    fn needs_repaint(&self) -> bool {
        self.repaint
    }

    fn is_empty(&self) -> bool {
        self.next_frame.is_none() && self.curr_frame.is_none() && self.next_frame.is_none()
    }
}

struct App {
    frame_queue: Arc<Mutex<FrameQueue>>,
    image_key: Option<ImageKey>,
    use_gl: bool,
}

impl App {
    fn new() -> Self {
        Self {
            frame_queue: Arc::new(Mutex::new(FrameQueue::new())),
            image_key: None,
            use_gl: false,
        }
    }

    fn init_image_key(&mut self, api: &RenderApi, txn: &mut Transaction, frame: &Frame) {
        if self.image_key.is_some() {
            return;
        }

        self.image_key = Some(api.generate_image_key());
        let image_descriptor = ImageDescriptor::new(
            frame.get_width(),
            frame.get_height(),
            ImageFormat::BGRA8,
            false,
            false,
        );

        if self.use_gl {
            txn.add_image(
                self.image_key.clone().unwrap(),
                image_descriptor,
                ImageData::External(ExternalImageData {
                    id: ExternalImageId(0),
                    channel_index: 0,
                    image_type: ExternalImageType::TextureHandle(TextureTarget::Default),
                }),
                None,
            );
        } else {
            txn.add_image(
                self.image_key.clone().unwrap(),
                image_descriptor,
                ImageData::new_shared(frame.get_data()),
                None,
            );
        }
    }
}

#[cfg(not(target_os = "android"))]
impl ui::Example for App {
    fn push_txn(
        &mut self,
        api: &RenderApi,
        builder: &mut DisplayListBuilder,
        txn: &mut Transaction,
    ) {
        if self.frame_queue.lock().unwrap().is_empty() {
            return; /* we are not ready yet, sir */
        }

        let frame = self.frame_queue.lock().unwrap().get().unwrap();
        let width = frame.get_width();
        let height = frame.get_height();

        if self.image_key.is_some() {
            if let Some(old_frame) = self.frame_queue.lock().unwrap().prev() {
                let old_width = old_frame.get_width();
                let old_height = old_frame.get_height();
                if (width != old_width) || (height != old_height) {
                    txn.delete_image(self.image_key.unwrap());
                    self.image_key = None;
                }
            }
        }

        if self.image_key.is_none() {
            self.init_image_key(api, txn, &frame);
        } else if !self.use_gl {
            txn.update_image(
                self.image_key.clone().unwrap(),
                ImageDescriptor::new(width, height, ImageFormat::BGRA8, false, false),
                ImageData::new_shared(frame.get_data()),
                &DirtyRect::All,
            );
        }

        let bounds = (0, 0).to(width as i32, height as i32);
        let info = LayoutPrimitiveInfo::new(bounds);
        builder.push_stacking_context(
            &info,
            None,
            TransformStyle::Flat,
            MixBlendMode::Normal,
            &[],
            RasterSpace::Screen,
        );

        let image_size = LayoutSize::new(width as f32, height as f32);
        let info = LayoutPrimitiveInfo::new(bounds);
        builder.push_image(
            &info,
            image_size,
            LayoutSize::zero(),
            ImageRendering::Auto,
            AlphaType::PremultipliedAlpha,
            self.image_key.clone().unwrap(),
            ColorF::WHITE,
        );
        builder.pop_stacking_context();
    }

    fn on_event(&self, _: winit::WindowEvent, _: &RenderApi, _: DocumentId) -> bool {
        false
    }

    fn needs_repaint(&self) -> bool {
        self.frame_queue.lock().unwrap().needs_repaint()
    }

    fn get_image_handlers(
        &self,
        _gl: &gl::Gl,
    ) -> (
        Option<Box<webrender::ExternalImageHandler>>,
        Option<Box<webrender::OutputImageHandler>>,
    ) {
        if !self.use_gl {
            (None, None)
        } else {
            let queue = self.frame_queue.clone();
            let provider = FrameProvider {
                frame_queue: queue,
                curr_frame: None,
            };
            (Some(Box::new(provider)), None)
        }
    }

    fn draw_custom(&self, _gl: &gl::Gl) {}

    fn use_gl(&mut self, use_gl: bool) {
        self.use_gl = use_gl;
    }
}

impl FrameRenderer for App {
    fn render(&mut self, frame: Frame) {
        self.frame_queue.lock().unwrap().add(frame)
    }
}

#[cfg(target_os = "android")]
fn main() {
    panic!("Unsupported");
}

#[cfg(not(target_os = "android"))]
fn main() {
    let args: Vec<_> = env::args().collect();
    let (use_gl, filename) = if args.len() == 2 {
        let fname: &str = args[1].as_ref();
        (false, fname)
    } else if args.len() == 3 {
        if args[1] == "--gl" {
            let fname: &str = args[2].as_ref();
            (true, fname)
        } else {
            panic!("Usage: cargo run --bin player [--gl] <file_path>")
        }
    } else {
        panic!("Usage: cargo run --bin player [--gl] <file_path>")
    };

    let path = Path::new(filename);
    let app = Arc::new(Mutex::new(App::new()));
    ServoMedia::init::<servo_media_auto::Backend>();
    ui::main_wrapper(app, &path, use_gl, None);
}
