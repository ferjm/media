/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Copied from WebRender's boilerplate.rs.

extern crate euclid;

use gleam::gl;
use glutin::ContextTrait;
use servo_media::player::context::*;
use servo_media::player::frame::FrameRenderer;
use servo_media::ClientContextId;
use std::path::Path;
use std::sync::{Arc, Mutex};
use webrender;
use webrender::api::*;
use webrender::ShaderPrecacheFlags;
use winit;

use player_wrapper::PlayerWrapper;

struct PlayerContextGlutin {
    gl_context: GlContext,
    native_display: NativeDisplay,
    gl_api: GlApi,
}

impl PlayerContextGlutin {
    pub fn new(use_gl: bool, windowed_context: &glutin::WindowedContext) -> Self {
        if !use_gl {
            return Self {
                gl_context: GlContext::Unknown,
                native_display: NativeDisplay::Unknown,
                gl_api: GlApi::None,
            };
        }

        let (gl_context, native_display, gl_api) = {
            use glutin::os::ContextTraitExt;

            unsafe {
                windowed_context
                    .make_current()
                    .expect("Couldn't make window current");
            }

            let context = windowed_context.context();
            let raw_handle = unsafe { context.raw_handle() };
            let api = windowed_context.get_api();

            #[cfg(any(
                target_os = "linux",
                target_os = "dragonfly",
                target_os = "freebsd",
                target_os = "netbsd",
                target_os = "openbsd"
            ))]
            {
                let gl_context = {
                    use glutin::os::unix::RawHandle;

                    match raw_handle {
                        RawHandle::Egl(egl_context) => GlContext::Egl(egl_context as usize),
                        RawHandle::Glx(glx_context) => GlContext::Glx(glx_context as usize),
                    }
                };
                let native_display = if let Some(display) =
                    unsafe { windowed_context.context().get_egl_display() }
                {
                    NativeDisplay::Egl(display as usize)
                } else {
                    use glutin::os::unix::WindowExt;

                    if let Some(display) = windowed_context.window().get_wayland_display() {
                        NativeDisplay::Wayland(display as usize)
                    } else if let Some(display) = windowed_context.window().get_xlib_display() {
                        NativeDisplay::X11(display as usize)
                    } else {
                        NativeDisplay::Unknown
                    }
                };

                let gl_api = match api {
                    glutin::Api::OpenGl => GlApi::OpenGL3,
                    glutin::Api::OpenGlEs => GlApi::Gles2,
                    _ => GlApi::None,
                };

                (gl_context, native_display, gl_api)
            }

            #[cfg(target_os = "macos")]
            {
                let context = unsafe { context.get_context_obj().unwrap() };
                let gl_context = GlContext::Cgl(context as usize);
                let native_display = NativeDisplay::Unknown;
                let gl_api = match api {
                    glutin::Api::OpenGl => GlApi::OpenGL3,
                    _ => GlApi::None,
                };

                (gl_context, native_display, gl_api)
            }

            #[cfg(not(any(
                target_os = "linux",
                target_os = "dragonfly",
                target_os = "freebsd",
                target_os = "netbsd",
                target_os = "openbsd",
                target_os = "macos",
            )))]
            {
                println!("GL rendering unavailable for this platform");
                (GlContext::Unknown, NativeDisplay::Unknown, GlApi::None)
            }
        };

        Self {
            gl_context,
            native_display,
            gl_api,
        }
    }
}

impl PlayerGLContext for PlayerContextGlutin {
    fn get_gl_context(&self) -> GlContext {
        self.gl_context.clone()
    }

    fn get_native_display(&self) -> NativeDisplay {
        self.native_display.clone()
    }

    fn get_gl_api(&self) -> GlApi {
        self.gl_api.clone()
    }
}

struct Notifier {
    events_proxy: winit::EventsLoopProxy,
}

impl Notifier {
    fn new(events_proxy: winit::EventsLoopProxy) -> Notifier {
        Notifier { events_proxy }
    }
}

impl RenderNotifier for Notifier {
    fn clone(&self) -> Box<dyn RenderNotifier> {
        Box::new(Notifier {
            events_proxy: self.events_proxy.clone(),
        })
    }

    fn wake_up(&self) {
        #[cfg(not(target_os = "android"))]
        let _ = self.events_proxy.wakeup();
    }

    fn new_frame_ready(
        &self,
        _: DocumentId,
        _scrolled: bool,
        _composite_needed: bool,
        _render_time: Option<u64>,
    ) {
        self.wake_up();
    }
}

pub trait HandyDandyRectBuilder {
    fn to(&self, x2: i32, y2: i32) -> LayoutRect;
    fn by(&self, w: i32, h: i32) -> LayoutRect;
}

// Allows doing `(x, y).to(x2, y2)` or `(x, y).by(width, height)` with i32
// values to build a f32 LayoutRect
impl HandyDandyRectBuilder for (i32, i32) {
    fn to(&self, x2: i32, y2: i32) -> LayoutRect {
        LayoutRect::new(
            LayoutPoint::new(self.0 as f32, self.1 as f32),
            LayoutSize::new((x2 - self.0) as f32, (y2 - self.1) as f32),
        )
    }

    fn by(&self, w: i32, h: i32) -> LayoutRect {
        LayoutRect::new(
            LayoutPoint::new(self.0 as f32, self.1 as f32),
            LayoutSize::new(w as f32, h as f32),
        )
    }
}

pub trait Example {
    const TITLE: &'static str = "Servo Media Test Player";
    const WIDTH: u32 = 1920;
    const HEIGHT: u32 = 1080;

    fn push_txn(
        &mut self,
        api: &RenderApi,
        builder: &mut DisplayListBuilder,
        txn: &mut Transaction,
    );
    fn on_event(&self, winit::WindowEvent, &RenderApi, DocumentId) -> bool {
        false
    }
    fn needs_repaint(&self) -> bool {
        false
    }
    fn get_image_handlers(
        &self,
        _gl: &dyn gl::Gl,
    ) -> (
        Option<Box<dyn webrender::ExternalImageHandler>>,
        Option<Box<dyn webrender::OutputImageHandler>>,
    ) {
        (None, None)
    }
    fn draw_custom(&self, _gl: &dyn gl::Gl) {}

    fn use_gl(&mut self, _use_gl: bool) {}
}

pub fn main_wrapper<E: Example + FrameRenderer>(
    example: Arc<Mutex<E>>,
    path: &Path,
    no_video: bool,
    use_gl: bool,
    options: Option<webrender::RendererOptions>,
) {
    let mut events_loop = winit::EventsLoop::new();
    let window = winit::WindowBuilder::new()
        .with_title(E::TITLE)
        .with_multitouch()
        .with_dimensions(winit::dpi::LogicalSize::new(
            E::WIDTH as f64,
            E::HEIGHT as f64,
        ));
    let windowed_context = Arc::new(
        glutin::ContextBuilder::new()
            .with_vsync(true)
            .build_windowed(window, &events_loop)
            .unwrap(),
    );

    unsafe {
        windowed_context.make_current().ok();
    }

    let gl = match windowed_context.get_api() {
        glutin::Api::OpenGl => unsafe {
            gl::GlFns::load_with(|symbol| windowed_context.get_proc_address(symbol) as *const _)
        },
        glutin::Api::OpenGlEs => unsafe {
            gl::GlesFns::load_with(|symbol| windowed_context.get_proc_address(symbol) as *const _)
        },
        glutin::Api::WebGl => unimplemented!(),
    };

    println!("OpenGL version {}", gl.get_string(gl::VERSION));
    let device_pixel_ratio = windowed_context.get_hidpi_factor() as f32;
    println!("Device pixel ratio: {}", device_pixel_ratio);
    let mut debug_flags = webrender::DebugFlags::ECHO_DRIVER_MESSAGES;

    println!("Loading shaders...");
    let opts = webrender::RendererOptions {
        resource_override_path: None,
        precache_flags: ShaderPrecacheFlags::EMPTY,
        device_pixel_ratio,
        clear_color: Some(ColorF::new(0.3, 0.0, 0.0, 1.0)),
        //scatter_gpu_cache_updates: false,
        debug_flags,
        ..options.unwrap_or(webrender::RendererOptions::default())
    };

    let framebuffer_size = {
        let size = windowed_context
            .get_inner_size()
            .unwrap()
            .to_physical(device_pixel_ratio as f64);
        DeviceIntSize::new(size.width as i32, size.height as i32)
    };
    let notifier = Box::new(Notifier::new(events_loop.create_proxy()));
    let (mut renderer, sender) =
        webrender::Renderer::new(gl.clone(), notifier, opts, None).unwrap();
    let api = sender.create_api();
    let document_id = api.add_document(framebuffer_size, 0);

    let gl_context = Box::new(PlayerContextGlutin::new(use_gl, &windowed_context));

    let example_: Option<Arc<Mutex<dyn FrameRenderer>>> = if no_video {
        None
    } else {
        Some(example.clone())
    };
    let player_wrapper =
        PlayerWrapper::new(&ClientContextId::build(1, 1), path, example_, gl_context);
    example.lock().unwrap().use_gl(player_wrapper.use_gl());

    let (external, output) = example.lock().unwrap().get_image_handlers(&*gl);

    if let Some(output_image_handler) = output {
        renderer.set_output_image_handler(output_image_handler);
    }

    if let Some(external_image_handler) = external {
        renderer.set_external_image_handler(external_image_handler);
    }

    let epoch = Epoch(0);
    let pipeline_id = PipelineId(0, 0);
    let layout_size = framebuffer_size.to_f32() / euclid::TypedScale::new(device_pixel_ratio);
    let mut builder = DisplayListBuilder::new(pipeline_id, layout_size);
    let mut txn = Transaction::new();

    example
        .lock()
        .unwrap()
        .push_txn(&api, &mut builder, &mut txn);
    txn.set_display_list(epoch, None, layout_size, builder.finalize(), true);
    txn.set_root_pipeline(pipeline_id);
    txn.generate_frame();
    api.send_transaction(document_id, txn);

    println!("Entering event loop");
    events_loop.run_forever(|global_event| {
        let mut txn = Transaction::new();
        let mut custom_event = true;

        match global_event {
            winit::Event::WindowEvent {
                event: winit::WindowEvent::CloseRequested,
                ..
            } => return winit::ControlFlow::Break,
            winit::Event::WindowEvent {
                event:
                    winit::WindowEvent::KeyboardInput {
                        input:
                            winit::KeyboardInput {
                                state: winit::ElementState::Pressed,
                                virtual_keycode: Some(key),
                                ..
                            },
                        ..
                    },
                ..
            } => match key {
                winit::VirtualKeyCode::Escape => return winit::ControlFlow::Break,
                winit::VirtualKeyCode::P => debug_flags.toggle(webrender::DebugFlags::PROFILER_DBG),
                winit::VirtualKeyCode::O => {
                    debug_flags.toggle(webrender::DebugFlags::RENDER_TARGET_DBG)
                }
                winit::VirtualKeyCode::I => {
                    debug_flags.toggle(webrender::DebugFlags::TEXTURE_CACHE_DBG)
                }
                winit::VirtualKeyCode::S => {
                    debug_flags.toggle(webrender::DebugFlags::COMPACT_PROFILER)
                }
                winit::VirtualKeyCode::Q => debug_flags.toggle(
                    webrender::DebugFlags::GPU_TIME_QUERIES
                        | webrender::DebugFlags::GPU_SAMPLE_QUERIES,
                ),
                winit::VirtualKeyCode::Key1 => txn.set_window_parameters(
                    framebuffer_size,
                    DeviceIntRect::new(DeviceIntPoint::zero(), framebuffer_size),
                    1.0,
                ),
                winit::VirtualKeyCode::Key2 => txn.set_window_parameters(
                    framebuffer_size,
                    DeviceIntRect::new(DeviceIntPoint::zero(), framebuffer_size),
                    2.0,
                ),
                winit::VirtualKeyCode::M => api.notify_memory_pressure(),
                #[cfg(feature = "capture")]
                winit::VirtualKeyCode::C => {
                    let path: PathBuf = "../captures/example".into();
                    //TODO: switch between SCENE/FRAME capture types
                    // based on "shift" modifier, when `glutin` is updated.
                    let bits = CaptureBits::all();
                    api.save_capture(path, bits);
                }
                _ => {
                    let win_event = match global_event {
                        winit::Event::WindowEvent { event, .. } => event,
                        _ => unreachable!(),
                    };
                    custom_event = example
                        .lock()
                        .unwrap()
                        .on_event(win_event, &api, document_id)
                }
            },
            winit::Event::WindowEvent { event, .. } => {
                custom_event = example.lock().unwrap().on_event(event, &api, document_id)
            }
            _ => (),
        };

        if custom_event || example.lock().unwrap().needs_repaint() {
            let mut builder = DisplayListBuilder::new(pipeline_id, layout_size);

            example
                .lock()
                .unwrap()
                .push_txn(&api, &mut builder, &mut txn);
            txn.set_display_list(epoch, None, layout_size, builder.finalize(), true);
            txn.generate_frame();
        }
        api.send_transaction(document_id, txn);

        renderer.update();
        renderer.render(framebuffer_size).unwrap();
        let _ = renderer.flush_pipeline_info();
        example.lock().unwrap().draw_custom(&*gl);
        windowed_context.swap_buffers().ok();

        winit::ControlFlow::Continue
    });

    player_wrapper.shutdown();
    renderer.deinit();
}
