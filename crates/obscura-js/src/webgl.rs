//! GPU-accelerated WebGL context using glow (OpenGL bindings).
//!
//! Provides a real GPU-backed WebGL implementation for headless rendering.
//! Uses EGL for headless GL context on Linux.

use std::ffi::CString;

pub struct GlContext {
    #[cfg(target_os = "linux")]
    egl_display: *mut std::os::raw::c_void,
    #[cfg(target_os = "linux")]
    egl_context: *mut std::os::raw::c_void,
    #[cfg(target_os = "linux")]
    egl_surface: *mut std::os::raw::c_void,
    gl: Option<glow::Context>,
}

unsafe impl Send for GlContext {}
unsafe impl Sync for GlContext {}

impl GlContext {
    /// Create a headless GL context with the given dimensions.
    pub fn new_headless(width: u32, height: u32) -> Result<Self, String> {
        #[cfg(target_os = "linux")]
        {
            Self::new_egl_headless(width, height)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err("Headless GL context not supported on this platform".into())
        }
    }

    #[cfg(target_os = "linux")]
    fn new_egl_headless(width: u32, height: u32) -> Result<Self, String> {
        unsafe {
            let display = egl::get_display(egl::DEFAULT_DISPLAY);
            if display.is_null() {
                return Err("Failed to get EGL display".into());
            }

            let mut major = 0;
            let mut minor = 0;
            if !egl::initialize(display, &mut major, &mut minor) {
                return Err("Failed to initialize EGL".into());
            }

            let config_attribs = [
                egl::SURFACE_TYPE as i32,
                egl::PIXMAP_BIT as i32,
                egl::RED_SIZE as i32,
                8,
                egl::GREEN_SIZE as i32,
                8,
                egl::BLUE_SIZE as i32,
                8,
                egl::ALPHA_SIZE as i32,
                8,
                egl::DEPTH_SIZE as i32,
                24,
                egl::STENCIL_SIZE as i32,
                8,
                egl::RENDERABLE_TYPE as i32,
                egl::OPENGL_ES2_BIT as i32,
                egl::NONE as i32,
            ];

            let mut config = std::ptr::null_mut();
            let mut num_configs = 0;
            egl::choose_config(
                display,
                config_attribs.as_ptr(),
                &mut config,
                1,
                &mut num_configs,
            );

            if num_configs == 0 {
                return Err("No suitable EGL config".into());
            }

            let context_attribs = [egl::CONTEXT_CLIENT_VERSION as i32, 2, egl::NONE as i32];
            let context = egl::create_context(
                display,
                config,
                egl::NO_CONTEXT,
                context_attribs.as_ptr(),
            );
            if context.is_null() {
                return Err("Failed to create EGL context".into());
            }

            let surface_attribs = [
                egl::WIDTH as i32,
                width as i32,
                egl::HEIGHT as i32,
                height as i32,
                egl::NONE as i32,
            ];
            let surface =
                egl::create_pbuffer_surface(display, config, surface_attribs.as_ptr());
            if surface.is_null() {
                return Err("Failed to create EGL surface".into());
            }

            egl::make_current(display, surface, surface, context);

            let gl = glow::Context::from_loader_cstr(|name| {
                let name = CString::new(name).unwrap();
                egl::get_proc_address(name.as_ptr()) as *const _
            });

            Ok(GlContext {
                egl_display: display,
                egl_context: context,
                egl_surface: surface,
                gl: Some(gl),
            })
        }
    }

    pub fn gl(&self) -> &glow::Context {
        self.gl.as_ref().expect("GL context not initialized")
    }

    pub fn make_current(&self) {
        #[cfg(target_os = "linux")]
        unsafe {
            egl::make_current(
                self.egl_display,
                self.egl_surface,
                self.egl_surface,
                self.egl_context,
            );
        }
    }

    pub fn swap_buffers(&self) {
        #[cfg(target_os = "linux")]
        unsafe {
            egl::swap_buffers(self.egl_display, self.egl_surface);
        }
    }

    pub fn read_pixels(&self, x: i32, y: i32, width: i32, height: i32) -> Vec<u8> {
        let gl = self.gl();
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        unsafe {
            gl.read_pixels(
                x,
                y,
                width,
                height,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(&mut pixels),
            );
        }
        pixels
    }
}

impl Drop for GlContext {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        unsafe {
            egl::make_current(
                self.egl_display,
                egl::NO_SURFACE,
                egl::NO_SURFACE,
                egl::NO_CONTEXT,
            );
            egl::destroy_surface(self.egl_display, self.egl_surface);
            egl::destroy_context(self.egl_display, self.egl_context);
            egl::terminate(self.egl_display);
        }
    }
}
