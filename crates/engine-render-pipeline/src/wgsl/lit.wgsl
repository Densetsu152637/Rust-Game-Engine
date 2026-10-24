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
    let metallic = material.options.y * select(1.0, mr.b, maps.z > 0.5);
    let roughness = max(material.options.z * select(1.0, mr.g, maps.z > 0.5), 0.045);
    let alpha = roughness * roughness;
    let a2 = alpha * alpha;
    let nv = max(dot(n, v), 0.0001);
    let f0 = mix(vec3<f32>(0.04), color.rgb, metallic);
    var direct = vec3<f32>(0.0);
    // Only directional lights are shaded by this material today.
    for (var index = 0u; index < engine_light_count(); index++) {
        let light = engine_light(index);
        if light.kind != ENGINE_LIGHT_DIRECTIONAL { continue; }
        let visibility = engine_shadow_visibility(light, input.world, material.options.w > 0.5);
        let l = normalize(light.direction);
        let half_vector = l + v;
        let h = half_vector / max(length(half_vector), 0.000001);
        let nl = max(dot(n, l), 0.0);
        let nh = max(dot(n, h), 0.0);
        let vh = max(dot(v, h), 0.0);
        let denom = nh * nh * (a2 - 1.0) + 1.0;
        let distribution = a2 / max(3.14159265 * denom * denom, 0.0000001);
        let gv = 2.0 * nv / (nv + sqrt(a2 + (1.0 - a2) * nv * nv));
        let gl = 2.0 * nl / max(nl + sqrt(a2 + (1.0 - a2) * nl * nl), 0.000001);
        let fresnel = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - vh, 5.0);
        let specular = distribution * gv * gl * fresnel / max(4.0 * nv * nl, 0.000001);
        let diffuse = (vec3<f32>(1.0) - fresnel) * (1.0 - metallic) * color.rgb / 3.14159265;
        direct += (diffuse + specular) * (light.color * light.intensity) * nl * visibility;
    }
    let ambient = color.rgb * (1.0 - metallic) * engine_ambient();
    let shaded = direct + ambient;
    let result = select(shaded, color.rgb, material.options.x > 0.5);
    return vec4<f32>(result, color.a);
}
