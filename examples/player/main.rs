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
extern crate time;
extern crate webrender;
#[cfg(not(target_os = "android"))]
extern crate winit;

use gleam::gl;
use servo_media::player::frame::{Frame, FrameRenderer};
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

struct App {
    frame_queue: Vec<Frame>,
    current_frame: Option<Frame>,
    image_key: Option<ImageKey>,
    use_gl: bool,
}

impl App {
    fn new() -> Self {
        Self {
            frame_queue: Vec::new(),
            current_frame: None,
            image_key: None,
            use_gl: false,
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
        let frame = if self.frame_queue.is_empty() {
            if self.current_frame.is_none() {
                return;
            }
            self.current_frame.take().unwrap()
        } else {
            self.frame_queue.pop().unwrap()
        };
        let width = frame.get_width();
        let height = frame.get_height();

        if self.image_key.is_some() && self.current_frame.is_some() {
            let old_frame = self.current_frame.take().unwrap();
            let old_width = old_frame.get_width();
            let old_height = old_frame.get_height();
            if (width != old_width) || (height != old_height) {
                txn.delete_image(self.image_key.unwrap());
                self.image_key = None;
            }
        }

        let mut image_descriptor =
            ImageDescriptor::new(width, height, ImageFormat::BGRA8, false, false);
        image_descriptor.stride = frame.get_stride();
        image_descriptor.offset = frame.get_offset();

        let image_data = if !self.use_gl {
            ImageData::new_shared(frame.get_data().clone())
        } else {
            ImageData::External(ExternalImageData {
                id: ExternalImageId(0),
                channel_index: 0,
                image_type: ExternalImageType::TextureHandle(TextureTarget::Default),
            })
        };

        if self.image_key.is_none() {
            self.image_key = Some(api.generate_image_key());
            txn.add_image(
                self.image_key.clone().unwrap(),
                image_descriptor,
                image_data,
                None,
            );
        } else if !self.use_gl {
            // TODO: fix tearing
            txn.update_image(
                self.image_key.clone().unwrap(),
                image_descriptor,
                image_data,
                &DirtyRect::All,
            );
        } else
        /* if self.use_gl */
        {
            return;
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
        !self.frame_queue.is_empty()
    }

    fn get_image_handlers(
        &self,
        _gl: &gl::Gl,
    ) -> (
        Option<Box<webrender::ExternalImageHandler>>,
        Option<Box<webrender::OutputImageHandler>>,
    ) {
        (None, None)
    }

    fn draw_custom(&self, _gl: &gl::Gl) {}

    fn use_gl(&mut self, use_gl: bool) {
        self.use_gl = use_gl;
    }
}

impl FrameRenderer for App {
    fn render(&mut self, frame: Frame) {
        self.frame_queue.push(frame);
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
    ui::main_wrapper(app, &path, use_gl, None);
}
