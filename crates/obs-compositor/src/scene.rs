//! Scene graph types — equivalent to obs_scene_t / obs_sceneitem_t in libobs.
//!
//! libobs scene items have a 2D transform matrix, blending mode, and visibility.
//! We replicate this with simple affine transforms and enum blend modes.

/// 2D affine transform for a scene item.
/// Equivalent to obs_transform_info in libobs/obs-scene.h
#[derive(Debug, Clone)]
pub struct Transform {
    /// Position of the item's top-left corner in output pixels
    pub x: f32,
    pub y: f32,
    /// Scale factors (1.0 = native size)
    pub scale_x: f32,
    pub scale_y: f32,
    /// Rotation in degrees (clockwise)
    pub rotation: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            rotation: 0.0,
        }
    }
}

/// Blend mode — equivalent to OBS_BLEND_* flags in libobs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendMode {
    /// Normal alpha compositing (Porter-Duff over)
    Normal,
    /// Additive blending
    Additive,
    /// Multiply blending
    Multiply,
}

impl Default for BlendMode {
    fn default() -> Self {
        BlendMode::Normal
    }
}

/// A single item in the scene — a source reference with display properties.
/// Equivalent to obs_scene_item in libobs/obs-scene.h
#[derive(Debug, Clone)]
pub struct SceneItem {
    /// Index into the compositor's source list
    pub source_index: usize,
    pub transform: Transform,
    pub blend_mode: BlendMode,
    pub visible: bool,
    /// Alpha multiplier (0.0–1.0)
    pub alpha: f32,
}

impl SceneItem {
    pub fn new(source_index: usize) -> Self {
        Self {
            source_index,
            transform: Transform::default(),
            blend_mode: BlendMode::Normal,
            visible: true,
            alpha: 1.0,
        }
    }

    pub fn with_transform(mut self, t: Transform) -> Self {
        self.transform = t;
        self
    }

    pub fn with_alpha(mut self, alpha: f32) -> Self {
        self.alpha = alpha.clamp(0.0, 1.0);
        self
    }
}
