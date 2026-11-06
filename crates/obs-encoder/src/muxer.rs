//! MP4 muxer using ffmpeg-next (libavformat).
//!
//! Equivalent to OBS `obs-ffmpeg-mux.c` / ffmpeg-mux.c:
//!   ffmpeg_mux_init() → Mp4Muxer::new()
//!   ffmpeg_mux_packet() → Mp4Muxer::write_video() / write_audio()
//!   ffmpeg_mux_destroy() → Mp4Muxer::finalize()
//!
//! Interleaves H.264 video and AAC audio into an MP4 container.

use anyhow::{Context, Result};
use ffmpeg_next as ffmpeg;
use ffmpeg_next::{
    codec, encoder, format, Packet, Rational,
};
use obs_core::{
    traits::Output,
    types::{EncodedPacket, PipelineConfig},
};
use tracing::info;

/// MP4 file muxer.
///
/// Owns the FFmpeg output format context and manages two streams:
///   stream 0 → H.264 video
///   stream 1 → AAC audio
pub struct Mp4Muxer {
    octx: format::context::Output,
    video_stream_idx: usize,
    audio_stream_idx: usize,
    video_timebase: Rational,
    audio_timebase: Rational,
    finalized: bool,
}

impl Mp4Muxer {
    /// Create and open the output MP4 file.
    ///
    /// # Arguments
    /// * `config` — pipeline config (dimensions, fps, sample rate, output path)
    /// * `_video_extra_data` — H.264 SPS/PPS extradata (reserved for future use)
    pub fn new(config: &PipelineConfig, _video_extra_data: Option<&[u8]>) -> Result<Self> {
        ffmpeg::init().context("Failed to init FFmpeg")?;

        let mut octx = format::output(&config.output_path)
            .context(format!("Cannot open output file: {}", config.output_path))?;

        // --- Add video stream ---
        // Build a codec context, configure it, open it, then use the opened
        // encoder (which AsRef<codec::Context>) to copy codec parameters to
        // the stream via add_stream_with.
        //
        // Key: codec::Context::encoder() consumes self, so we chain all the
        // way through to the opened Encoder, then borrow it as &codec::Context.
        let video_codec = encoder::find(codec::Id::H264).context("H264 codec not found")?;
        let video_encoder = {
            let ctx = codec::Context::new_with_codec(video_codec);
            let mut enc = ctx.encoder().video().context("Not a video encoder")?;
            enc.set_width(config.output_width);
            enc.set_height(config.output_height);
            enc.set_format(format::Pixel::YUV420P);
            enc.set_time_base(Rational::new(config.fps_den as i32, config.fps_num as i32));
            enc.set_bit_rate(config.video_bitrate_kbps as usize * 1000);
            enc.open_as(video_codec).context("Failed to open video codec for muxer")?
        };
        // video_encoder: encoder::video::Encoder, which AsRef<codec::Context>
        let video_ctx_ref: &codec::Context = video_encoder.as_ref();
        let mut video_stream = octx.add_stream_with(video_ctx_ref).context("Failed to add video stream")?;
        let video_stream_idx = video_stream.index();
        let video_timebase = Rational::new(config.fps_den as i32, config.fps_num as i32);
        video_stream.set_time_base(video_timebase);

        // --- Add audio stream: same pattern ---
        let audio_codec = encoder::find(codec::Id::AAC).context("AAC codec not found")?;
        let audio_encoder = {
            let ctx = codec::Context::new_with_codec(audio_codec);
            let mut enc = ctx.encoder().audio().context("Not an audio encoder")?;
            enc.set_rate(config.sample_rate as i32);
            enc.set_format(format::Sample::F32(format::sample::Type::Planar));
            enc.set_channel_layout(match config.channels {
                1 => ffmpeg_next::ChannelLayout::MONO,
                _ => ffmpeg_next::ChannelLayout::STEREO,
            });
            enc.set_bit_rate(config.audio_bitrate_kbps as usize * 1000);
            enc.open_as(audio_codec).context("Failed to open audio codec for muxer")?
        };
        let audio_ctx_ref: &codec::Context = audio_encoder.as_ref();
        let mut audio_stream = octx.add_stream_with(audio_ctx_ref).context("Failed to add audio stream")?;
        let audio_stream_idx = audio_stream.index();
        let audio_timebase = Rational::new(1, config.sample_rate as i32);
        audio_stream.set_time_base(audio_timebase);

        // Write the MP4 file header (moov atom placeholder)
        format::context::output::dump(&octx, 0, Some(&config.output_path));
        octx.write_header().context("Failed to write MP4 header")?;

        info!(
            "Mp4Muxer: opened '{}', video stream {}, audio stream {}",
            config.output_path, video_stream_idx, audio_stream_idx
        );

        Ok(Self {
            octx,
            video_stream_idx,
            audio_stream_idx,
            video_timebase,
            audio_timebase,
            finalized: false,
        })
    }

    fn write_packet(&mut self, pkt: &EncodedPacket, stream_idx: usize, timebase: Rational) -> Result<()> {
        let mut ffpkt = Packet::copy(&pkt.data);
        ffpkt.set_pts(Some(pkt.pts));
        ffpkt.set_dts(Some(pkt.dts));
        ffpkt.set_stream(stream_idx);
        ffpkt.rescale_ts(timebase, self.octx.stream(stream_idx).unwrap().time_base());

        // Interleave automatically handles DTS ordering
        ffpkt.write_interleaved(&mut self.octx).context("Failed to write packet to MP4")?;
        Ok(())
    }
}

impl Output for Mp4Muxer {
    fn write_video(&mut self, pkt: &EncodedPacket) -> Result<()> {
        self.write_packet(pkt, self.video_stream_idx, self.video_timebase)
    }

    fn write_audio(&mut self, pkt: &EncodedPacket) -> Result<()> {
        self.write_packet(pkt, self.audio_stream_idx, self.audio_timebase)
    }

    fn finalize(&mut self) -> Result<()> {
        if !self.finalized {
            self.octx.write_trailer().context("Failed to write MP4 trailer")?;
            self.finalized = true;
            info!("Mp4Muxer: finalized output");
        }
        Ok(())
    }
}

impl Drop for Mp4Muxer {
    fn drop(&mut self) {
        if let Err(e) = self.finalize() {
            tracing::warn!("Mp4Muxer: error during drop finalization: {}", e);
        }
    }
}
