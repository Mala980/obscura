use std::ffi::CString;
use std::num::NonZeroU32;
use std::sync::Arc;

use glutin::config::ConfigTemplateBuilder;
use glutin::context::{ContextApi, ContextAttributesBuilder, Version};
use glutin::display::GetGlDisplay;
use glutin::prelude::*;
use glutin::surface::{SurfaceAttributesBuilder, WindowSurface};
use glutin::{display::Display, surface::Surface};
use glutin_winit::{DisplayBuilder, GlWindow};
use raw_window_handle::HasRawWindowHandle;
use winit::dpi::LogicalSize;
use winit::event_loop::EventLoop;
use winit::window::WindowBuilder;

pub struct EglHeadlessContext {
    display: Display,
    context: glutin::context::PossiblyCurrentContext,
    surface: Surface<WindowSurface>,
    window: Arc<winit::window::Window>,
}

unsafe impl Send for EglHeadlessContext {}
unsafe impl Sync for EglHeadlessContext {}

impl EglHeadlessContext {
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        let event_loop = EventLoop::new().map_err(|e| format!("Failed to create event loop: {e}"))?;

        let size = LogicalSize::new(width as f64, height as f64);

        let window_builder = WindowBuilder::new()
            .with_title("obscura-headless")
            .with_inner_size(size)
            .with_visible(false);

        let template = ConfigTemplateBuilder::new().with_alpha_size(8);

        let display_builder = DisplayBuilder::new().with_window_builder(Some(window_builder));

        let (window, gl_config) = display_builder
            .build(&event_loop, template, |configs| {
                configs
                    .reduce(|accum, config| {
                        if config.num_samples() > accum.num_samples() {
                            config
                        } else {
                            accum
                        }
                    })
                    .unwrap()
            })
            .map_err(|e| format!("Failed to build display: {e}"))?;

        let raw_window_handle = window
            .as_ref()
            .map(|w| w.raw_window_handle());

        let context_attributes = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::OpenGl(Some(Version::new(3, 3))))
            .build(raw_window_handle);

        let gl_display = gl_config.display();

        let not_current_context = unsafe {
            gl_display
                .create_context(&gl_config, &context_attributes)
                .map_err(|e| format!("Failed to create GL context: {e}"))?
        };

        let window = window.ok_or_else(|| "Failed to create window".to_string())?;

        let attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(
            raw_window_handle::RawWindowHandle::from(window.raw_window_handle()),
            NonZeroU32::new(width).unwrap(),
            NonZeroU32::new(height).unwrap(),
        );

        let surface = unsafe {
            gl_display
                .create_window_surface(&gl_config, &attrs)
                .map_err(|e| format!("Failed to create window surface: {e}"))?
        };

        let context = not_current_context
            .make_current(&surface)
            .map_err(|e| format!("Failed to make context current: {e}"))?;

        Ok(Self {
            display: gl_display,
            context,
            surface,
            window: Arc::new(window),
        })
    }

    pub fn load_gl(&self) -> glow::Context {
        unsafe {
            glow::Context::from_loader_function(|name| {
                let name = CString::new(name).unwrap();
                self.display.get_proc_address(&name) as *const _
            })
        }
    }

    pub fn gl_window(&self) -> (Arc<winit::window::Window>, &Surface<WindowSurface>, &glutin::context::PossiblyCurrentContext) {
        (self.window.clone(), &self.surface, &self.context)
    }
}
