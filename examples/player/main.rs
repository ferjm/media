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

struct FrameProvider {
    default_texture_id: gl::GLuint,
    frame_queue: Arc<Mutex<FrameQueue>>,
    cur_frame: Option<Frame>,
}

impl FrameProvider {
    fn new(frame_queue: Arc<Mutex<FrameQueue>>, gl: &gl::Gl) -> Self {
        let texture_id = gl.gen_textures(1)[0];

        gl.bind_texture(gl::TEXTURE_2D, texture_id);
        gl.tex_parameter_i(
            gl::TEXTURE_2D,
            gl::TEXTURE_MAG_FILTER,
            gl::LINEAR as gl::GLint,
        );
        gl.tex_parameter_i(
            gl::TEXTURE_2D,
            gl::TEXTURE_MIN_FILTER,
            gl::LINEAR as gl::GLint,
        );
        gl.tex_parameter_i(
            gl::TEXTURE_2D,
            gl::TEXTURE_WRAP_S,
            gl::CLAMP_TO_EDGE as gl::GLint,
        );
        gl.tex_parameter_i(
            gl::TEXTURE_2D,
            gl::TEXTURE_WRAP_T,
            gl::CLAMP_TO_EDGE as gl::GLint,
        );
        gl.tex_image_2d(
            gl::TEXTURE_2D,
            0,
            gl::RGBA as gl::GLint,
            100,
            100,
            0,
            gl::BGRA,
            gl::UNSIGNED_BYTE,
            None,
        );
        gl.bind_texture(gl::TEXTURE_2D, 0);

        Self {
            default_texture_id: texture_id,
            frame_queue: frame_queue,
            cur_frame: None,
        }
    }
}

impl webrender::ExternalImageHandler for FrameProvider {
    fn lock(
        &mut self,
        _key: ExternalImageId,
        _channel_index: u8,
        _rendering: ImageRendering,
    ) -> webrender::ExternalImage {
        self.cur_frame = self.frame_queue.lock().unwrap().prev();

        let id = self
            .cur_frame
            .take()
            .and_then(|frame| {
                self.cur_frame = Some(frame.clone());
                Some(frame.get_texture_id())
            })
            .or_else(|| Some(self.default_texture_id))
            .unwrap();

        eprint!("/{:?}/", id);

        webrender::ExternalImage {
            uv: TexelRect::new(0.0, 0.0, 1.0, 1.0),
            source: webrender::ExternalImageSource::NativeTexture(id),
        }
    }
    fn unlock(&mut self, _key: ExternalImageId, _channel_index: u8) {}
}

struct FrameQueue {
    prev_frame: Option<Frame>,
    cur_frame: Option<Frame>,
    repaint: bool,
}

impl FrameQueue {
    fn new() -> Self {
        Self {
            prev_frame: None,
            cur_frame: None,
            repaint: false,
        }
    }

    fn add(&mut self, frame: Frame) {
        self.cur_frame = Some(frame);
        self.repaint = true;
    }

    fn get(&mut self) -> Option<Frame> {
        self.cur_frame
            .take()
            .and_then(|frame| {
                self.repaint = false;
                self.prev_frame = Some(frame.clone());
                Some(frame)
            })
            .or_else(|| self.prev())
    }

    fn prev(&mut self) -> Option<Frame> {
        self.prev_frame.take().and_then(|frame| {
            self.prev_frame = Some(frame.clone());
            Some(frame)
        })
    }

    fn needs_repaint(&self) -> bool {
        self.repaint
    }

    fn is_empty(&self) -> bool {
        self.cur_frame.is_none() && self.prev_frame.is_none()
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
            txn.update_image(
                self.image_key.clone().unwrap(),
                image_descriptor,
                image_data,
                &DirtyRect::All,
            );
        } else {
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
        gl: &gl::Gl,
    ) -> (
        Option<Box<webrender::ExternalImageHandler>>,
        Option<Box<webrender::OutputImageHandler>>,
    ) {
        if !self.use_gl {
            (None, None)
        } else {
            (
                Some(Box::new(FrameProvider::new(self.frame_queue.clone(), gl))),
                None,
            )
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
    ui::main_wrapper(app, &path, use_gl, None);
}
