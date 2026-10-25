// RT diffuse global illumination. Appended to the ray-query and shading
// libraries at group 0.
//
// Per pixel, `rays` cosine-weighted paths of up to `bounces` segments leave
// the G-buffer surface. Each hit adds its emission plus the sun reflected by
// its Lambertian albedo (next-event estimation: one rt_occluded shadow ray);
// a miss adds the sky. Later segments are cosine-sampled with throughput *=
// albedo. The output rgb is the cosine-weighted mean incident radiance, i.e.
// irradiance / pi: multiply by the surface albedo for diffuse radiance. The
// direct sun on the G-buffer surface itself is excluded (that is the shadowed
// direct-lighting pass). a = 1 where there is geometry, 0 elsewhere (rgb 0).

struct GiParams {
    lighting: RtLightingRecord,
    // x: normal bias, y: max ray distance.
    ray: vec4<f32>,
    // x, y: output size; z: instance mask; w: material count.
    size: vec4<u32>,
    // x: frame index, y: seed, z: rays per pixel, w: bounces.
    frame: vec4<u32>,
}

@group(1) @binding(0) var gi_world_position: texture_2d<f32>;
@group(1) @binding(1) var gi_world_normal: texture_2d<f32>;
@group(1) @binding(2) var gi_output: texture_storage_2d<rgba16float, write>;
@group(1) @binding(3) var<storage, read> gi_materials: array<RtMaterialRecord>;
@group(1) @binding(4) var<uniform> gi_params: GiParams;

fn gi_path(origin_in: vec3<f32>, dir_in: vec3<f32>) -> vec3<f32> {
    var origin = origin_in;
    var dir = dir_in;
    var throughput = vec3<f32>(1.0);
    var radiance = vec3<f32>(0.0);
    let bias = gi_params.ray.x;
    let t_max = gi_params.ray.y;
    let mask = gi_params.size.z;
    let bounces = gi_params.frame.w;
    for (var bounce = 1u; bounce <= bounces; bounce += 1u) {
        let hit = rt_closest_hit(origin, dir, 0.0, t_max, mask);
        if !hit.hit {
            radiance += throughput * rt_sky(dir, gi_params.lighting.sky_zenith.xyz, gi_params.lighting.sky_horizon.xyz);
            break;
        }
        let surface = rt_surface(hit, origin, dir);
        let material = gi_materials[min(surface.material_index, gi_params.size.w - 1u)];
        let n = rt_face_forward(surface.geometric_normal, dir);
        radiance += throughput * (material.emissive.xyz
            + rt_sun_diffuse(gi_params.lighting, surface.position, n, material.albedo.xyz, bias, t_max, mask));
        throughput *= material.albedo.xyz;
        if bounce == bounces || all(throughput == vec3<f32>(0.0)) {
            break;
        }
        origin = surface.position + n * bias;
        dir = rt_cosine_sample(n);
    }
    return radiance;
}

@compute @workgroup_size(8, 8)
fn diffuse_gi(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = gi_params.size.xy;
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    let p = textureLoad(gi_world_position, id.xy, 0);
    let raw_n = textureLoad(gi_world_normal, id.xy, 0).xyz;
    if p.w < 0.5 || dot(raw_n, raw_n) <= 1e-12 {
        textureStore(gi_output, id.xy, vec4<f32>(0.0));
        return;
    }
    rt_seed(id.xy, size.x, gi_params.frame.x, gi_params.frame.y);
    let n = normalize(raw_n);
    let origin = p.xyz + n * gi_params.ray.x;
    let rays = gi_params.frame.z;
    var sum = vec3<f32>(0.0);
    for (var i = 0u; i < rays; i += 1u) {
        sum += gi_path(origin, rt_cosine_sample(n));
    }
    textureStore(gi_output, id.xy, vec4<f32>(sum / f32(rays), 1.0));
}
