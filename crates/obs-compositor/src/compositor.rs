//! GPU scene compositor using wgpu.
//!
//! Equivalent to libobs/obs-video.c `obs_graphics_thread` + libobs/graphics/
//!
//! Architecture:
//! 1. Initialize wgpu device (async, then block with pollster)
//! 2. Create a render target texture (RGBA8) at the output resolution
//! 3. For each scene item, upload VideoFrame as a wgpu Texture
//! 4. Run composite shader (composite.wgsl) for each layer
//! 5. Copy render target to CPU-readable buffer
//! 6. Return CompositeFrame to the encoder thread
//!
//! This is a headless compositor — no window/surface is needed.
//! For preview, a winit window can be attached to display the render target.

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use obs_core::types::{CompositeFrame, PixelFormat, VideoFrame};
use parking_lot::Mutex;
use tracing::{debug, info};

use crate::scene::SceneItem;

// WGSL shader source embedded at compile time.
const COMPOSITE_SHADER: &str = include_str!("shaders/composite.wgsl");

/// Per-layer uniform data sent to the shader.
/// Must match the `LayerUniforms` struct in composite.wgsl.
/// 16-byte aligned (required by wgpu uniform buffers).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct LayerUniforms {
    uv_rect: [f32; 4], // [u_offset, v_offset, u_scale, v_scale]
    params: [f32; 4],  // [alpha, is_bgra, 0.0, 0.0]
}

/// Cached GPU resources for a compositing layer to eliminate per-frame allocations.
struct CachedLayer {
    width: u32,
    height: u32,
    texture: wgpu::Texture,
    uniform_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

/// The main compositor. Owns the wgpu device and rendering resources.
pub struct Compositor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    render_pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    bind_group_layout: wgpu::BindGroupLayout,
    /// Output render target (RGBA8Unorm)
    render_target: wgpu::Texture,
    render_target_view: wgpu::TextureView,
    /// Staging buffer for GPU → CPU readback
    output_buffer: wgpu::Buffer,
    output_width: u32,
    output_height: u32,
    /// Reusable GPU layer resources (zero-allocation rendering loop)
    layer_cache: Mutex<Vec<CachedLayer>>,
    /// Pre-allocated readback buffer to avoid per-frame CPU heap churn
    readback_buffer: Mutex<Vec<u8>>,
}

impl Compositor {
    /// Create the compositor. Blocks on wgpu adapter/device initialization.
    pub fn new(output_width: u32, output_height: u32) -> Result<Self> {
        pollster::block_on(Self::init_async(output_width, output_height))
    }

    async fn init_async(output_width: u32, output_height: u32) -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        // Request adapter — prefer discrete GPU, fall back to integrated / software
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None, // headless
                force_fallback_adapter: false,
            })
            .await
            .context("Failed to find a wgpu adapter")?;

        info!(
            "wgpu adapter: {} ({:?})",
            adapter.get_info().name,
            adapter.get_info().backend
        );

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("obs-compositor"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    memory_hints: Default::default(),
                },
                None,
            )
            .await
            .context("Failed to create wgpu device")?;

        // --- Shader module ---
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("composite_shader"),
            source: wgpu::ShaderSource::Wgsl(COMPOSITE_SHADER.into()),
        });

        // --- Bind group layout ---
        // Bindings: 0=texture, 1=sampler, 2=uniform
        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("composite_bgl"),
                entries: &[
                    // Binding 0: source texture
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    // Binding 1: sampler
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    // Binding 2: layer uniforms
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("composite_pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        // --- Render pipeline ---
        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("composite_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[], // Full-screen triangle, no vertex buffer
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // --- Sampler (bilinear, clamp-to-edge) ---
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("composite_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // --- Render target texture ---
        let render_target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("render_target"),
            size: wgpu::Extent3d {
                width: output_width,
                height: output_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let render_target_view =
            render_target.create_view(&wgpu::TextureViewDescriptor::default());

        // --- Output staging buffer (GPU → CPU) ---
        // wgpu requires rows to be padded to COPY_BYTES_PER_ROW_ALIGNMENT.
        let padded_row = padded_bytes_per_row(output_width);
        let output_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("output_staging_buffer"),
            size: (padded_row * output_height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        info!(
            "Compositor initialized: {}x{} (padded row={})",
            output_width, output_height, padded_row
        );

        Ok(Self {
            device,
            queue,
            render_pipeline,
            sampler,
            bind_group_layout,
            render_target,
            render_target_view,
            output_buffer,
            output_width,
            output_height,
            layer_cache: Mutex::new(Vec::new()),
            readback_buffer: Mutex::new(vec![0u8; (output_width * output_height * 4) as usize]),
        })
    }

    /// Composite a list of VideoFrames (scene items) into a single CompositeFrame.
    ///
    /// Layers are drawn bottom-to-top (index 0 = background).
    /// Uses cached GPU textures and buffers to eliminate per-frame memory allocation.
    pub fn composite(
        &self,
        frames: &[(SceneItem, &VideoFrame)],
        pts_ns: u64,
    ) -> Result<CompositeFrame> {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("composite_encoder"),
            });

        let mut cache = self.layer_cache.lock();
        let mut active_indices = Vec::with_capacity(frames.len());

        // 1. Prepare and update GPU resources for each visible layer
        for (item, frame) in frames {
            if !item.visible {
                continue;
            }

            let idx = active_indices.len();
            if idx >= cache.len() || cache[idx].width != frame.width || cache[idx].height != frame.height {
                // (Re)create layer texture and bind group once when size changes
                let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("cached_layer_texture"),
                    size: wgpu::Extent3d {
                        width: frame.width,
                        height: frame.height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                let uniform_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("cached_layer_uniform_buf"),
                    size: std::mem::size_of::<LayerUniforms>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("cached_layer_bind_group"),
                    layout: &self.bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&texture_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: uniform_buf.as_entire_binding(),
                        },
                    ],
                });

                let new_layer = CachedLayer {
                    width: frame.width,
                    height: frame.height,
                    texture,
                    uniform_buf,
                    bind_group,
                };

                if idx < cache.len() {
                    cache[idx] = new_layer;
                } else {
                    cache.push(new_layer);
                }
            }

            let cached = &cache[idx];

            // Direct GPU texture copy with 0 heap allocation & 0 CPU swizzle
            self.queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &cached.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &frame.data,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(frame.width * 4),
                    rows_per_image: Some(frame.height),
                },
                wgpu::Extent3d {
                    width: frame.width,
                    height: frame.height,
                    depth_or_array_layers: 1,
                },
            );

            // Compute UV rect
            let uv_rect = self.compute_uv_rect(item, frame);
            let is_bgra = if frame.format == PixelFormat::Bgra { 1.0f32 } else { 0.0f32 };
            let uniforms = LayerUniforms {
                uv_rect,
                params: [item.alpha, is_bgra, 0.0, 0.0],
            };
            self.queue.write_buffer(&cached.uniform_buf, 0, bytemuck::cast_slice(&[uniforms]));

            active_indices.push(idx);
        }

        // --- Render pass ---
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("composite_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.render_target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Clear to black on first layer
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_pass.set_pipeline(&self.render_pipeline);

            for &idx in &active_indices {
                render_pass.set_bind_group(0, &cache[idx].bind_group, &[]);
                render_pass.draw(0..3, 0..1); // Full-screen triangle
            }
        }

        // --- Copy render target → staging buffer ---
        let padded_row = padded_bytes_per_row(self.output_width);
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &self.render_target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &self.output_buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: Some(self.output_height),
                },
            },
            wgpu::Extent3d {
                width: self.output_width,
                height: self.output_height,
                depth_or_array_layers: 1,
            },
        );

        self.queue.submit(std::iter::once(encoder.finish()));

        // --- Map staging buffer and read back pixels ---
        let rgba_data = {
            let buffer_slice = self.output_buffer.slice(..);
            let (sender, receiver) = std::sync::mpsc::channel();
            buffer_slice.map_async(wgpu::MapMode::Read, move |r| {
                sender.send(r).unwrap();
            });
            self.device.poll(wgpu::MaintainBase::Wait);
            receiver.recv().context("GPU readback channel closed")??;

            let mapped = buffer_slice.get_mapped_range();
            // Strip wgpu row padding to get tightly-packed RGBA into pre-allocated buffer
            let row_bytes = self.output_width as usize * 4;
            let padded = padded_row as usize;
            let mut out = self.readback_buffer.lock();
            let total_len = row_bytes * self.output_height as usize;
            if out.len() != total_len {
                out.resize(total_len, 0);
            }
            for row in 0..self.output_height as usize {
                let start = row * padded;
                let dst_start = row * row_bytes;
                out[dst_start..dst_start + row_bytes].copy_from_slice(&mapped[start..start + row_bytes]);
            }
            out.clone()
        };
        self.output_buffer.unmap();

        debug!(
            "Compositor: composite done, {}x{} RGBA, pts={}ns",
            self.output_width, self.output_height, pts_ns
        );

        Ok(CompositeFrame {
            video: VideoFrame::new_rgba(rgba_data, self.output_width, self.output_height, pts_ns),
            pts_ns,
        })
    }

    /// Compute [u_offset, v_offset, u_scale, v_scale] for a scene item.
    fn compute_uv_rect(&self, item: &SceneItem, _frame: &VideoFrame) -> [f32; 4] {
        // For a full-frame source (no crop, no scale applied to UV):
        // Position/scale are applied by stretching the UV rect into the output.
        let x_norm = item.transform.x / self.output_width as f32;
        let y_norm = item.transform.y / self.output_height as f32;
        // For MVP: just use full UV rect (crop and scale via transform TODO)
        [x_norm, y_norm, item.transform.scale_x, item.transform.scale_y]
    }
}

/// Round up bytes per row to wgpu's COPY_BYTES_PER_ROW_ALIGNMENT (256 bytes).
fn padded_bytes_per_row(width: u32) -> u32 {
    let bytes_per_row = width * 4; // 4 bytes per RGBA pixel
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    (bytes_per_row + align - 1) / align * align
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padded_bytes_aligned() {
        // 1920 * 4 = 7680. Already a multiple of 256.
        assert_eq!(padded_bytes_per_row(1920), 7680);
        // 1280 * 4 = 5120. Already aligned.
        assert_eq!(padded_bytes_per_row(1280), 5120);
        // 100 * 4 = 400. Next multiple of 256 = 512.
        assert_eq!(padded_bytes_per_row(100), 512);
    }
}
