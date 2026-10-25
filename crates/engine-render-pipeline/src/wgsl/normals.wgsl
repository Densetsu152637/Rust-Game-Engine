// Built-in normal prepass: world-space unit normals (xyz) and coverage (w).
// Uses authored normals when present, else face normals facing the camera.
#import engine::mesh

struct Output {
    @builtin(position) position: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
}

@vertex fn vs_main(vertex: EngineVertex, @builtin(instance_index) instance: u32) -> Output {
    var out: Output;
    let world = engine_world_from_local(instance) * vec4<f32>(vertex.position, 1.0);
    out.position = engine_view.clip_from_world * world;
    out.world = world.xyz;
    out.normal = (engine_normal_from_local(instance) * vec4<f32>(vertex.normal, 0.0)).xyz;
    return out;
}

@fragment fn fs_main(input: Output, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let face = cross(dpdx(input.world), dpdy(input.world));
    var n = face / max(length(face), 0.000001);
    let v = normalize(engine_view.camera_position.xyz - input.world);
    n = select(-n, n, dot(n, v) >= 0.0);
    if length(input.normal) > 0.000001 { n = normalize(input.normal) * select(-1.0, 1.0, front); }
    return vec4<f32>(n, 1.0);
}
