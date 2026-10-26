// The built-in metallic-roughness material (engine material contract).
#import engine::material

struct Output {
    @builtin(position) position: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) normal: vec3<f32>,
}

@vertex fn vs_main(vertex: EngineVertex, @builtin(instance_index) instance: u32) -> Output {
    var out: Output;
    let world = engine_world_from_local(instance) * vec4<f32>(vertex.position, 1.0);
    out.position = engine_clip_from_world() * world;
    out.world = world.xyz;
    out.uv = vertex.uv;
    out.color = vertex.color;
    out.normal = (engine_normal_from_local(instance) * vec4<f32>(vertex.normal, 0.0)).xyz;
    return out;
}

@fragment fn fs_main(input: Output, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let color = textureSample(image, image_sampler, input.uv) * input.color * material.tint;
    let derivative_normal = cross(dpdx(input.world), dpdy(input.world));
    var n = derivative_normal / max(length(derivative_normal), 0.000001);
    let v = normalize(engine_camera_position() - input.world);
    // Derivative normals face the viewed side; authored normals follow triangle winding.
    n = select(-n, n, dot(n, v) >= 0.0);
    if length(input.normal) > 0.000001 { n = normalize(input.normal) * select(-1.0, 1.0, front); }
    let dx = dpdx(input.world);
    let dy = dpdy(input.world);
    let tx = dpdx(input.uv);
    let ty = dpdy(input.uv);
    let determinant = tx.x * ty.y - tx.y * ty.x;
    let sampled_normal = textureSample(normal_map, image_sampler, input.uv).xyz * 2.0 - vec3<f32>(1.0);
    if maps.x > 0.5 && abs(determinant) > 0.000001 {
        let tangent = (dx * ty.y - dy * tx.y) / determinant;
        let bitangent = (dy * tx.x - dx * ty.x) / determinant;
        let projected = tangent - n * dot(n, tangent);
        if length(projected) > 0.000001 {
            let t = normalize(projected);
            let b = cross(n, t) * select(-1.0, 1.0, dot(cross(n, t), bitangent) >= 0.0);
            let mapped = mat3x3<f32>(t, b, n) * vec3<f32>(sampled_normal.xy * maps.y, sampled_normal.z);
            if length(mapped) > 0.000001 { n = normalize(mapped); }
        }
    }
    let mr = textureSample(metallic_roughness_map, image_sampler, input.uv);
    var surface: EngineSurface;
    surface.albedo = color.rgb;
    surface.metallic = material.options.y * select(1.0, mr.b, maps.z > 0.5);
    surface.roughness = material.options.z * select(1.0, mr.g, maps.z > 0.5);
    surface.normal = n;
    let direct = engine_direct_lighting(surface, input.world, v, input.position.xy, material.options.w > 0.5);
    let ambient = engine_material_indirect_lighting(surface, v, input.position.xy);
    let shaded = direct + ambient;
    let result = select(shaded, color.rgb, material.options.x > 0.5);
    return vec4<f32>(result, color.a);
}
