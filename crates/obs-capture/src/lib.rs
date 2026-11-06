//! obs-capture — Screen and audio capture implementations.
//!
//! Equivalent to OBS plugins:
//!   win-capture/     → ScreenCaptureSource (using `scrap`)
//!   win-wasapi/      → AudioCaptureSource  (using `cpal`)

pub mod audio;
pub mod screen;

pub use audio::AudioCaptureSource;
pub use screen::ScreenCaptureSource;
