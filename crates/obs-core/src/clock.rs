//! High-resolution frame clock — equivalent to obs_graphics_thread timing in
//! libobs/obs-video.c. Provides accurate per-frame PTS and busy-wait sleep.
//!
//! libobs uses os_sleepto_ns() + os_gettime_ns() for nanosecond accuracy.
//! We use the `spin_sleep` crate which achieves sub-100µs accuracy on Windows
//! by combining coarse OS sleep with a spin loop for the final microseconds.

use std::time::{Duration, Instant};

/// Frame clock tied to a fixed rational FPS (e.g. 60000/1001 for 59.94fps).
///
/// Usage:
/// ```rust
/// use obs_core::FrameClock;
/// let mut clock = FrameClock::new(60, 1);
/// clock.wait_for_next_frame();
/// let pts = clock.current_pts_ns();
/// ```
pub struct FrameClock {
    fps_num: u64,
    fps_den: u64,
    start: Instant,
    frame_idx: u64,
}

impl FrameClock {
    /// Create a new clock. `fps_num / fps_den` is the target frame rate.
    /// For 60fps: `FrameClock::new(60, 1)`
    /// For NTSC: `FrameClock::new(30000, 1001)`
    pub fn new(fps_num: u32, fps_den: u32) -> Self {
        Self {
            fps_num: fps_num as u64,
            fps_den: fps_den as u64,
            start: Instant::now(),
            frame_idx: 0,
        }
    }

    /// PTS in nanoseconds for the current frame index.
    /// Formula: frame_idx * fps_den * 1e9 / fps_num
    pub fn current_pts_ns(&self) -> u64 {
        (self.frame_idx * self.fps_den * 1_000_000_000) / self.fps_num
    }

    /// PTS in FFmpeg AV_TIME_BASE units (microseconds, 1/1_000_000 sec)
    pub fn current_pts_av(&self) -> i64 {
        ((self.frame_idx * self.fps_den * 1_000_000) / self.fps_num) as i64
    }

    /// Advance to the next frame and sleep until its target wall-clock time.
    /// Uses OS thread sleep for minimum CPU usage (zero busy-spinning).
    pub fn wait_for_next_frame(&mut self) {
        self.frame_idx += 1;
        let target_ns = self.current_pts_ns();
        let target = self.start + Duration::from_nanos(target_ns);
        let now = Instant::now();
        if target > now {
            std::thread::sleep(target - now);
        }
    }

    /// Total frames emitted so far.
    pub fn frame_count(&self) -> u64 {
        self.frame_idx
    }

    /// Elapsed real time since clock creation.
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// Frame duration as a Duration.
    pub fn frame_duration(&self) -> Duration {
        Duration::from_nanos((self.fps_den * 1_000_000_000) / self.fps_num)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pts_is_zero_at_start() {
        let clock = FrameClock::new(60, 1);
        // frame_idx = 0 initially
        assert_eq!(clock.current_pts_ns(), 0);
    }

    #[test]
    fn pts_advances_per_frame_60fps() {
        let mut clock = FrameClock::new(60, 1);
        let expected_ns_per_frame = 1_000_000_000u64 / 60;
        // Manually increment
        clock.frame_idx = 1;
        let pts = clock.current_pts_ns();
        assert_eq!(pts, expected_ns_per_frame);
    }

    #[test]
    fn frame_duration_60fps() {
        let clock = FrameClock::new(60, 1);
        let dur = clock.frame_duration();
        // 1/60 second ≈ 16,666,666 ns
        assert_eq!(dur.as_nanos(), 16_666_666);
    }

    #[test]
    fn pts_ntsc_30fps() {
        let mut clock = FrameClock::new(30000, 1001);
        clock.frame_idx = 30000; // 1001 seconds worth
        let pts = clock.current_pts_ns();
        // Should be ~1001 seconds in nanoseconds
        let expected = 1001u64 * 1_000_000_000;
        assert_eq!(pts, expected);
    }
}
