// Optional engine::material implementation for orthographic lit sprites.
#import engine::brdf
struct EngineVertex {
    @location(0) position: vec3<f32>, @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>, @location(3) normal: vec3<f32>,
}
const ENGINE_LIGHT_DIRECTIONAL: u32 = 0u;
const ENGINE_LIGHT_POINT: u32 = 1u;
const ENGINE_LIGHT_SPOT: u32 = 2u;
const ENGINE_LIGHT_AREA: u32 = 3u;
struct EngineLight {
    color: vec3<f32>, intensity: f32,
    position: vec3<f32>, range: f32,
    direction: vec3<f32>, kind: u32,
    tangent: vec3<f32>, shadow_index: i32,
    params: vec4<f32>,
}
struct EngineIncident { l: vec3<f32>, irradiance: vec3<f32>, }
struct EngineMaterial { tint: vec4<f32>, options: vec4<f32>, }
struct EngineSpriteLighting {
    ambient: vec4<f32>, count: vec4<u32>, offset: vec4<f32>,
    lights: array<EngineLight,64>,
}
@group(0) @binding(0) var<uniform> engine_sprite_clip_from_world: mat4x4<f32>;
@group(1) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;
@group(1) @binding(2) var metallic_roughness_map: texture_2d<f32>;
@group(1) @binding(3) var normal_map: texture_2d<f32>;
@group(2) @binding(0) var<uniform> material: EngineMaterial;
@group(2) @binding(1) var<uniform> engine_sprite_lighting: EngineSpriteLighting;
@group(2) @binding(2) var<uniform> maps: vec4<f32>;

// Clip matrix is UNSHIFTED. World-space input and light evaluation share the
// presenter translation; no offset is baked into the camera a second time.
fn engine_world_from_local(instance: u32) -> mat4x4<f32> {
    return mat4x4<f32>(vec4<f32>(1,0,0,0),vec4<f32>(0,1,0,0),vec4<f32>(0,0,1,0),vec4<f32>(engine_sprite_lighting.offset.xyz,1));
}
fn engine_normal_from_local(instance: u32) -> mat4x4<f32> {
    return mat4x4<f32>(vec4<f32>(1,0,0,0),vec4<f32>(0,1,0,0),vec4<f32>(0,0,1,0),vec4<f32>(0,0,0,1));
}
fn engine_previous_world_from_local(instance: u32) -> mat4x4<f32> { return engine_world_from_local(instance); }
fn engine_clip_from_world() -> mat4x4<f32> { return engine_sprite_clip_from_world; }
// Use this for an orthographic sprite; a fixed camera POSITION is insufficient.
fn engine_sprite_view_direction() -> vec3<f32> { return vec3<f32>(0,0,1); }
fn engine_camera_position() -> vec3<f32> { return vec3<f32>(0,0,1); }
fn engine_ambient() -> vec3<f32> { return engine_sprite_lighting.ambient.rgb; }
fn engine_light_count() -> u32 { return min(engine_sprite_lighting.count.x,64u); }
fn engine_light(index: u32) -> EngineLight { return engine_sprite_lighting.lights[index]; }
fn engine_view_depth(world: vec3<f32>) -> f32 { return 0.0; }
fn engine_light_incident(light: EngineLight, world: vec3<f32>, n: vec3<f32>, v: vec3<f32>) -> EngineIncident {
    if light.kind == ENGINE_LIGHT_DIRECTIONAL {
        return EngineIncident(light.direction,light.color*light.intensity);
    }
    let delta = light.position-world;
    let distance = length(delta);
    return EngineIncident(delta/max(distance,0.000001),light.color*light.intensity*engine_distance_falloff(distance,light.range));
}
fn engine_light_visibility(light: EngineLight, world: vec3<f32>, n: vec3<f32>) -> f32 { return 1.0; }
fn engine_cascade_index(light: EngineLight, world: vec3<f32>) -> i32 { return -1; }
fn engine_light_contribution(light: EngineLight, surface: EngineSurface, world: vec3<f32>, v: vec3<f32>, receive_shadows: bool) -> vec3<f32> {
    let incident = engine_light_incident(light,world,surface.normal,v);
    let brdf = engine_brdf(surface,v,incident.l);
    return (brdf.diffuse+brdf.specular)*incident.irradiance*max(dot(surface.normal,incident.l),0.0);
}
fn engine_direct_lighting(surface: EngineSurface, world: vec3<f32>, v: vec3<f32>, frag: vec2<f32>, receive_shadows: bool) -> vec3<f32> {
    var result = vec3<f32>(0);
    for (var i=0u;i<engine_light_count();i++) { result += engine_light_contribution(engine_light(i),surface,world,v,false); }
    return result;
}
fn engine_light_contribution_screen(light: EngineLight, surface: EngineSurface, world: vec3<f32>, v: vec3<f32>, receive_shadows: bool, screen: vec2<f32>) -> vec3<f32> {
    return engine_light_contribution(light,surface,world,v,false);
}
fn engine_indirect_lighting(surface: EngineSurface, v: vec3<f32>, frag: vec2<f32>) -> vec3<f32> {
    return surface.albedo*(1.0-surface.metallic)*engine_ambient();
}
fn engine_material_indirect_lighting(surface: EngineSurface, v: vec3<f32>, frag: vec2<f32>) -> vec3<f32> {
    return engine_indirect_lighting(surface,v,frag);
}