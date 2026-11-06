//! OBS pipeline orchestrator — wires capture, compositor, and encoder together.
//!
//! Equivalent to the high-level flow in libobs/obs.c: startup, thread launch,
//! and the crossbeam channels that connect the three worker threads.
//!
//! Thread layout (mirrors libobs threading model):
//!   capture_thread   → produces VideoFrame / AudioFrame
//!   compositor_thread (obs-compositor) → receives VideoFrames, composites, sends CompositeFrame
//!   encoder_thread   (obs-encoder) → encodes CompositeFrame + AudioFrame → EncodedPacket
//!   mux_thread       (obs-encoder) → writes EncodedPackets to file

use crate::types::{MediaPacket, PipelineConfig};
use crossbeam_channel::{bounded, Receiver, Sender};

/// Channel capacity constants.
/// libobs uses obs_video.num_buffered_video_frames (default 3 for triple-buffer).
/// We use slightly larger queues to absorb burst latency.
pub const VIDEO_CHANNEL_CAPACITY: usize = 8;
pub const AUDIO_CHANNEL_CAPACITY: usize = 32; // audio blocks are small

/// Handle to the inter-thread channels. Clones of these are handed to workers.
pub struct PipelineChannels {
    /// Capture thread sends into this; compositor receives from it.
    pub video_tx: Sender<MediaPacket>,
    pub video_rx: Receiver<MediaPacket>,

    /// Capture thread sends into this; encoder receives from it.
    pub audio_tx: Sender<MediaPacket>,
    pub audio_rx: Receiver<MediaPacket>,
}

impl PipelineChannels {
    pub fn new() -> Self {
        let (video_tx, video_rx) = bounded(VIDEO_CHANNEL_CAPACITY);
        let (audio_tx, audio_rx) = bounded(AUDIO_CHANNEL_CAPACITY);
        Self {
            video_tx,
            video_rx,
            audio_tx,
            audio_rx,
        }
    }
}

impl Default for PipelineChannels {
    fn default() -> Self {
        Self::new()
    }
}

/// Top-level pipeline coordinator.
///
/// Callers build this, attach sources and encoders (from obs-capture and
/// obs-encoder crates), then call `run()` which blocks until recording is done
/// or `stop()` is called from another thread.
pub struct OBSPipeline {
    pub config: PipelineConfig,
    pub channels: PipelineChannels,
}

impl OBSPipeline {
    pub fn new(config: PipelineConfig) -> Self {
        Self {
            channels: PipelineChannels::new(),
            config,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MediaPacket, VideoFrame};

    #[test]
    fn pipeline_channels_send_receive() {
        let ch = PipelineChannels::new();
        let frame = VideoFrame::new_rgba(vec![0u8; 4], 1, 1, 0);
        ch.video_tx.send(MediaPacket::Video(frame)).unwrap();
        match ch.video_rx.recv().unwrap() {
            MediaPacket::Video(f) => assert_eq!(f.width, 1),
            _ => panic!("wrong packet type"),
        }
    }

    #[test]
    fn flush_packet_propagates() {
        let ch = PipelineChannels::new();
        ch.video_tx.send(MediaPacket::Flush).unwrap();
        assert!(matches!(ch.video_rx.recv().unwrap(), MediaPacket::Flush));
    }
}
