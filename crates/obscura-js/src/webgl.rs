//! GPU-accelerated WebGL context using glow (OpenGL bindings).
//!
//! Provides a real GPU-backed WebGL implementation for headless rendering.
//! Uses the obscura-render-gpu crate's EGL headless context (glutin-based)
//! so the browser and the renderer share the same GL context infrastructure.

use std::ffi::CString;
use std::sync::Arc;

pub struct GlContext {
    egl: Arc<obscura_render_gpu::EglHeadlessContext>,
    gl: glow::Context,
}

unsafe impl Send for GlContext {}
unsafe impl Sync for GlContext {}

impl GlContext {
    /// Create a headless GL context with the given dimensions.
    pub fn new_headless(width: u32, height: u32) -> Result<Self, String> {
        let egl = Arc::new(obscura_render_gpu::EglHeadlessContext::new(width, height)?);
        let gl = egl.load_gl();
        Ok(Self { egl, gl })
    }

    pub fn gl(&self) -> &glow::Context {
        &self.gl
    }

    pub fn make_current(&self) {
        // The EGL context is created current already; make_current is a no-op
        // because glutin manages the current context per-thread.
    }

    pub fn swap_buffers(&self) {
        // Window surface swap is managed by glutin; expose for parity.
    }

    pub fn read_pixels(&self, x: i32, y: i32, width: i32, height: i32) -> Vec<u8> {
        let gl = &self.gl;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        unsafe {
            gl.read_pixels(
                x,
                y,
                width,
                height,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut pixels)),
            );
        }
        pixels
    }
}

// Helper used by the glow loader signature; retained for API stability.
#[allow(dead_code)]
fn loader(name: &str, display: &glutin::display::Display) -> *const std::ffi::c_void {
    let name = CString::new(name).unwrap();
    display.get_proc_address(&name) as *const std::ffi::c_void
}
