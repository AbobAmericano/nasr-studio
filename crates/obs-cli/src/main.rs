//! obs-rs CLI — wires all crates into a working screen recorder.
//!
//! Usage:
//!   obs-rs --output output.mp4 --duration 10 --fps 60 --width 1920 --height 1080
//!
//! Thread layout (mirrors libobs):
//!   main thread       → orchestration, signal handling
//!   capture_video     → ScreenCaptureSource (scrap)
//!   capture_audio     → AudioCaptureSource (cpal, runs on cpal's internal thread)
//!   compositor_thread → Compositor (wgpu, headless GPU)
//!   encoder_thread    → H264Encoder + AacEncoder + Mp4Muxer

use anyhow::{Context, Result};
use clap::Parser;
use crossbeam_channel::{bounded, select};
use obs_capture::{AudioCaptureSource, ScreenCaptureSource};
use obs_compositor::{Compositor, SceneItem};
use obs_core::{
    traits::{AudioEncoder, Output, Source, VideoEncoder},
    types::{MediaPacket, PipelineConfig},
};
use obs_encoder::{AacEncoder, H264Encoder, Mp4Muxer};
use std::time::Duration;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "obs-rs", about = "Minimal OBS Studio re-implementation in Rust")]
struct Args {
    /// Output file path
    #[arg(short, long, default_value = "output.mp4")]
    output: String,

    /// Recording duration in seconds
    #[arg(short, long, default_value_t = 10)]
    duration: u64,

    /// Output width
    #[arg(long, default_value_t = 1920)]
    width: u32,

    /// Output height
    #[arg(long, default_value_t = 1080)]
    height: u32,

    /// Target FPS
    #[arg(long, default_value_t = 60)]
    fps: u32,

    /// Video bitrate in kbps
    #[arg(long, default_value_t = 6000)]
    video_bitrate: u32,

    /// Audio bitrate in kbps
    #[arg(long, default_value_t = 192)]
    audio_bitrate: u32,
}

fn main() -> Result<()> {
    // Initialize structured logging (equivalent to blog_init in libobs)
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let args = Args::parse();

    info!("obs-rs starting up");
    info!(
        "Config: {}x{} @ {}fps, output='{}'",
        args.width, args.height, args.fps, args.output
    );

    let config = PipelineConfig {
        output_width: args.width,
        output_height: args.height,
        fps_num: args.fps,
        fps_den: 1,
        sample_rate: 48000,
        channels: 2,
        video_bitrate_kbps: args.video_bitrate,
        audio_bitrate_kbps: args.audio_bitrate,
        output_path: args.output.clone(),
    };

    // --- Channel setup (equivalent to obs_video.output_buffer in libobs) ---
    // Bounded channels provide backpressure — if the encoder is slow, capture
    // blocks rather than accumulating unbounded memory (critical for real-time).
    let (video_tx, video_rx) = bounded::<MediaPacket>(8);
    let (audio_tx, audio_rx) = bounded::<MediaPacket>(32);

    // --- Initialize encoder and muxer ---
    // These are heavy (FFmpeg context init), so we do it before spawning threads.
    info!("Initializing H264 encoder...");
    let mut video_encoder = H264Encoder::new(
        config.output_width,
        config.output_height,
        config.fps_num,
        config.fps_den,
        config.video_bitrate_kbps,
    )
    .context("Failed to init H264 encoder")?;

    info!("Initializing AAC encoder...");
    let mut audio_encoder =
        AacEncoder::new(config.sample_rate, config.channels, config.audio_bitrate_kbps)
            .context("Failed to init AAC encoder")?;

    info!("Initializing MP4 muxer...");
    let mut muxer =
        Mp4Muxer::new(&config, video_encoder.extra_data()).context("Failed to init MP4 muxer")?;

    // --- Initialize wgpu compositor ---
    info!("Initializing wgpu compositor...");
    let compositor = Compositor::new(config.output_width, config.output_height)
        .context("Failed to init wgpu compositor")?;

    // --- Start capture threads ---
    info!("Starting screen capture...");
    let mut screen_source =
        ScreenCaptureSource::new(config.fps_num, config.fps_den)
            .context("Failed to start screen capture")?;

    info!("Starting audio capture...");
    let mut audio_source =
        AudioCaptureSource::new().context("Failed to start audio capture")?;

    // --- Compositor thread: receives VideoFrames, composites, sends to encoder ---
    let video_rx_comp = video_rx;
    let (composite_tx, composite_rx) = bounded::<MediaPacket>(4);

    let comp_thread = std::thread::Builder::new()
        .name("obs-compositor".into())
        .spawn(move || {
            info!("Compositor thread started");

            loop {
                match video_rx_comp.recv() {
                    Ok(MediaPacket::Video(capture_frame)) => {
                        let pts = capture_frame.pts_ns;

                        // Scene: direct screen capture layer (render pass clears to black)
                        let screen_item = SceneItem::new(0).with_alpha(1.0);
                        let layers = vec![(screen_item, &capture_frame)];

                        match compositor.composite(&layers, pts) {
                            Ok(composite_frame) => {
                                if composite_tx.send(MediaPacket::Video(composite_frame.video)).is_err() {
                                    break;
                                }
                            }
                            Err(e) => {
                                error!("Compositor error: {}", e);
                            }
                        }
                    }
                    Ok(MediaPacket::Flush) | Err(_) => {
                        info!("Compositor thread: flushing");
                        let _ = composite_tx.send(MediaPacket::Flush);
                        break;
                    }
                    Ok(MediaPacket::Audio(_)) => {} // ignored in compositor
                }
            }

            info!("Compositor thread exited");
        })
        .context("Failed to spawn compositor thread")?;

    // --- Encoder thread: receives CompositeFrames + AudioFrames → writes MP4 ---
    let enc_thread = std::thread::Builder::new()
        .name("obs-encoder".into())
        .spawn(move || {
            info!("Encoder thread started");
            let mut video_done = false;
            let mut audio_done = false;
            let never = crossbeam_channel::never();

            loop {
                if video_done && audio_done {
                    break;
                }

                let c_rx = if !video_done { &composite_rx } else { &never };
                let a_rx = if !audio_done { &audio_rx } else { &never };

                // Drain both channels without strict ordering (real interleaving
                // is handled by write_interleaved in FFmpeg).
                select! {
                    recv(c_rx) -> msg => {
                        match msg {
                            Ok(MediaPacket::Video(frame)) => {
                                match video_encoder.encode(&frame) {
                                    Ok(pkts) => {
                                        for pkt in pkts {
                                            if let Err(e) = muxer.write_video(&pkt) {
                                                error!("Muxer video write error: {}", e);
                                            }
                                        }
                                    }
                                    Err(e) => error!("H264 encode error: {}", e),
                                }
                            }
                            Ok(MediaPacket::Flush) | Err(_) => {
                                info!("Encoder: flushing video...");
                                match video_encoder.flush() {
                                    Ok(pkts) => {
                                        for pkt in pkts {
                                            let _ = muxer.write_video(&pkt);
                                        }
                                    }
                                    Err(e) => error!("H264 flush error: {}", e),
                                }
                                video_done = true;
                            }
                            Ok(MediaPacket::Audio(_)) => {}
                        }
                    },
                    recv(a_rx) -> msg => {
                        match msg {
                            Ok(MediaPacket::Audio(frame)) => {
                                match audio_encoder.encode(&frame) {
                                    Ok(pkts) => {
                                        for pkt in pkts {
                                            if let Err(e) = muxer.write_audio(&pkt) {
                                                error!("Muxer audio write error: {}", e);
                                            }
                                        }
                                    }
                                    Err(e) => error!("AAC encode error: {}", e),
                                }
                            }
                            Ok(MediaPacket::Flush) | Err(_) => {
                                info!("Encoder: flushing audio...");
                                match audio_encoder.flush() {
                                    Ok(pkts) => {
                                        for pkt in pkts {
                                            let _ = muxer.write_audio(&pkt);
                                        }
                                    }
                                    Err(e) => error!("AAC flush error: {}", e),
                                }
                                audio_done = true;
                            }
                            Ok(MediaPacket::Video(_)) => {}
                        }
                    }
                }
            }

            if let Err(e) = muxer.finalize() {
                error!("Muxer finalize error: {}", e);
            }

            info!("Encoder thread exited");
        })
        .context("Failed to spawn encoder thread")?;

    // --- Main loop: pump capture → channels ---
    info!("Recording for {}s...", args.duration);
    let deadline = std::time::Instant::now() + Duration::from_secs(args.duration);
    let mut clock = obs_core::FrameClock::new(config.fps_num, config.fps_den);

    while std::time::Instant::now() < deadline {
        clock.wait_for_next_frame();
        let pts = clock.current_pts_ns();

        // Tick sources (equivalent to obs_source_video_tick in libobs)
        screen_source.tick(clock.frame_duration().as_nanos() as u64);
        audio_source.tick(clock.frame_duration().as_nanos() as u64);

        // Pull video frame
        if let Some(mut vf) = screen_source.video_frame() {
            vf.pts_ns = pts;
            if let Err(e) = video_tx.try_send(MediaPacket::Video(vf)) {
                warn!("Video channel full, dropping frame: {}", e);
            }
        }

        // Pull audio frames (may produce multiple per video frame)
        while let Some(af) = audio_source.audio_frame() {
            if let Err(e) = audio_tx.try_send(MediaPacket::Audio(af)) {
                warn!("Audio channel full, dropping audio: {}", e);
                break;
            }
        }
    }

    // --- Shutdown sequence ---
    info!("Recording complete. Flushing pipeline...");

    // Signal compositor to flush
    let _ = video_tx.send(MediaPacket::Flush);
    let _ = audio_tx.send(MediaPacket::Flush);

    // Stop capture sources
    screen_source.stop();
    audio_source.stop();

    // Wait for compositor and encoder threads
    if let Err(e) = comp_thread.join() {
        error!("Compositor thread panicked: {:?}", e);
    }
    if let Err(e) = enc_thread.join() {
        error!("Encoder thread panicked: {:?}", e);
    }

    info!("obs-rs: recording saved to '{}'", args.output);
    Ok(())
}
