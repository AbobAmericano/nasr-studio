//! Core data types shared across all obs-rs crates.
//!
//! Rust equivalents of libobs frame structs defined in:
//!   libobs/media-io/video-io.h
//!   libobs/media-io/audio-io.h

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Pixel formats (matches libobs video_format enum)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelFormat {
    /// RGBA 8-bit per channel (what scrap produces)
    Rgba,
    /// BGRA 8-bit per channel (Windows DXGI common output)
    Bgra,
    /// Planar YUV 4:2:0 (what H.264 encoders consume)
    I420,
    /// Semi-planar YUV 4:2:0 (NVENC-friendly)
    Nv12,
}

// ---------------------------------------------------------------------------
// Audio formats
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioFormat {
    /// 32-bit float, planar (what cpal produces natively)
    F32,
    /// 16-bit signed integer, interleaved (what many AAC encoders consume)
    S16,
}

// ---------------------------------------------------------------------------
// VideoFrame — equivalent to obs_video_frame / video_data in libobs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct VideoFrame {
    /// Raw pixel data wrapped in Arc for zero-copy cloning and low RAM footprint
    pub data: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// Presentation timestamp in nanoseconds (monotonic, matches libobs ns clock)
    pub pts_ns: u64,
    /// Stride in bytes (may be padded)
    pub stride: usize,
}

impl VideoFrame {
    pub fn new_rgba(data: Vec<u8>, width: u32, height: u32, pts_ns: u64) -> Self {
        let stride = width as usize * 4; // 4 bytes per RGBA pixel
        Self {
            data: Arc::new(data),
            width,
            height,
            format: PixelFormat::Rgba,
            pts_ns,
            stride,
        }
    }

    pub fn new_bgra(data: Vec<u8>, width: u32, height: u32, pts_ns: u64) -> Self {
        let stride = width as usize * 4;
        Self {
            data: Arc::new(data),
            width,
            height,
            format: PixelFormat::Bgra,
            pts_ns,
            stride,
        }
    }

    pub fn new_rgba_arc(data: Arc<Vec<u8>>, width: u32, height: u32, pts_ns: u64) -> Self {
        let stride = width as usize * 4;
        Self {
            data,
            width,
            height,
            format: PixelFormat::Rgba,
            pts_ns,
            stride,
        }
    }

    pub fn new_bgra_arc(data: Arc<Vec<u8>>, width: u32, height: u32, pts_ns: u64) -> Self {
        let stride = width as usize * 4;
        Self {
            data,
            width,
            height,
            format: PixelFormat::Bgra,
            pts_ns,
            stride,
        }
    }

    /// Convert BGRA → RGBA in-place (swaps R and B channels).
    /// libobs does this internally in obs-video.c before encoding.
    pub fn bgra_to_rgba(&mut self) {
        if self.format != PixelFormat::Bgra {
            return;
        }
        let data = Arc::make_mut(&mut self.data);
        for chunk in data.chunks_exact_mut(4) {
            chunk.swap(0, 2); // swap B and R
        }
        self.format = PixelFormat::Rgba;
    }
}

// ---------------------------------------------------------------------------
// AudioFrame — equivalent to audio_data in libobs/media-io/audio-io.h
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct AudioFrame {
    /// Interleaved f32 samples: [L0, R0, L1, R1, ...]
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
    pub format: AudioFormat,
    /// PTS in nanoseconds
    pub pts_ns: u64,
}

impl AudioFrame {
    pub fn new(samples: Vec<f32>, sample_rate: u32, channels: u16, pts_ns: u64) -> Self {
        Self {
            samples,
            sample_rate,
            channels,
            format: AudioFormat::F32,
            pts_ns,
        }
    }

    /// Number of frames (samples per channel)
    pub fn frame_count(&self) -> usize {
        self.samples.len() / self.channels as usize
    }

    /// Duration of this audio block
    pub fn duration(&self) -> Duration {
        Duration::from_nanos(
            (self.frame_count() as u64 * 1_000_000_000) / self.sample_rate as u64,
        )
    }
}

// ---------------------------------------------------------------------------
// CompositeFrame — output of the compositor (ready for encoding)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CompositeFrame {
    pub video: VideoFrame,
    pub pts_ns: u64,
}

// ---------------------------------------------------------------------------
// EncodedPacket — output of an encoder (ready for muxing)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct EncodedPacket {
    pub data: Vec<u8>,
    /// Presentation timestamp (in stream timebase units)
    pub pts: i64,
    /// Decode timestamp
    pub dts: i64,
    pub is_keyframe: bool,
    pub stream_type: StreamType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamType {
    Video,
    Audio,
}

// ---------------------------------------------------------------------------
// MediaPacket — unified channel type for the inter-thread pipeline
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum MediaPacket {
    Video(VideoFrame),
    Audio(AudioFrame),
    /// Signals shutdown to downstream workers
    Flush,
}

// ---------------------------------------------------------------------------
// PipelineConfig — equivalent to obs_video_info / obs_audio_info
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfig {
    // Video
    pub output_width: u32,
    pub output_height: u32,
    pub fps_num: u32,
    pub fps_den: u32,

    // Audio
    pub sample_rate: u32,
    pub channels: u16,

    // Encoding
    pub video_bitrate_kbps: u32,
    pub audio_bitrate_kbps: u32,
    pub output_path: String,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            output_width: 1920,
            output_height: 1080,
            fps_num: 60,
            fps_den: 1,
            sample_rate: 48000,
            channels: 2,
            video_bitrate_kbps: 6000,
            audio_bitrate_kbps: 192,
            output_path: "output.mp4".to_string(),
        }
    }
}

impl PipelineConfig {
    pub fn frame_duration_ns(&self) -> u64 {
        (self.fps_den as u64 * 1_000_000_000) / self.fps_num as u64
    }
}
