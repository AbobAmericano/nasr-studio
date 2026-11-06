// composite.wgsl — WGSL shader for obs-compositor
// Equivalent to OBS's draw_filter.effect and default.effect in libobs/data/

// --- Vertex stage ---
// Full-screen triangle trick: generates a clip-space triangle covering
// the entire screen without a vertex buffer.

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) tex_coord: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) in_vertex_index: u32) -> VertexOutput {
    // Full-screen triangle vertices (NDC)
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    var tex_coords = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );

    var out: VertexOutput;
    out.position = vec4<f32>(positions[in_vertex_index], 0.0, 1.0);
    out.tex_coord = tex_coords[in_vertex_index];
    return out;
}

// --- Fragment stage ---

// Uniform buffer for per-layer compositing parameters.
// Mirrors the OBS scene item transform and alpha.
// 32 bytes total: exactly matches Rust's LayerUniforms.
struct LayerUniforms {
    // UV rect [x, y, width, height] in normalized [0,1] texture space
    uv_rect: vec4<f32>,
    // params.x = alpha multiplier, params.yzw reserved
    params: vec4<f32>,
};

@group(0) @binding(0) var source_texture: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;
@group(0) @binding(2) var<uniform> layer: LayerUniforms;

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Map output UV into the source's UV rect (supports crop/scale)
    let uv = layer.uv_rect.xy + in.tex_coord * layer.uv_rect.zw;

    // Sample source texture
    var color = textureSample(source_texture, source_sampler, uv);

    // If source is BGRA (params.y > 0.5), swizzle in GPU hardware (0 CPU overhead)
    if layer.params.y > 0.5 {
        color = vec4<f32>(color.b, color.g, color.r, 1.0);
    } else {
        if color.a < 0.001 {
            color.a = 1.0;
        }
    }

    // Apply alpha multiplier
    color.a = color.a * layer.params.x;

    return color;
}
