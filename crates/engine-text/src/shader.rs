//! WGSL source for sampling engine-text atlas pages. GPU integration (pipeline, bind groups,
//! `NativeRenderPass`) belongs to the renderer side; this crate only ships and validates source.

/// Glyph shader for coverage and SDF atlas pages.
///
/// Bindings (group 0): `0` uniform `TextUniforms { view_proj, params }`, `1` the atlas page
/// `texture_2d<f32>`, `2` a filtering sampler. Vertex layout matches `runtime::Vertex`
/// (`@location(0) position: vec3`, `@location(1) uv: vec2`, `@location(2) color: vec4`).
///
/// `params.x` selects the mode: `0` coverage in alpha, `1` signed distance in alpha (engine-text
/// SDF pages), `2` median of RGB for multi-channel distance pages produced elsewhere
/// (engine-text does not generate MSDF pages). `params.y` biases the edge in normalised
/// distance units (positive is bolder, for example an outline pass), and `params.z` scales the
/// anti-aliasing width (1 is one screen pixel). The distance edge is smoothed with
/// `smoothstep` over the screen-space derivative `fwidth`, so text stays sharp at any scale.
/// Output is straight alpha: the vertex colour's RGB with alpha multiplied by coverage.
pub const GLYPH_SHADER_WGSL: &str = r#"
struct TextUniforms {
    view_proj: mat4x4<f32>,
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> text: TextUniforms;
@group(0) @binding(1) var atlas_texture: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip = text.view_proj * vec4<f32>(input.position, 1.0);
    out.uv = input.uv;
    out.color = input.color;
    return out;
}

fn median3(r: f32, g: f32, b: f32) -> f32 {
    return max(min(r, g), min(max(r, g), b));
}

// distance: 0..1 with the glyph edge at 0.5 (inside > 0.5).
fn distance_coverage(distance: f32, bias: f32, softness: f32) -> f32 {
    let signed = distance - 0.5 + bias;
    let width = max(0.5 * fwidth(signed) * softness, 0.00001);
    return smoothstep(-width, width, signed);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(atlas_texture, atlas_sampler, input.uv);
    let mode = u32(text.params.x + 0.5);
    let distance = select(texel.a, median3(texel.r, texel.g, texel.b), mode == 2u);
    let sdf_alpha = distance_coverage(distance, text.params.y, max(text.params.z, 0.0001));
    let alpha = select(sdf_alpha, texel.a, mode == 0u);
    return vec4<f32>(input.color.rgb, input.color.a * alpha);
}
"#;

/// `params.x` value for coverage pages.
pub const SHADER_MODE_COVERAGE: f32 = 0.0;
/// `params.x` value for engine-text SDF pages.
pub const SHADER_MODE_SDF: f32 = 1.0;
/// `params.x` value for externally generated multi-channel distance pages.
pub const SHADER_MODE_MSDF: f32 = 2.0;
