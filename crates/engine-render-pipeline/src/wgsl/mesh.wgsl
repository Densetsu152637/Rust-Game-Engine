// engine::mesh - engine vertex layout and per-instance data of meshes drawn
// by the built-in mesh path (group 3).
#import engine::view

struct EngineVertex {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    // Zero when the mesh has no authored normals.
    @location(3) normal: vec3<f32>,
}

struct EngineInstance {
    world_from_local: mat4x4<f32>,
    // Inverse transpose of world_from_local.
    normal_from_local: mat4x4<f32>,
    // Last frame's transform (equal after a history reset or for new draws).
    previous_world_from_local: mat4x4<f32>,
    // x: 32-bit hash of the draw id (picking); yzw reserved.
    params: vec4<u32>,
}

@group(3) @binding(0) var<storage, read> engine_instances: array<EngineInstance>;

fn engine_world_from_local(instance: u32) -> mat4x4<f32> {
    return engine_instances[instance].world_from_local;
}

fn engine_normal_from_local(instance: u32) -> mat4x4<f32> {
    return engine_instances[instance].normal_from_local;
}

fn engine_previous_world_from_local(instance: u32) -> mat4x4<f32> {
    return engine_instances[instance].previous_world_from_local;
}
