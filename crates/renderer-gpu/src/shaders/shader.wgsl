// Vertex input: per-instance data for each cell
struct VertexInput {
    @builtin(vertex_index) vertex_index: u32,
    // Per-instance attributes
    @location(0) grid_pos: vec2<f32>,    // grid column, row
    @location(1) glyph_uv: vec2<f32>,   // UV offset into glyph atlas (top-left corner)
    @location(2) fg_color: vec4<f32>,
    @location(3) bg_color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) fg_color: vec4<f32>,
    @location(2) bg_color: vec4<f32>,
};

struct Uniforms {
    // Grid dimensions in pixels
    cell_size: vec2<f32>,
    // Surface dimensions in pixels
    surface_size: vec2<f32>,
    // Glyph UV size (width and height of one glyph in UV space)
    glyph_uv_size: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var glyph_texture: texture_2d<f32>;
@group(0) @binding(2) var glyph_sampler: sampler;

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    // Generate quad vertices from vertex_index (0..5 for two triangles)
    // Triangle 1: 0,1,2  Triangle 2: 2,1,3
    // Positions: 0=TL, 1=BL, 2=TR, 3=BR
    var local_pos: vec2<f32>;
    var local_uv: vec2<f32>;

    switch input.vertex_index {
        case 0u: {
            local_pos = vec2<f32>(0.0, 0.0);
            local_uv = vec2<f32>(0.0, 0.0);
        }
        case 1u: {
            local_pos = vec2<f32>(0.0, 1.0);
            local_uv = vec2<f32>(0.0, 1.0);
        }
        case 2u: {
            local_pos = vec2<f32>(1.0, 0.0);
            local_uv = vec2<f32>(1.0, 0.0);
        }
        case 3u: {
            local_pos = vec2<f32>(1.0, 0.0);
            local_uv = vec2<f32>(1.0, 0.0);
        }
        case 4u: {
            local_pos = vec2<f32>(0.0, 1.0);
            local_uv = vec2<f32>(0.0, 1.0);
        }
        case 5u: {
            local_pos = vec2<f32>(1.0, 1.0);
            local_uv = vec2<f32>(1.0, 1.0);
        }
        default: {
            local_pos = vec2<f32>(0.0, 0.0);
            local_uv = vec2<f32>(0.0, 0.0);
        }
    }

    // Pixel position of this vertex
    let pixel_pos = (input.grid_pos + local_pos) * uniforms.cell_size;

    // Convert to NDC: x: [0, width] -> [-1, 1], y: [0, height] -> [1, -1] (flip Y)
    let ndc = vec2<f32>(
        pixel_pos.x / uniforms.surface_size.x * 2.0 - 1.0,
        1.0 - pixel_pos.y / uniforms.surface_size.y * 2.0,
    );

    var output: VertexOutput;
    output.position = vec4<f32>(ndc, 0.0, 1.0);
    // Map local UV to atlas UV
    output.uv = input.glyph_uv + local_uv * uniforms.glyph_uv_size;
    output.fg_color = input.fg_color;
    output.bg_color = input.bg_color;

    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let glyph_alpha = textureSample(glyph_texture, glyph_sampler, input.uv).r;
    let color = mix(input.bg_color, input.fg_color, glyph_alpha);
    return color;
}
