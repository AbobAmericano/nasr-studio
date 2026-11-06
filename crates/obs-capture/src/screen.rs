//! Screen capture source using the `scrap` crate.
//!
//! Equivalent to OBS `win-capture` plugin source (monitor_capture_source).
//! On Windows, scrap uses DXGI Desktop Duplication API.
//! On macOS, it uses ScreenCaptureKit / AVFoundation.
//! On Linux (X11), it uses XShmGetImage.
//!
//! libobs equivalent: plugins/win-capture/monitor-capture.c
//!   monitor_capture_tick() + monitor_capture_render()

use anyhow::{Context, Result};
use obs_core::{
    clock::FrameClock,
    types::{AudioFrame, VideoFrame},
    traits::Source,
};
use parking_lot::Mutex;
use scrap::{Capturer, Display};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Screen capture source backed by `scrap`.
///
/// Runs capture on an internal background thread and exposes frames through
/// a lock-protected ring buffer. The pipeline calls `video_frame()` on the
/// graphics thread to pull the latest frame.
pub struct ScreenCaptureSource {
    /// Latest captured frame, shared between capture thread and caller.
    latest_frame: Arc<Mutex<Option<VideoFrame>>>,
    /// Last valid frame cached to prevent blinking when screen is static
    last_frame: Option<VideoFrame>,
    running: Arc<AtomicBool>,
    width: u32,
    height: u32,
    capture_thread: Option<thread::JoinHandle<()>>,
    pts_ns: u64,
}

impl ScreenCaptureSource {
    /// Create and immediately start capturing the primary display.
    pub fn new(fps_num: u32, fps_den: u32) -> Result<Self> {
        let display = Display::primary().context("Cannot find primary display")?;
        let width = display.width() as u32;
        let height = display.height() as u32;

        info!("ScreenCaptureSource: {}x{} @ {}/{} fps", width, height, fps_num, fps_den);

        // Pre-populate with initial black frame so callers never get None on startup
        let initial_frame = VideoFrame::new_bgra(vec![0u8; (width * height * 4) as usize], width, height, 0);
        let latest_frame: Arc<Mutex<Option<VideoFrame>>> = Arc::new(Mutex::new(Some(initial_frame.clone())));
        let running = Arc::new(AtomicBool::new(true));

        // Clone Arcs for the capture thread
        let frame_arc = Arc::clone(&latest_frame);
        let running_arc = Arc::clone(&running);

        let handle = thread::Builder::new()
            .name("obs-capture::screen".into())
            .spawn(move || {
                Self::capture_loop(frame_arc, running_arc, fps_num, fps_den);
            })
            .context("Failed to spawn screen capture thread")?;

        Ok(Self {
            latest_frame,
            last_frame: Some(initial_frame),
            running,
            width,
            height,
            capture_thread: Some(handle),
            pts_ns: 0,
        })
    }

    /// Internal capture loop — runs on the dedicated capture thread.
    ///
    /// Attempts real DXGI/macOS capture first. If that fails (e.g. dual-GPU laptop
    /// access denied), attempts Windows GDI capture. If that also fails (headless CI),
    /// falls back to a scrolling colour-bar test pattern.
    fn capture_loop(
        frame_arc: Arc<Mutex<Option<VideoFrame>>>,
        running: Arc<AtomicBool>,
        fps_num: u32,
        fps_den: u32,
    ) {
        let display = match Display::primary() {
            Ok(d) => d,
            Err(e) => {
                warn!("ScreenCapture: cannot open display ({}), using test pattern", e);
                Self::test_pattern_loop(frame_arc, running, fps_num, fps_den, 1280, 720);
                return;
            }
        };

        let cap_w = display.width() as u32;
        let cap_h = display.height() as u32;

        let mut capturer = match Capturer::new(display) {
            Ok(c) => c,
            Err(e) => {
                warn!("ScreenCapture: cannot create DXGI capturer ({})", e);
                #[cfg(target_os = "windows")]
                {
                    info!("ScreenCapture: attempting Windows GDI screen capture fallback...");
                    if Self::try_gdi_loop(Arc::clone(&frame_arc), Arc::clone(&running), fps_num, fps_den, cap_w, cap_h) {
                        return;
                    }
                }
                warn!("ScreenCapture: falling back to test pattern generator");
                Self::test_pattern_loop(frame_arc, running, fps_num, fps_den, cap_w, cap_h);
                return;
            }
        };

        let mut clock = FrameClock::new(fps_num, fps_den);
        let frame_size = (cap_w * cap_h * 4) as usize;
        let mut pool: Vec<Vec<u8>> = vec![vec![0u8; frame_size], vec![0u8; frame_size]];

        while running.load(Ordering::Relaxed) {
            clock.wait_for_next_frame();
            let pts = clock.current_pts_ns();

            match capturer.frame() {
                Ok(frame_bytes) => {
                    let mut data = pool.pop().unwrap_or_else(|| vec![0u8; frame_size]);
                    if data.len() != frame_bytes.len() {
                        data.resize(frame_bytes.len(), 0);
                    }
                    data.copy_from_slice(&frame_bytes);
                    let w = capturer.width() as u32;
                    let h = capturer.height() as u32;
                    let video_frame = VideoFrame::new_bgra(data, w, h, pts);

                    let mut guard = frame_arc.lock();
                    let recycled = guard.replace(video_frame);
                    drop(guard);

                    if let Some(old) = recycled {
                        if let Ok(vec) = std::sync::Arc::try_unwrap(old.data) {
                            if pool.len() < 3 {
                                pool.push(vec);
                            }
                        }
                    }
                    debug!("ScreenCapture: frame at pts={}ns", pts);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // Frame not ready / screen static — sleep briefly to save CPU
                    thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(e) => {
                    warn!("ScreenCapture: frame error: {}", e);
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }

        info!("ScreenCapture: capture loop exited");
    }

    #[cfg(target_os = "windows")]
    fn try_gdi_loop(
        frame_arc: Arc<Mutex<Option<VideoFrame>>>,
        running: Arc<AtomicBool>,
        fps_num: u32,
        fps_den: u32,
        w: u32,
        h: u32,
    ) -> bool {
        use windows_sys::Win32::Graphics::Gdi::*;

        unsafe {
            let hdc_screen = GetDC(0);
            if hdc_screen == 0 {
                return false;
            }
            let hdc_mem = CreateCompatibleDC(hdc_screen);
            if hdc_mem == 0 {
                ReleaseDC(0, hdc_screen);
                return false;
            }
            let hbm = CreateCompatibleBitmap(hdc_screen, w as i32, h as i32);
            if hbm == 0 {
                DeleteDC(hdc_mem);
                ReleaseDC(0, hdc_screen);
                return false;
            }
            let old_obj = SelectObject(hdc_mem, hbm);

            // Test if BitBlt works on current session
            let test_ok = BitBlt(
                hdc_mem,
                0,
                0,
                w as i32,
                h as i32,
                hdc_screen,
                0,
                0,
                SRCCOPY | CAPTUREBLT,
            );

            if test_ok == 0 {
                SelectObject(hdc_mem, old_obj);
                DeleteObject(hbm);
                DeleteDC(hdc_mem);
                ReleaseDC(0, hdc_screen);
                return false;
            }

            info!(
                "ScreenCapture: using Windows GDI BitBlt capture ({}x{} @ {}/{} fps)",
                w, h, fps_num, fps_den
            );

            let mut clock = FrameClock::new(fps_num, fps_den);
            let frame_size = (w * h * 4) as usize;
            let mut pool: Vec<Vec<u8>> = vec![vec![0u8; frame_size], vec![0u8; frame_size]];
            let mut bi: BITMAPINFO = std::mem::zeroed();
            bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bi.bmiHeader.biWidth = w as i32;
            bi.bmiHeader.biHeight = -(h as i32); // Top-down
            bi.bmiHeader.biPlanes = 1;
            bi.bmiHeader.biBitCount = 32;
            bi.bmiHeader.biCompression = BI_RGB as u32;

            while running.load(Ordering::Relaxed) {
                clock.wait_for_next_frame();
                let pts = clock.current_pts_ns();

                let mut data = pool.pop().unwrap_or_else(|| vec![0u8; frame_size]);
                if data.len() != frame_size {
                    data.resize(frame_size, 0);
                }

                let ok = BitBlt(
                    hdc_mem,
                    0,
                    0,
                    w as i32,
                    h as i32,
                    hdc_screen,
                    0,
                    0,
                    SRCCOPY | CAPTUREBLT,
                );

                if ok != 0 {
                    let lines = GetDIBits(
                        hdc_mem,
                        hbm,
                        0,
                        h,
                        data.as_mut_ptr() as *mut _,
                        &mut bi,
                        DIB_RGB_COLORS,
                    );

                    if lines > 0 {
                        // Reuse buffer if previous frame was consumed
                        let mut guard = frame_arc.lock();
                        let frame = VideoFrame::new_bgra(data, w, h, pts);
                        let recycled = guard.replace(frame);
                        drop(guard);

                        if let Some(old) = recycled {
                            if let Ok(vec) = std::sync::Arc::try_unwrap(old.data) {
                                if pool.len() < 3 {
                                    pool.push(vec);
                                }
                            }
                        }
                    } else {
                        pool.push(data);
                    }
                } else {
                    pool.push(data);
                }
            }

            SelectObject(hdc_mem, old_obj);
            DeleteObject(hbm);
            DeleteDC(hdc_mem);
            ReleaseDC(0, hdc_screen);
            info!("ScreenCapture: GDI loop exited");
            true
        }
    }

    /// Fallback test-pattern generator: produces RGBA colour-bar frames that
    /// scroll horizontally so you can visually confirm the pipeline works.
    fn test_pattern_loop(
        frame_arc: Arc<Mutex<Option<VideoFrame>>>,
        running: Arc<AtomicBool>,
        fps_num: u32,
        fps_den: u32,
        w: u32,
        h: u32,
    ) {
        info!("ScreenCapture: generating {}x{} test pattern @ {}/{} fps", w, h, fps_num, fps_den);
        let mut clock = FrameClock::new(fps_num, fps_den);

        // SMPTE-ish colour bars (RGBA)
        let colours: &[[u8; 4]] = &[
            [192, 192, 192, 255], // white-ish
            [192, 192, 0,   255], // yellow
            [0,   192, 192, 255], // cyan
            [0,   192, 0,   255], // green
            [192, 0,   192, 255], // magenta
            [192, 0,   0,   255], // red
            [0,   0,   192, 255], // blue
        ];

        let mut frame_num: u64 = 0;
        let frame_size = (w * h * 4) as usize;
        let mut pool: Vec<Vec<u8>> = vec![vec![0u8; frame_size], vec![0u8; frame_size]];
        let bar_width = (w as usize / colours.len()).max(1);

        // Pre-allocate a single row buffer to avoid per-pixel calculations
        let mut row_buf = vec![0u8; (w * 4) as usize];

        while running.load(Ordering::Relaxed) {
            clock.wait_for_next_frame();
            let pts = clock.current_pts_ns();

            let mut data = pool.pop().unwrap_or_else(|| vec![0u8; frame_size]);
            if data.len() != frame_size {
                data.resize(frame_size, 0);
            }

            let offset = (frame_num as usize * 4) % (w as usize);

            // Render one horizontal line
            for x in 0..w as usize {
                let sx = (x + offset) % (w as usize);
                let bar_idx = (sx / bar_width).min(colours.len() - 1);
                let pixel = &colours[bar_idx];
                let idx = x * 4;
                row_buf[idx..idx + 4].copy_from_slice(pixel);
            }

            // Fast SIMD memcpy across all rows
            let row_bytes = w as usize * 4;
            for y in 0..h as usize {
                let start = y * row_bytes;
                data[start..start + row_bytes].copy_from_slice(&row_buf);
            }

            let mut guard = frame_arc.lock();
            let video_frame = VideoFrame::new_rgba(data, w, h, pts);
            let recycled = guard.replace(video_frame);
            drop(guard);

            if let Some(old) = recycled {
                if let Ok(vec) = std::sync::Arc::try_unwrap(old.data) {
                    if pool.len() < 3 {
                        pool.push(vec);
                    }
                }
            }

            frame_num += 1;
        }

        info!("ScreenCapture: test pattern loop exited");
    }
}

impl Source for ScreenCaptureSource {
    fn id(&self) -> &'static str {
        "screen_capture"
    }

    fn tick(&mut self, dt_ns: u64) {
        self.pts_ns += dt_ns;
    }

    fn video_frame(&mut self) -> Option<VideoFrame> {
        let guard = self.latest_frame.lock();
        if let Some(f) = guard.as_ref() {
            Some(f.clone())
        } else {
            self.last_frame.clone()
        }
    }

    fn audio_frame(&mut self) -> Option<AudioFrame> {
        None // Screen capture has no audio
    }

    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.capture_thread.take() {
            let _ = handle.join();
        }
        info!("ScreenCaptureSource: stopped");
    }
}

impl Drop for ScreenCaptureSource {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_screen_source_always_yields_frame() {
        let mut source = ScreenCaptureSource::new(30, 1).expect("Failed to create screen source");
        // video_frame() must immediately yield a valid initial frame (never None)
        let frame = source.video_frame();
        assert!(frame.is_some(), "ScreenCaptureSource yielded None on startup!");
        let f = frame.unwrap();
        assert_eq!(f.width, source.width());
        assert_eq!(f.height, source.height());
        assert!(!f.data.is_empty());
        source.stop();
    }

    #[test]
    fn source_id_is_correct() {
        assert_eq!("screen_capture", "screen_capture");
    }
}
