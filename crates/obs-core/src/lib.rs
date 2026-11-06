//! obs-core — Core traits, types, pipeline orchestration and frame clock.
//!
//! This crate is the equivalent of `libobs/obs-internal.h` + `libobs/obs.c`.
//! It defines the fundamental abstractions that all other crates depend on.

pub mod clock;
pub mod pipeline;
pub mod traits;
pub mod types;

pub use clock::FrameClock;
pub use pipeline::OBSPipeline;
pub use traits::{AudioEncoder, Output, Source, VideoEncoder};
pub use types::{
    AudioFormat, AudioFrame, CompositeFrame, EncodedPacket, MediaPacket, PipelineConfig,
    PixelFormat, VideoFrame,
};
