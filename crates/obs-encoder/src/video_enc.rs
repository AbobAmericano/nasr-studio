//! H.264 video encoder using ffmpeg-next (libx264 codec).
//!
//! Equivalent to OBS `obs-x264.c`:
//!   x264_encode() → encode raw YUV frames → produce NAL packets
//!
//! Key mapping:
//!   obs_encoder_info::encode()  → H264Encoder::encode()
//!   obs_encoder_info::flush()   → H264Encoder::flush()

use anyhow::{Context, Result};
use ffmpeg_next as ffmpeg;
use ffmpeg_next::{
    codec, encoder, format, frame, software::scaling, Dictionary, Packet, Rational,
};
use obs_core::{
    traits::VideoEncoder,
    types::{EncodedPacket, StreamType, VideoFrame},
};
use tracing::{debug, info};

/// H.264 encoder wrapping FFmpeg's libx264.
pub struct H264Encoder {
    encoder: encoder::Video,
    scaler: scaling::Context,
    enc_width: u32,
    enc_height: u32,
    scaler_src_width: u32,
    scaler_src_height: u32,
    pts: i64,
    _timebase: Rational,
    extra_data: Option<Vec<u8>>,
}

// In our pipeline, H264Encoder is moved to a dedicated encoding worker thread
// and accessed exclusively from that thread.
unsafe impl Send for H264Encoder {}

impl H264Encoder {
    /// Create a new H.264 encoder.
    ///
    /// # Arguments
    /// * `width` / `height` — output frame size (must match compositor output)
    /// * `fps_num` / `fps_den` — frame rate as rational (e.g. 60/1)
    /// * `bitrate_kbps` — target bitrate in kilobits/sec
    pub fn new(
        width: u32,
        height: u32,
        fps_num: u32,
        fps_den: u32,
        bitrate_kbps: u32,
    ) -> Result<Self> {
        ffmpeg::init().context("Failed to init FFmpeg")?;

        let codec = encoder::find(codec::Id::H264).context("H264 codec not found")?;
        let context = codec::Context::new_with_codec(codec);
        let mut enc = context.encoder().video().context("Not a video encoder")?;

        // Configure encoder (matches OBS x264_handler in obs-x264.c)
        enc.set_width(width);
        enc.set_height(height);
        enc.set_format(format::Pixel::YUV420P);
        enc.set_time_base(Rational::new(fps_den as i32, fps_num as i32));
        enc.set_frame_rate(Some(Rational::new(fps_num as i32, fps_den as i32)));
        enc.set_bit_rate((bitrate_kbps as usize) * 1000);
        enc.set_max_bit_rate((bitrate_kbps as usize) * 1500);

        // x264 options (equivalent to OBS default preset "veryfast", tune "zerolatency")
        let mut opts = Dictionary::new();
        opts.set("preset", "veryfast");
        opts.set("tune", "zerolatency");
        opts.set("crf", "23");

        let encoder = enc.open_with(opts).context("Failed to open H264 encoder")?;

        // Scaler: RGBA → YUV420P (equivalent to libswscale in obs-video.c)
        let scaler = scaling::Context::get(
            format::Pixel::RGBA,
            width,
            height,
            format::Pixel::YUV420P,
            width,
            height,
            scaling::Flags::BILINEAR,
        )
        .context("Failed to create swscale context")?;

        let timebase = Rational::new(fps_den as i32, fps_num as i32);

        info!(
            "H264Encoder: {}x{} @ {}/{} fps, {}kbps",
            width, height, fps_num, fps_den, bitrate_kbps
        );

        Ok(Self {
            encoder,
            scaler,
            enc_width: width,
            enc_height: height,
            scaler_src_width: width,
            scaler_src_height: height,
            pts: 0,
            _timebase: timebase,
            extra_data: None,
        })
    }

    fn receive_packets(&mut self) -> Result<Vec<EncodedPacket>> {
        let mut packets = Vec::new();
        loop {
            let mut pkt = Packet::empty();
            match self.encoder.receive_packet(&mut pkt) {
                Ok(()) => {
                    debug!(
                        "H264Encoder: packet pts={}, size={}",
                        pkt.pts().unwrap_or(-1),
                        pkt.size()
                    );
                    packets.push(EncodedPacket {
                        data: pkt.data().unwrap_or(&[]).to_vec(),
                        pts: pkt.pts().unwrap_or(0),
                        dts: pkt.dts().unwrap_or(0),
                        is_keyframe: pkt.is_key(),
                        stream_type: StreamType::Video,
                    });
                }
                Err(ffmpeg::Error::Other { errno: ffmpeg::error::EAGAIN }) => break,
                Err(ffmpeg::Error::Eof) => break,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(packets)
    }
}

impl VideoEncoder for H264Encoder {
    fn encode(&mut self, frame: &VideoFrame) -> Result<Vec<EncodedPacket>> {
        // Recreate scaler if incoming frame dimensions changed (supports dynamic resolution / scaling)
        if self.scaler_src_width != frame.width || self.scaler_src_height != frame.height {
            self.scaler = scaling::Context::get(
                format::Pixel::RGBA,
                frame.width,
                frame.height,
                format::Pixel::YUV420P,
                self.enc_width,
                self.enc_height,
                scaling::Flags::BILINEAR,
            )
            .context("Failed to create swscale context for frame resize")?;
            self.scaler_src_width = frame.width;
            self.scaler_src_height = frame.height;
        }

        // Create a source frame in RGBA format
        let mut src_frame = frame::Video::new(
            format::Pixel::RGBA,
            frame.width,
            frame.height,
        );

        // Copy raw RGBA pixels into the FFmpeg frame
        let stride = frame.width as usize * 4;
        let dst_stride = src_frame.stride(0);
        let dst = src_frame.data_mut(0);
        for row in 0..frame.height as usize {
            let src_start = row * stride;
            let dst_start = row * dst_stride;
            if src_start + stride <= frame.data.len() {
                dst[dst_start..dst_start + stride]
                    .copy_from_slice(&frame.data[src_start..src_start + stride]);
            }
        }

        // Scale RGBA (frame.width x frame.height) → YUV420P (self.enc_width x self.enc_height)
        let mut yuv_frame = frame::Video::new(format::Pixel::YUV420P, self.enc_width, self.enc_height);
        self.scaler
            .run(&src_frame, &mut yuv_frame)
            .context("swscale RGBA→YUV420P failed")?;

        yuv_frame.set_pts(Some(self.pts));
        self.pts += 1;

        // Send frame to encoder
        self.encoder.send_frame(&yuv_frame).context("Failed to send frame to H264 encoder")?;

        self.receive_packets()
    }

    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        self.encoder.send_eof().context("Failed to send EOF to H264 encoder")?;
        self.receive_packets()
    }

    fn extra_data(&self) -> Option<&[u8]> {
        self.extra_data.as_deref()
    }
}
