//! obs-encoder — FFmpeg-backed H.264/AAC encoder and MP4 muxer.
//!
//! Equivalent to OBS plugins:
//!   obs-ffmpeg/obs-ffmpeg-mux.c    → Mp4Muxer
//!   obs-x264/obs-x264.c            → H264Encoder (uses libx264 via ffmpeg)
//!   obs-ffmpeg/obs-ffmpeg-audio-encoders.c → AacEncoder

pub mod audio_enc;
pub mod muxer;
pub mod video_enc;

pub use audio_enc::AacEncoder;
pub use muxer::Mp4Muxer;
pub use video_enc::H264Encoder;
