//! Headless EGL OpenGL context using pbuffer surfaces.
//!
//! This creates a GPU-backed OpenGL context without any window system
//! (X11/Wayland) by using EGL pbuffer surfaces, which is exactly how
//! headless browsers like Chrome headless and Servo create their GL context.
//!
//! EGL constants are defined manually because the `egl` crate v0.2 ships
//! raw FFI bindings without the C constant values.

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// EGL constants (from EGL/egl.h)
// ---------------------------------------------------------------------------
const EGL_DEFAULT_DISPLAY: *mut c_void = std::ptr::null_mut();
const EGL_NO_CONTEXT: *mut c_void = std::ptr::null_mut();
const EGL_NO_SURFACE: *mut c_void = std::ptr::null_mut();
const EGL_OPENGL_ES_API: i32 = 0x30A0;
const EGL_TRUE: i32 = 1;
const EGL_FALSE: i32 = 0;
const EGL_SUCCESS: i32 = 0x3000;

// Config attributes
const EGL_SURFACE_TYPE: i32 = 0x3033;
const EGL_PBUFFER_BIT: i32 = 0x0001;
const EGL_RED_SIZE: i32 = 0x3024;
const EGL_GREEN_SIZE: i32 = 0x3023;
const EGL_BLUE_SIZE: i32 = 0x3022;
const EGL_ALPHA_SIZE: i32 = 0x3021;
const EGL_DEPTH_SIZE: i32 = 0x3025;
const EGL_STENCIL_SIZE: i32 = 0x3026;
const EGL_RENDERABLE_TYPE: i32 = 0x3040;
const EGL_OPENGL_ES2_BIT: i32 = 0x0004;
const EGL_NONE: i32 = 0x3038;

// Context attributes
const EGL_CONTEXT_CLIENT_VERSION: i32 = 0x3098;
const EGL_CONTEXT_CLIENT_TYPE: i32 = 0x3097;
const EGL_CONTEXT_MAJOR_VERSION: i32 = 0x3098;

// Surface attributes
const EGL_WIDTH: i32 = 0x3057;
const EGL_HEIGHT: i32 = 0x3056;

// Read/draw
const EGL_DRAW: i32 = 0x3059;
const EGL_READ: i32 = 0x305A;

// ---------------------------------------------------------------------------
// Raw EGL function pointer types
// ---------------------------------------------------------------------------
type EglGetDisplay = unsafe extern "C" fn(*const c_void) -> *mut c_void;
type EglInitialize =
    unsafe extern "C" fn(*mut c_void, *mut c_int, *mut c_int) -> c_int;
type EglChooseConfig = unsafe extern "C" fn(
    *mut c_void,
    *const c_int,
    *mut *mut c_void,
    c_int,
    *mut c_int,
) -> c_int;
type EglCreateContext = unsafe extern "C" fn(
    *mut c_void,
    *mut c_void,
    *mut c_void,
    *const c_int,
) -> *mut c_void;
type EglCreatePbufferSurface =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_int) -> *mut c_void;
type EglMakeCurrent = unsafe extern "C" fn(
    *mut c_void,
    *mut c_void,
    *mut c_void,
    *mut c_void,
) -> c_int;
type EglGetProcAddress = unsafe extern "C" fn(*const c_char) -> *const c_void;
type EglSwapBuffers = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type EglDestroySurface = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type EglDestroyContext = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type EglTerminate = unsafe extern "C" fn(*mut c_void) -> c_int;
type EglBindApi = unsafe extern "C" fn(c_int) -> c_int;
type EglGetError = unsafe extern "C" fn() -> c_int;

/// A headless GPU-backed OpenGL context created via EGL pbuffer surface.
pub struct EglHeadlessContext {
    display: *mut c_void,
    context: *mut c_void,
    surface: *mut c_void,
    lib: Option<*mut c_void>,
}

unsafe impl Send for EglHeadlessContext {}
unsafe impl Sync for EglHeadlessContext {}

fn last_error() -> String {
    let get_error: EglGetError =
        unsafe { std::mem::transmute(egl_proc("eglGetError")) };
    let code = unsafe { get_error() };
    format!("EGL error 0x{:04x}", code as u16)
}

fn egl_proc(name: &str) -> *const c_void {
    let name = CString::new(name).unwrap();
    unsafe {
        let lib = libc::dlopen(
            CString::new("libEGL.so.1").unwrap().as_ptr(),
            libc::RTLD_NOW,
        );
        if lib.is_null() {
            return std::ptr::null();
        }
        let ptr = libc::dlsym(lib, name.as_ptr());
        libc::dlclose(lib);
        ptr as *const c_void
    }
}

impl EglHeadlessContext {
    /// Create a headless GPU-backed OpenGL context with the given dimensions.
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        unsafe {
            let get_display: EglGetDisplay =
                std::mem::transmute(egl_proc("eglGetDisplay"));
            let initialize: EglInitialize =
                std::mem::transmute(egl_proc("eglInitialize"));
            let choose_config: EglChooseConfig =
                std::mem::transmute(egl_proc("eglChooseConfig"));
            let create_context: EglCreateContext =
                std::mem::transmute(egl_proc("eglCreateContext"));
            let create_surface: EglCreatePbufferSurface =
                std::mem::transmute(egl_proc("eglCreatePbufferSurface"));
            let make_current: EglMakeCurrent =
                std::mem::transmute(egl_proc("eglMakeCurrent"));
            let bind_api: EglBindApi = std::mem::transmute(egl_proc("eglBindAPI"));

            let display = get_display(EGL_DEFAULT_DISPLAY);
            if display.is_null() {
                return Err(format!("eglGetDisplay failed: {}", last_error()));
            }

            let mut major = 0;
            let mut minor = 0;
            if initialize(display, &mut major, &mut minor) == EGL_FALSE {
                return Err(format!("eglInitialize failed: {}", last_error()));
            }

            if bind_api(EGL_OPENGL_ES_API) == EGL_FALSE {
                return Err(format!("eglBindAPI failed: {}", last_error()));
            }

            let config_attribs = [
                EGL_SURFACE_TYPE,
                EGL_PBUFFER_BIT,
                EGL_RED_SIZE,
                8,
                EGL_GREEN_SIZE,
                8,
                EGL_BLUE_SIZE,
                8,
                EGL_ALPHA_SIZE,
                8,
                EGL_DEPTH_SIZE,
                24,
                EGL_STENCIL_SIZE,
                8,
                EGL_RENDERABLE_TYPE,
                EGL_OPENGL_ES2_BIT,
                EGL_NONE,
            ];

            let mut config: *mut c_void = std::ptr::null_mut();
            let mut num_configs = 0;
            choose_config(
                display,
                config_attribs.as_ptr(),
                &mut config,
                1,
                &mut num_configs,
            );

            if num_configs == 0 || config.is_null() {
                return Err(format!("eglChooseConfig failed: {}", last_error()));
            }

            let context_attribs = [EGL_CONTEXT_CLIENT_VERSION, 3, EGL_NONE];
            let context = create_context(
                display,
                config,
                EGL_NO_CONTEXT,
                context_attribs.as_ptr(),
            );
            if context.is_null() {
                return Err(format!("eglCreateContext failed: {}", last_error()));
            }

            let surface_attribs = [
                EGL_WIDTH,
                width as c_int,
                EGL_HEIGHT,
                height as c_int,
                EGL_NONE,
            ];
            let surface = create_surface(display, config, surface_attribs.as_ptr());
            if surface.is_null() {
                return Err(format!(
                    "eglCreatePbufferSurface failed: {}",
                    last_error()
                ));
            }

            if make_current(display, surface, surface, context) == EGL_FALSE {
                return Err(format!("eglMakeCurrent failed: {}", last_error()));
            }

            Ok(EglHeadlessContext {
                display,
                context,
                surface,
                lib: None,
            })
        }
    }

    /// Load the OpenGL function pointers via glow using EGL's proc address.
    pub fn load_gl(&self) -> glow::Context {
        unsafe {
            glow::Context::from_loader_cstr(|name| {
                let get_proc: EglGetProcAddress =
                    std::mem::transmute(egl_proc("eglGetProcAddress"));
                get_proc(name.as_ptr()) as *const _
            })
        }
    }

    /// No-op: the context is already current on the creating thread.
    pub fn make_current(&self) {}

    /// Swap buffers (no-op for pbuffer surfaces).
    pub fn swap_buffers(&self) {
        unsafe {
            let swap: EglSwapBuffers =
                std::mem::transmute(egl_proc("eglSwapBuffers"));
            let _ = swap(self.display, self.surface);
        }
    }

    pub fn display(&self) -> *mut c_void {
        self.display
    }
}

impl Drop for EglHeadlessContext {
    fn drop(&mut self) {
        unsafe {
            let make_current: EglMakeCurrent =
                std::mem::transmute(egl_proc("eglMakeCurrent"));
            let _ = make_current(
                self.display,
                EGL_NO_SURFACE,
                EGL_NO_SURFACE,
                EGL_NO_CONTEXT,
            );
            if !self.surface.is_null() {
                let destroy_surface: EglDestroySurface =
                    std::mem::transmute(egl_proc("eglDestroySurface"));
                let _ = destroy_surface(self.display, self.surface);
            }
            if !self.context.is_null() {
                let destroy_context: EglDestroyContext =
                    std::mem::transmute(egl_proc("eglDestroyContext"));
                let _ = destroy_context(self.display, self.context);
            }
            let terminate: EglTerminate =
                std::mem::transmute(egl_proc("eglTerminate"));
            let _ = terminate(self.display);
        }
    }
}
