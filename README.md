# nasr studio (nsr studio)

A rebuilt, ultra-lightweight harness and reimplementation of OBS Studio written in pure Rust.

Designed for high performance, zero-copy buffer pipelines, and minimal system resource footprint.

## Screenshots

### Main Studio Interface
![Main Studio Interface](docs/screenshots/main-interface.jpg)

### Settings Dialogs

| Video Settings | Audio Settings |
| :---: | :---: |
| ![Video Settings](docs/screenshots/video-settings.png) | ![Audio Settings](docs/screenshots/audio-settings.png) |

## Features

- **Ultra-Lightweight Resource Footprint**:
  - **RAM**: < 40 MB active working set (compared to 350+ MB standard OBS)
  - **CPU**: < 1.5% idle preview overhead across multi-core systems
- **Display & Window Capture**:
  - Low-overhead DXGI Desktop Duplication & GDI BitBlt capture fallbacks
  - Zero-copy buffer recycling via `Arc<Vec<u8>>` bounded pool
- **Studio Audio Mixer & Engine**:
  - Multi-channel input/output capture via CPAL
  - Real-time stereo panning, volume gain (0%–200%), software noise gate, and sync offset delay
  - Real-time decibel audio meters
- **Video Compositing & GPU Acceleration**:
  - `wgpu` (DirectX / Vulkan) multi-source compositing pipeline
  - High-efficiency downsampler for real-time GUI preview
- **Hardware-Accelerated Encoding**:
  - H.264 video encoding via FFmpeg `libx264`
  - AAC stereo audio encoding
  - MP4 container muxing with monotonic presentation timestamps (PTS)

## Architecture

The project is structured as a modular Cargo workspace:

- [`crates/obs-core`](crates/obs-core): Core frame primitives (`VideoFrame`, `AudioFrame`), nanosecond `FrameClock`, and synchronization traits.
- [`crates/obs-capture`](crates/obs-capture): Screen capture (DXGI / Win32 GDI) and audio input capture (CPAL / WASAPI).
- [`crates/obs-compositor`](crates/obs-compositor): GPU-accelerated multi-source scene compositor and WGSL shaders.
- [`crates/obs-encoder`](crates/obs-encoder): H.264 video encoder, AAC audio encoder, and MP4 muxer.
- [`crates/obs-cli`](crates/obs-cli): Headless command-line recorder and testing utility.
- [`crates/obs-ui`](crates/obs-ui): GUI desktop application built with `eframe` and `egui`.

## Getting Started

### Prerequisites

- Rust 1.85+ (Edition 2024 support)
- FFmpeg shared libraries (libavcodec, libavformat, libavutil, libswscale)
- LLVM / Clang (for bindgen)

### Building & Running

To build in release mode:

```bash
cargo build --release -p nasr-studio
```

To run the GUI:

```bash
# Windows launcher
.\run-nasr.bat

# Or directly with Cargo
cargo run --release -p nasr-studio
```

## Authors & Contributors

- **AbobAmericano** ([abobamericano@gmail.com](mailto:abobamericano@gmail.com)) — Lead Author & Developer
- **Antigravity** — AI Pair Programmer & Performance Architect

## License

This project is licensed under the **GNU General Public License v3.0 or later** ([GPL-3.0-or-later](LICENSE)).
