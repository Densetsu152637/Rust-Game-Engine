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

// Intensities are shading units: photometric value x view exposure
// (directional: lux, point/spot: candela, area: cd/m^2).
struct EngineLight {
    color: vec3<f32>,
    intensity: f32,
    position: vec3<f32>,
    // Distance where the windowed inverse-square falloff reaches zero.
    range: f32,
    // Directional: from surfaces toward the light. Spot/area: light forward.
    direction: vec3<f32>,
    kind: u32,
    // Area: unit width axis of the rectangle.
    tangent: vec3<f32>,
    // First EngineShadow (cascade 0, cube face +X), or -1.
    shadow_index: i32,
    // Directional: cascade count, cascade blend. Spot: cos inner, cos outer.
    // Area: half width, half height, two-sided.
    params: vec4<f32>,
}

struct EngineLightList {
    // x light count, y directional light count (they come first); zw reserved.
    counts: vec4<u32>,
    lights: array<EngineLight>,
}

struct EngineShadow {
    clip_from_world: mat4x4<f32>,
    // Atlas uv offset xy, uv size, layer.
    rect: vec4<f32>,
    // Depth bias and normal bias (texels), world texel size (perspective: at
    // unit distance), 1 when perspective.
    bias: vec4<f32>,
    // Mode (0 hard, 1 PCF, 2 PCSS), PCF radius, PCSS source size, PCSS max radius.
    sampling: vec4<f32>,
    // Projection near, far; cascade split near, far (camera view depth).
    range: vec4<f32>,
}

// The froxel grid of local lights built by engine.lights.clusters.
struct EngineClusterGrid {
    // x, y, z cluster counts; w lights per cluster list.
    dims: vec4<u32>,
    // x near, y far (view depth); slice = log(depth) * z + w.
    depth: vec4<f32>,
    // xy clusters per render pixel; zw reserved.
    tile: vec4<f32>,
    // x 0 clustered / 1 all lights, y first local light, z light count.
    params: vec4<u32>,
    counts: array<u32>,
}

@group(0) @binding(0) var<uniform> engine_view: EngineView;
@group(0) @binding(1) var<storage, read> engine_lights: EngineLightList;
@group(0) @binding(2) var<storage, read> engine_shadows: array<EngineShadow>;
@group(0) @binding(3) var engine_shadow_atlas: texture_depth_2d_array;
@group(0) @binding(4) var<storage, read> engine_clusters: EngineClusterGrid;
// Cluster c's light indices start at c * engine_clusters.dims.w.
@group(0) @binding(5) var<storage, read> engine_cluster_lights: array<u32>;
// Linear-filtered less-equal comparison sampler of the shadow atlas.
@group(0) @binding(6) var engine_shadow_sampler: sampler_comparison;

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
