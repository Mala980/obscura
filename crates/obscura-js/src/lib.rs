pub mod cdp_watchdog;
pub mod frame;
mod import_map;
pub mod markdown;
pub mod module_loader;
pub mod ops;
pub mod runtime;
pub mod v8_flags;
mod write_stream;
#[cfg(feature = "webgl-gpu")]
pub mod webgl;
#[cfg(feature = "gstreamer")]
pub mod gstreamer;

pub use markdown::HTML_TO_MARKDOWN_JS;
pub use v8_flags::set_v8_flags;

#[cfg(feature = "render")]
pub use obscura_render::{
    screenshot_png, screenshot_png_scrolled, screenshot_png_scrolled_at_animation_time,
    screenshot_png_scrolled_at_animation_time_with_surface_color,
    validate_capture_region, AnimationSample, AnimationSampleMode, AnimationSampleTime,
    CaptureError, CaptureRegion, CssMediaType, ImageRequestProfile,
    MAX_CAPTURE_DIMENSION, MAX_CAPTURE_PIXELS,
};

#[cfg(feature = "gstreamer")]
pub use obscura_media::GStreamerPlayer;
