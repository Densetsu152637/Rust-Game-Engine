// ui.world - engine.UiWorldPanel and engine.WorldText in the 3D frame.
//
// Item-local geometry (x right, y up, world units) is placed by the item's
// world matrix, optionally turned toward the camera of the view being drawn
// (billboard modes mirror layout.rs `billboard_matrix`), rasterised with the
// view's jittered view-projection and depth-tested against the scene depth
// without writing it. Vertex colours are authored in sRGB and converted to
// linear; image textures are sampled through their sRGB view; the result is
// straight-alpha blended into linear HDR colour.
#import engine::view

struct Item {
    world_from_local: mat4x4<f32>,
    // x: billboard (0 none, 1 full, 2 yaw, 3 screen), y: 1 when the texture is
    // an SDF page (distance in alpha).
    params: vec4<u32>,
}

@group(1) @binding(0) var<uniform> item: Item;
@group(2) @binding(0) var ui_texture: texture_2d<f32>;
@group(2) @binding(1) var ui_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
}

struct VertexOutput {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
}

fn placed(world: mat4x4<f32>, mode: u32) -> mat4x4<f32> {
    if (mode == 0u) {
        return world;
    }
    let position = world[3].xyz;
    let sx = length(world[0].xyz);
    let sy = length(world[1].xyz);
    let sz = length(world[2].xyz);
    var right: vec3<f32>;
    var up: vec3<f32>;
    var back: vec3<f32>;
    if (mode == 3u) {
        right = normalize(engine_view.world_from_view[0].xyz);
        up = normalize(engine_view.world_from_view[1].xyz);
        back = normalize(engine_view.world_from_view[2].xyz);
    } else {
        var toward = engine_view.camera_position.xyz - position;
        if (mode == 2u) {
            toward.y = 0.0;
        }
        if (dot(toward, toward) < 1e-12) {
            return world;
        }
        back = normalize(toward);
        var reference = vec3<f32>(0.0, 1.0, 0.0);
        if (abs(back.y) > 0.999) {
            reference = vec3<f32>(1.0, 0.0, 0.0);
        }
        right = normalize(cross(reference, back));
        up = cross(back, right);
    }
    return mat4x4<f32>(
        vec4<f32>(right * sx, 0.0),
        vec4<f32>(up * sy, 0.0),
        vec4<f32>(back * sz, 0.0),
        vec4<f32>(position, 1.0),
    );
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let model = placed(item.world_from_local, item.params.x);
    out.clip = engine_view.clip_from_world * (model * vec4<f32>(input.position, 1.0));
    out.uv = input.uv;
    out.color = vec4<f32>(srgb_to_linear(input.color.rgb), input.color.a);
    return out;
}

// distance: 0..1 with the glyph edge at 0.5 (inside > 0.5); one screen pixel of
// anti-aliasing from the screen-space derivative.
fn distance_coverage(distance: f32) -> f32 {
    let signed = distance - 0.5;
    let width = max(0.5 * fwidth(signed), 0.00001);
    return smoothstep(-width, width, signed);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(ui_texture, ui_sampler, input.uv);
    let sdf = item.params.y == 1u;
    let coverage = distance_coverage(texel.a);
    let modulated = vec4<f32>(input.color.rgb * texel.rgb, input.color.a * texel.a);
    let from_distance = vec4<f32>(input.color.rgb, input.color.a * coverage);
    return select(modulated, from_distance, sdf);
}
