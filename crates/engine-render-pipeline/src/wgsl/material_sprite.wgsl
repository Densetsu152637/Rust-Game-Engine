// engine::material for 2D sprites with a material: the same functions and
// material bindings as the 3D module, implemented with the sprite path's
// per-draw uniform. Sprites are unlit: no lights, no shadows, identity model.
struct EngineVertex {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) normal: vec3<f32>,
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
    direction: vec3<f32>,
    kind: u32,
    params: vec4<f32>,
    shadow_index: i32,
    flags: u32,
    reserved: vec2<u32>,
}

struct EngineMaterial {
    tint: vec4<f32>,
    options: vec4<f32>,
}

@group(0) @binding(0) var<uniform> engine_sprite_clip_from_world: mat4x4<f32>;
@group(1) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;
@group(1) @binding(2) var metallic_roughness_map: texture_2d<f32>;
@group(1) @binding(3) var normal_map: texture_2d<f32>;
@group(2) @binding(0) var<uniform> material: EngineMaterial;
@group(2) @binding(2) var<uniform> maps: vec4<f32>;

fn engine_world_from_local(instance: u32) -> mat4x4<f32> {
    return mat4x4<f32>(vec4<f32>(1.0, 0.0, 0.0, 0.0), vec4<f32>(0.0, 1.0, 0.0, 0.0), vec4<f32>(0.0, 0.0, 1.0, 0.0), vec4<f32>(0.0, 0.0, 0.0, 1.0));
}

fn engine_normal_from_local(instance: u32) -> mat4x4<f32> {
    return engine_world_from_local(instance);
}

fn engine_previous_world_from_local(instance: u32) -> mat4x4<f32> {
    return engine_world_from_local(instance);
}

fn engine_clip_from_world() -> mat4x4<f32> {
    return engine_sprite_clip_from_world;
}

fn engine_camera_position() -> vec3<f32> {
    return vec3<f32>(0.0, 0.0, 1.0);
}

fn engine_ambient() -> vec3<f32> {
    return vec3<f32>(0.0);
}

fn engine_light_count() -> u32 {
    return 0u;
}

fn engine_light(index: u32) -> EngineLight {
    return EngineLight(vec3<f32>(0.0), 0.0, vec3<f32>(0.0), 0.0, vec3<f32>(0.0, 0.0, 1.0), ENGINE_LIGHT_DIRECTIONAL, vec4<f32>(0.0), -1, 0u, vec2<u32>(0u));
}

fn engine_shadow_visibility(light: EngineLight, world: vec3<f32>, receive: bool) -> f32 {
    return 1.0;
}
