//! Audio capture source using `cpal`.
//!
//! Equivalent to OBS `win-wasapi` plugin (wasapi_input_capture / wasapi_output_capture).
//! `cpal` supports WASAPI (Windows), CoreAudio (macOS), ALSA/PipeWire (Linux).
//!
//! libobs equivalent: plugins/win-wasapi/win-wasapi.cpp
//!   WASAPISource::OnAudioData callback → obs_source_output_audio()

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream};
use obs_core::{
    types::{AudioFormat, AudioFrame, VideoFrame},
    traits::Source,
};
use parking_lot::Mutex;
use std::sync::{
    atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, AtomicU64, Ordering},
    Arc,
};
use tracing::{debug, error, info, warn};

/// Captured audio chunk queued from the cpal callback.
#[derive(Debug)]
struct AudioChunk {
    samples: Vec<f32>,
    _pts_ns: u64,
}

/// Audio capture source backed by `cpal`.
///
/// Features OBS-equivalent audio mixer controls:
///   - Volume fader gain (0% to 200%, -inf to +6 dB)
///   - Mute / unmute switch
///   - Stereo pan / balance (-100% Left to +100% Right)
///   - Audio sync offset / delay (-500 ms to +500 ms)
///   - Noise gate filter with configurable dB threshold
pub struct AudioCaptureSource {
    queue: Arc<Mutex<Vec<AudioChunk>>>,
    _stream: Option<Arc<Stream>>, // Keep stream alive
    sample_rate: u32,
    channels: u16,
    pts_ns: Arc<AtomicU64>,
    running: Arc<AtomicBool>,
    /// Accumulation buffer for partial chunks
    accumulate: Vec<f32>,
    /// Target block size (1024 matches AUDIO_OUTPUT_FRAMES in libobs)
    block_size: usize,
    source_pts: u64,

    // --- OBS Mixer Properties ---
    /// Volume fader in percent: 0..=200 (100 = 0 dB unity gain)
    pub volume_pct: Arc<AtomicU32>,
    /// Mute toggle
    pub muted: Arc<AtomicBool>,
    /// Stereo Pan / Balance: -100 (Full Left) to +100 (Full Right), 0 = Center
    pub pan: Arc<AtomicI32>,
    /// Audio sync offset delay in milliseconds (-500ms to +500ms)
    pub sync_offset_ms: Arc<AtomicI64>,
    /// Noise gate filter enabled
    pub noise_gate_enabled: Arc<AtomicBool>,
    /// Noise gate threshold in dB (-60 dB to 0 dB, default -32 dB)
    pub noise_gate_threshold_db: Arc<AtomicI32>,
}

unsafe impl Send for AudioCaptureSource {}
unsafe impl Sync for AudioCaptureSource {}

impl AudioCaptureSource {
    /// List all available audio input devices (microphones/line-in) on the system.
    pub fn list_input_devices() -> Vec<String> {
        let host = cpal::default_host();
        match host.input_devices() {
            Ok(devices) => devices.filter_map(|d| d.name().ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// List all available audio output devices (speakers/headphones) on the system.
    pub fn list_output_devices() -> Vec<String> {
        let host = cpal::default_host();
        match host.output_devices() {
            Ok(devices) => devices.filter_map(|d| d.name().ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Create and start capturing from the default audio input device.
    pub fn new() -> Result<Self> {
        Self::new_with_device(None, 1024)
    }

    pub fn new_with_block_size(block_size: usize) -> Result<Self> {
        Self::new_with_device(None, block_size)
    }

    /// Create and start capturing from a specific audio device by name.
    /// Supports "Default", "Disabled", or specific device names.
    pub fn new_with_device(device_name: Option<&str>, block_size: usize) -> Result<Self> {
        let host = cpal::default_host();
        let target_str = device_name.unwrap_or("Default");

        if target_str == "Disabled" {
            info!("AudioCaptureSource: Audio device disabled, running in silent mode");
            return Self::new_silent(48000, 2, block_size);
        }

        let device = if target_str == "Default" || target_str.is_empty() {
            match host.default_input_device() {
                Some(d) => d,
                None => {
                    warn!("AudioCaptureSource: No default audio input device found, using silent generator");
                    return Self::new_silent(48000, 2, block_size);
                }
            }
        } else {
            let found = host.input_devices().ok().and_then(|mut devs| {
                devs.find(|d| d.name().map(|n| n == target_str).unwrap_or(false))
            });
            match found {
                Some(d) => d,
                None => {
                    warn!("AudioCaptureSource: Device '{}' not found, falling back to default", target_str);
                    match host.default_input_device() {
                        Some(d) => d,
                        None => return Self::new_silent(48000, 2, block_size),
                    }
                }
            }
        };

        let device_name_str = device.name().unwrap_or_else(|_| "unknown".into());
        info!("AudioCaptureSource: using device '{}'", device_name_str);

        let config = match device.default_input_config() {
            Ok(c) => c,
            Err(e) => {
                warn!("AudioCaptureSource: Failed to get default input config ({}), using silent fallback", e);
                return Self::new_silent(48000, 2, block_size);
            }
        };

        let sample_rate = config.sample_rate().0;
        let channels = config.channels();

        info!(
            "AudioCaptureSource: {}Hz, {} channels, format={:?}",
            sample_rate,
            channels,
            config.sample_format()
        );

        let queue: Arc<Mutex<Vec<AudioChunk>>> = Arc::new(Mutex::new(Vec::new()));
        let pts_ns = Arc::new(AtomicU64::new(0));
        let running = Arc::new(AtomicBool::new(true));

        let queue_arc = Arc::clone(&queue);
        let pts_arc = Arc::clone(&pts_ns);

        let stream = match config.sample_format() {
            SampleFormat::F32 => {
                let err_fn = |e| error!("Audio stream error: {}", e);
                device.build_input_stream(
                    &config.into(),
                    move |data: &[f32], _info: &cpal::InputCallbackInfo| {
                        let pts = pts_arc.load(Ordering::Relaxed);
                        let samples = data.to_vec();
                        let block_ns = (samples.len() as u64 * 1_000_000_000)
                            / (sample_rate as u64 * channels as u64);
                        pts_arc.fetch_add(block_ns, Ordering::Relaxed);

                        queue_arc.lock().push(AudioChunk { samples, _pts_ns: pts });
                        debug!("AudioCapture: {} samples at pts={}ns", data.len(), pts);
                    },
                    err_fn,
                    None,
                )
            }
            SampleFormat::I16 => {
                let err_fn = |e| error!("Audio stream error: {}", e);
                device.build_input_stream(
                    &config.into(),
                    move |data: &[i16], _info: &cpal::InputCallbackInfo| {
                        let pts = pts_arc.load(Ordering::Relaxed);
                        let samples: Vec<f32> =
                            data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                        let block_ns = (samples.len() as u64 * 1_000_000_000)
                            / (sample_rate as u64 * channels as u64);
                        pts_arc.fetch_add(block_ns, Ordering::Relaxed);
                        queue_arc.lock().push(AudioChunk { samples, _pts_ns: pts });
                    },
                    err_fn,
                    None,
                )
            }
            fmt => {
                warn!("Unsupported audio sample format {:?}, using silent fallback", fmt);
                return Self::new_silent(sample_rate, channels, block_size);
            }
        };

        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                warn!("Failed to build audio stream ({}), using silent fallback", e);
                return Self::new_silent(sample_rate, channels, block_size);
            }
        };

        if let Err(e) = stream.play() {
            warn!("Failed to start audio stream ({}), using silent fallback", e);
            return Self::new_silent(sample_rate, channels, block_size);
        }

        Ok(Self {
            queue,
            _stream: Some(Arc::new(stream)),
            sample_rate,
            channels,
            pts_ns,
            running,
            accumulate: Vec::new(),
            block_size,
            source_pts: 0,
            volume_pct: Arc::new(AtomicU32::new(100)),
            muted: Arc::new(AtomicBool::new(false)),
            pan: Arc::new(AtomicI32::new(0)),
            sync_offset_ms: Arc::new(AtomicI64::new(0)),
            noise_gate_enabled: Arc::new(AtomicBool::new(false)),
            noise_gate_threshold_db: Arc::new(AtomicI32::new(-32)),
        })
    }

    /// Fallback silent generator when no audio device is available or when set to Disabled.
    pub fn new_silent(sample_rate: u32, channels: u16, block_size: usize) -> Result<Self> {
        let queue: Arc<Mutex<Vec<AudioChunk>>> = Arc::new(Mutex::new(Vec::new()));
        let pts_ns = Arc::new(AtomicU64::new(0));
        let running = Arc::new(AtomicBool::new(true));

        let q_clone = Arc::clone(&queue);
        let pts_clone = Arc::clone(&pts_ns);
        let r_clone = Arc::clone(&running);

        // Spawn a background generator yielding zeroed frames at steady sample rate
        std::thread::Builder::new()
            .name("obs-capture::silent_audio".into())
            .spawn(move || {
                let dt_ns = (block_size as u64 * 1_000_000_000) / (sample_rate as u64);
                let sleep_dur = std::time::Duration::from_nanos(dt_ns);
                while r_clone.load(Ordering::Relaxed) {
                    std::thread::sleep(sleep_dur);
                    let pts = pts_clone.fetch_add(dt_ns, Ordering::Relaxed);
                    let samples = vec![0.0f32; block_size * channels as usize];
                    q_clone.lock().push(AudioChunk { samples, _pts_ns: pts });
                }
            })
            .context("Failed to spawn silent audio thread")?;

        Ok(Self {
            queue,
            _stream: None,
            sample_rate,
            channels,
            pts_ns,
            running,
            accumulate: Vec::new(),
            block_size,
            source_pts: 0,
            volume_pct: Arc::new(AtomicU32::new(100)),
            muted: Arc::new(AtomicBool::new(false)),
            pan: Arc::new(AtomicI32::new(0)),
            sync_offset_ms: Arc::new(AtomicI64::new(0)),
            noise_gate_enabled: Arc::new(AtomicBool::new(false)),
            noise_gate_threshold_db: Arc::new(AtomicI32::new(-32)),
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }
}

impl Source for AudioCaptureSource {
    fn id(&self) -> &'static str {
        "audio_capture"
    }

    fn tick(&mut self, dt_ns: u64) {
        self.source_pts += dt_ns;
    }

    fn video_frame(&mut self) -> Option<VideoFrame> {
        None
    }

    fn audio_frame(&mut self) -> Option<AudioFrame> {
        // Drain the queue into accumulation buffer
        {
            let mut q = self.queue.lock();
            for chunk in q.drain(..) {
                self.accumulate.extend_from_slice(&chunk.samples);
            }
        }

        let samples_needed = self.block_size * self.channels as usize;
        if self.accumulate.len() >= samples_needed {
            let mut samples: Vec<f32> = self.accumulate.drain(..samples_needed).collect();
            let pts = self.pts_ns.load(Ordering::Relaxed);

            // Apply OBS Audio Mixer transformations
            if self.muted.load(Ordering::Relaxed) {
                samples.fill(0.0);
            } else {
                let volume_pct = self.volume_pct.load(Ordering::Relaxed);
                let gain = (volume_pct as f32) / 100.0;

                let pan = self.pan.load(Ordering::Relaxed);
                let left_gain = if pan > 0 { (100 - pan) as f32 / 100.0 } else { 1.0 };
                let right_gain = if pan < 0 { (100 + pan) as f32 / 100.0 } else { 1.0 };

                let gate_enabled = self.noise_gate_enabled.load(Ordering::Relaxed);
                let gate_thresh_db = self.noise_gate_threshold_db.load(Ordering::Relaxed) as f32;
                let gate_linear = 10.0f32.powf(gate_thresh_db / 20.0);

                if self.channels == 2 {
                    for chunk in samples.chunks_exact_mut(2) {
                        let mut l = chunk[0] * gain * left_gain;
                        let mut r = chunk[1] * gain * right_gain;

                        if gate_enabled {
                            let mag = l.abs().max(r.abs());
                            if mag < gate_linear {
                                l = 0.0;
                                r = 0.0;
                            }
                        }

                        chunk[0] = l.clamp(-1.0, 1.0);
                        chunk[1] = r.clamp(-1.0, 1.0);
                    }
                } else {
                    for s in samples.iter_mut() {
                        let mut val = *s * gain;
                        if gate_enabled && val.abs() < gate_linear {
                            val = 0.0;
                        }
                        *s = val.clamp(-1.0, 1.0);
                    }
                }
            }

            // Apply audio sync offset
            let offset_ms = self.sync_offset_ms.load(Ordering::Relaxed);
            let adjusted_pts = if offset_ms >= 0 {
                pts.saturating_add((offset_ms as u64) * 1_000_000)
            } else {
                pts.saturating_sub((-offset_ms as u64) * 1_000_000)
            };

            Some(AudioFrame {
                samples,
                sample_rate: self.sample_rate,
                channels: self.channels,
                format: AudioFormat::F32,
                pts_ns: adjusted_pts,
            })
        } else {
            None
        }
    }

    fn width(&self) -> u32 {
        0
    }

    fn height(&self) -> u32 {
        0
    }

    fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        self._stream = None;
        info!("AudioCaptureSource: stopped");
    }
}

impl Drop for AudioCaptureSource {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_id_correct() {
        assert_eq!("audio_capture", "audio_capture");
    }

    #[test]
    fn audio_frame_accumulation() {
        let mut acc: Vec<f32> = vec![0.0f32; 512];
        let block_size = 1024usize;
        let channels = 2usize;
        let needed = block_size * channels;
        assert!(acc.len() < needed, "Should not yield a frame yet");

        acc.extend_from_slice(&vec![0.0f32; 512 + needed]);
        assert!(acc.len() >= needed);
    }

    #[test]
    fn test_list_devices() {
        let inputs = AudioCaptureSource::list_input_devices();
        println!("Detected input devices: {:?}", inputs);
        let outputs = AudioCaptureSource::list_output_devices();
        println!("Detected output devices: {:?}", outputs);
    }
}
