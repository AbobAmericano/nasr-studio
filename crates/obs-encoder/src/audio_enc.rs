//! AAC audio encoder using ffmpeg-next.
//!
//! Equivalent to OBS `obs-ffmpeg-audio-encoders.c` (aac_encoder).
//!
//! libobs audio block size: AUDIO_OUTPUT_FRAMES = 1024 samples.
//! AAC also expects 1024-sample frames — perfect alignment.
//!
//! Audio arrives from cpal as **interleaved f32** at the device's native
//! sample rate (typically 48 kHz on Windows WASAPI).  The AAC encoder wants
//! **planar f32** at 48 kHz.  When the rates match we perform a simple
//! interleaved→planar de-interleave in pure Rust, avoiding swresample entirely.

use anyhow::{Context, Result};
use ffmpeg_next as ffmpeg;
use ffmpeg_next::{codec, encoder, format, frame, Packet, Rational};
use obs_core::{
    traits::AudioEncoder,
    types::{AudioFrame, EncodedPacket, StreamType},
};
use tracing::info;

/// AAC audio encoder.
pub struct AacEncoder {
    encoder: encoder::Audio,
    pts: i64,
    frame_size: usize,
    /// Accumulation buffer for input samples (encoder expects exactly frame_size frames)
    accumulate: Vec<f32>,
    channels: u16,
}

impl AacEncoder {
    /// Create a new AAC encoder.
    ///
    /// # Arguments
    /// * `sample_rate` — sample rate of incoming AudioFrames (e.g. 48000 or 44100)
    /// * `channels` — channel count (1=mono, 2=stereo)
    /// * `bitrate_kbps` — AAC bitrate (e.g. 192 for 192kbps stereo)
    pub fn new(sample_rate: u32, channels: u16, bitrate_kbps: u32) -> Result<Self> {
        ffmpeg::init().context("Failed to init FFmpeg")?;

        let codec = encoder::find(codec::Id::AAC).context("AAC codec not found")?;
        let context = codec::Context::new_with_codec(codec);
        let mut enc = context.encoder().audio().context("Not an audio encoder")?;

        enc.set_rate(sample_rate as i32);
        enc.set_format(format::Sample::F32(format::sample::Type::Planar));
        enc.set_channel_layout(channel_layout(channels));
        enc.set_bit_rate((bitrate_kbps as usize) * 1000);
        enc.set_time_base(Rational::new(1, sample_rate as i32));

        let encoder = enc.open().context("Failed to open AAC encoder")?;
        let frame_size = encoder.frame_size() as usize;

        info!(
            "AacEncoder: {}Hz, {} channels, {}kbps, frame_size={}",
            sample_rate, channels, bitrate_kbps, frame_size
        );

        Ok(Self {
            encoder,
            pts: 0,
            frame_size,
            accumulate: Vec::new(),
            channels,
        })
    }

    fn receive_packets(&mut self) -> Result<Vec<EncodedPacket>> {
        let mut packets = Vec::new();
        loop {
            let mut pkt = Packet::empty();
            match self.encoder.receive_packet(&mut pkt) {
                Ok(()) => {
                    packets.push(EncodedPacket {
                        data: pkt.data().unwrap_or(&[]).to_vec(),
                        pts: pkt.pts().unwrap_or(0),
                        dts: pkt.dts().unwrap_or(0),
                        is_keyframe: false,
                        stream_type: StreamType::Audio,
                    });
                }
                Err(ffmpeg::Error::Other { errno: ffmpeg::error::EAGAIN }) => break,
                Err(ffmpeg::Error::Eof) => break,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(packets)
    }

    /// De-interleave `frame_size` interleaved samples into a planar `frame::Audio`.
    ///
    /// Input layout (interleaved stereo, frame_size=1024):
    ///   [L0 R0 L1 R1 L2 R2 ... L1023 R1023]   (2048 f32 values)
    ///
    /// Output layout (planar stereo):
    ///   plane 0: [L0 L1 L2 ... L1023]  (1024 f32 values, 4096 bytes)
    ///   plane 1: [R0 R1 R2 ... R1023]  (1024 f32 values, 4096 bytes)
    fn deinterleave_to_planar(&self, interleaved: &[f32]) -> frame::Audio {
        let ch = self.channels as usize;
        let ns = self.frame_size;

        let mut dst_frame = frame::Audio::new(
            format::Sample::F32(format::sample::Type::Planar),
            ns,
            channel_layout(self.channels),
        );

        // Set nb_samples explicitly (frame::Audio::new allocates but doesn't
        // always set this field when the format/layout match).
        unsafe {
            (*dst_frame.as_mut_ptr()).nb_samples = ns as i32;
        }

        for c in 0..ch {
            let plane = dst_frame.data_mut(c);
            for s in 0..ns {
                let sample = interleaved[s * ch + c];
                let bytes = sample.to_le_bytes();
                let offset = s * 4; // 4 bytes per f32
                if offset + 4 <= plane.len() {
                    plane[offset..offset + 4].copy_from_slice(&bytes);
                }
            }
        }

        dst_frame
    }

    fn encode_accumulated(&mut self) -> Result<Vec<EncodedPacket>> {
        let mut all_packets = Vec::new();
        let samples_per_frame = self.frame_size * self.channels as usize;

        while self.accumulate.len() >= samples_per_frame {
            let interleaved: Vec<f32> = self
                .accumulate
                .drain(..samples_per_frame)
                .collect();

            let mut dst_frame = self.deinterleave_to_planar(&interleaved);
            dst_frame.set_pts(Some(self.pts));
            self.pts += self.frame_size as i64;

            self.encoder
                .send_frame(&dst_frame)
                .context("Failed to send audio frame to AAC encoder")?;

            all_packets.extend(self.receive_packets()?);
        }

        Ok(all_packets)
    }
}

impl AudioEncoder for AacEncoder {
    fn encode(&mut self, audio_frame: &AudioFrame) -> Result<Vec<EncodedPacket>> {
        self.accumulate.extend_from_slice(&audio_frame.samples);
        self.encode_accumulated()
    }

    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        let needed = self.frame_size * self.channels as usize;
        if !self.accumulate.is_empty() && self.accumulate.len() < needed {
            self.accumulate.resize(needed, 0.0f32);
        }
        let mut packets = self.encode_accumulated()?;
        self.encoder.send_eof().context("Failed to send EOF to AAC encoder")?;
        packets.extend(self.receive_packets()?);
        Ok(packets)
    }

    fn frame_size(&self) -> usize {
        self.frame_size
    }
}

fn channel_layout(channels: u16) -> ffmpeg_next::ChannelLayout {
    match channels {
        1 => ffmpeg_next::ChannelLayout::MONO,
        2 => ffmpeg_next::ChannelLayout::STEREO,
        _ => ffmpeg_next::ChannelLayout::STEREO,
    }
}
