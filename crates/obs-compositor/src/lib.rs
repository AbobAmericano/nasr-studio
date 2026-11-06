//! obs-compositor — wgpu-based GPU scene compositor.
//!
//! Equivalent to libobs/obs-video.c (obs_graphics_thread) + libobs/graphics/*.
//! Takes multiple VideoFrame inputs, uploads them as wgpu textures, runs a
//! WGSL composite shader, and reads back a CompositeFrame.

pub mod compositor;
pub mod scene;

pub use compositor::Compositor;
pub use scene::{BlendMode, SceneItem, Transform};
