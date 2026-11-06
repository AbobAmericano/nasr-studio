//! Trait definitions for the obs-rs plugin interface.
//!
//! These are idiomatic Rust equivalents of the libobs vtable structs:
//!   obs_source_info     → Source
//!   obs_encoder_info    → VideoEncoder / AudioEncoder
//!   obs_output_info     → Output

use crate::types::{AudioFrame, EncodedPacket, VideoFrame};
use anyhow::Result;

// ---------------------------------------------------------------------------
// Source trait — equivalent to obs_source_info + obs_source_t lifecycle
// ---------------------------------------------------------------------------

/// A source of raw media frames (video, audio, or both).
///
/// Implementors include screen capture, webcam input, media file playback,
/// and scene composites. This matches the `obs_source_info` callback table
/// in libobs with `create`, `destroy`, `video_tick`, `video_render`,
/// `get_width`, `get_height`.
pub trait Source: Send + Sync {
    /// Unique identifier (matches obs_source_info::id)
    fn id(&self) -> &'static str;

    /// Called once per frame by the pipeline. Implementations should do any
    /// state updates here (matches obs_source_info::video_tick).
    fn tick(&mut self, dt_ns: u64);

    /// Pull the next video frame if available (synchronous sources).
    /// Returns None if no new frame is ready (matches async source behaviour).
    fn video_frame(&mut self) -> Option<VideoFrame>;

    /// Pull the next audio chunk if available.
    fn audio_frame(&mut self) -> Option<AudioFrame>;

    /// Native capture width in pixels.
    fn width(&self) -> u32;

    /// Native capture height in pixels.
    fn height(&self) -> u32;

    /// Called on shutdown. Free any OS/GPU resources here.
    fn stop(&mut self);
}

// ---------------------------------------------------------------------------
// VideoEncoder trait — equivalent to obs_encoder_info (OBS_ENCODER_VIDEO)
// ---------------------------------------------------------------------------

/// Consumes raw VideoFrames and produces encoded bitstream packets.
///
/// Implementations wrap FFmpeg libavcodec encoders (x264, NVENC, etc.).
/// Matches the `encode` and `get_extra_data` callbacks in obs_encoder_info.
pub trait VideoEncoder: Send {
    /// Feed one raw frame into the encoder. May return 0 or more packets
    /// (encoders have internal latency / B-frame reorder buffers).
    fn encode(&mut self, frame: &VideoFrame) -> Result<Vec<EncodedPacket>>;

    /// Drain the encoder after all frames have been submitted.
    fn flush(&mut self) -> Result<Vec<EncodedPacket>>;

    /// Codec extradata (SPS/PPS for H.264, etc.) — matches get_extra_data.
    fn extra_data(&self) -> Option<&[u8]>;
}

// ---------------------------------------------------------------------------
// AudioEncoder trait — equivalent to obs_encoder_info (OBS_ENCODER_AUDIO)
// ---------------------------------------------------------------------------

/// Consumes raw AudioFrames and produces encoded audio packets.
pub trait AudioEncoder: Send {
    fn encode(&mut self, frame: &AudioFrame) -> Result<Vec<EncodedPacket>>;
    fn flush(&mut self) -> Result<Vec<EncodedPacket>>;
    /// Frame size expected by this encoder (e.g. 1024 samples for AAC)
    fn frame_size(&self) -> usize;
}

// ---------------------------------------------------------------------------
// Output trait — equivalent to obs_output_info + obs_output_t
// ---------------------------------------------------------------------------

/// Consumes encoded packets and writes them to a file or network stream.
///
/// Matches the `raw_video`, `raw_audio`, and `stop` callbacks in
/// obs_output_info. The muxer handles PTS interleaving and container format.
pub trait Output: Send {
    /// Write a video packet to the output.
    fn write_video(&mut self, pkt: &EncodedPacket) -> Result<()>;

    /// Write an audio packet to the output.
    fn write_audio(&mut self, pkt: &EncodedPacket) -> Result<()>;

    /// Flush and finalize the output (write container trailer).
    fn finalize(&mut self) -> Result<()>;
}
