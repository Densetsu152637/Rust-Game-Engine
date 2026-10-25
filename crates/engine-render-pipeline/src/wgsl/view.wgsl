// engine::view - the per-view bind group (group 0) every feature can import.
// Layout is a stable contract (docs/rendering-architecture.md).
struct EngineView {
    // Jittered view-projection: rasterize with this.
    clip_from_world: mat4x4<f32>,
    unjittered_clip_from_world: mat4x4<f32>,
    view_from_world: mat4x4<f32>,
    world_from_view: mat4x4<f32>,
    // Jittered projection.
    clip_from_view: mat4x4<f32>,
    unjittered_clip_from_view: mat4x4<f32>,
    // Inverse of the jittered projection.
    view_from_clip: mat4x4<f32>,
    // Inverse of the jittered view-projection.
    world_from_clip: mat4x4<f32>,
    // Previous frame's unjittered view-projection.
    previous_clip_from_world: mat4x4<f32>,
    camera_position: vec4<f32>,
    // xy pixels, zw reciprocals.
    render_size: vec4<f32>,
    output_size: vec4<f32>,
    // xy render pixels (y down), zw clip-space offset.
    jitter: vec4<f32>,
    ambient: vec4<f32>,
    // x seconds since start, y frame delta.
    time: vec4<f32>,
    // x frame index, y light count, z view id.
    counts: vec4<u32>,
}

const ENGINE_LIGHT_DIRECTIONAL: u32 = 0u;
const ENGINE_LIGHT_POINT: u32 = 1u;
const ENGINE_LIGHT_SPOT: u32 = 2u;
const ENGINE_LIGHT_AREA: u32 = 3u;

struct EngineLight {
    color: vec3<f32>,
    intensity: f32,
    position: vec3<f32>,
    range: f32,
    // Directional: from surfaces toward the light. Spot/area: light forward.
    direction: vec3<f32>,
    kind: u32,
    // Spot: cos inner, cos outer. Area: half width, half height, two-sided.
    params: vec4<f32>,
    // Index into engine_shadows, or -1.
    shadow_index: i32,
    flags: u32,
    reserved: vec2<u32>,
}

struct EngineLightList {
    // x light count; yzw reserved.
    counts: vec4<u32>,
    lights: array<EngineLight>,
}

struct EngineShadow {
    clip_from_world: mat4x4<f32>,
    // x depth bias, y atlas layer, z resolution, w 1.
    params: vec4<f32>,
}

@group(0) @binding(0) var<uniform> engine_view: EngineView;
@group(0) @binding(1) var<storage, read> engine_lights: EngineLightList;
@group(0) @binding(2) var<storage, read> engine_shadows: array<EngineShadow>;
@group(0) @binding(3) var engine_shadow_atlas: texture_depth_2d_array;

fn engine_light_count() -> u32 {
    return engine_lights.counts.x;
}

fn engine_light(index: u32) -> EngineLight {
    return engine_lights.lights[index];
}

fn engine_camera_position() -> vec3<f32> {
    return engine_view.camera_position.xyz;
}

fn engine_ambient() -> vec3<f32> {
    return engine_view.ambient.rgb;
}

fn engine_clip_from_world() -> mat4x4<f32> {
    return engine_view.clip_from_world;
}

// Fraction of `light` reaching `world` (3x3 PCF with receiver-plane depth
// correction). Fragment stage only: it uses derivatives, so call it in
// uniform control flow. `receive` false returns 1.
fn engine_shadow_visibility(light: EngineLight, world: vec3<f32>, receive: bool) -> f32 {
    let shadow = engine_shadows[max(light.shadow_index, 0)];
    let p = shadow.clip_from_world * vec4<f32>(world, 1.0);
    let ndc = p.xyz / p.w;
    let uv = ndc.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    // Derivatives before divergent bounds checks.
    let ux = dpdx(uv);
    let uy = dpdy(uv);
    let zx = dpdx(ndc.z);
    let zy = dpdy(ndc.z);
    let det = ux.x * uy.y - ux.y * uy.x;
    let safe_det = select(1.0, det, abs(det) > 0.00000001);
    let gradient = select(vec2<f32>(0.0), vec2<f32>(uy.y * zx - ux.y * zy, ux.x * zy - uy.x * zx) / safe_det, abs(det) > 0.00000001);
    if light.shadow_index < 0 || !receive { return 1.0; }
    if any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) || ndc.z < 0.0 || ndc.z > 1.0 { return 1.0; }
    let layer = i32(shadow.params.y);
    let size = vec2<i32>(textureDimensions(engine_shadow_atlas));
    let texel = vec2<i32>(uv * vec2<f32>(size));
    var sum = 0.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let coord = clamp(texel + vec2<i32>(x, y), vec2<i32>(0), size - vec2<i32>(1));
            let tap_uv = (vec2<f32>(coord) + vec2<f32>(0.5)) / vec2<f32>(size);
            let receiver_depth = ndc.z + dot(gradient, tap_uv - uv);
            sum += select(0.0, 1.0, receiver_depth - shadow.params.x <= textureLoad(engine_shadow_atlas, coord, layer, 0));
        }
    }
    return sum / 9.0;
}
