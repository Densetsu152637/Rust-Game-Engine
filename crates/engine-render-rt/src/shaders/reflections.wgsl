// RT reflections. Appended to the ray-query and shading libraries at group 0.
//
// One GGX visible-normal (VNDF) sampled reflection ray per pixel (perceptual
// roughness r, GGX alpha = r^2; r below ~0.032 reflects as a mirror). The
// output is the radiance arriving along the sampled direction (the specular
// BRDF and Fresnel are applied when compositing) and the hit distance:
//   rgb: closest hit emissive + albedo / pi * E_sun * cos * sun visibility,
//        or the sky radiance on a miss;
//   a:   hit distance; REFL_MISS (65504) on a miss; REFL_ROUGH (-1) where
//        roughness > cutoff (not traced: rgb 0, use the raster fallback);
//        REFL_NONE (-2) where there is no geometry.

const REFL_MISS: f32 = 65504.0;
const REFL_ROUGH: f32 = -1.0;
const REFL_NONE: f32 = -2.0;

struct ReflParams {
    lighting: RtLightingRecord,
    // xyz: camera position; w: roughness cutoff.
    eye: vec4<f32>,
    // x: normal bias, y: max ray distance.
    ray: vec4<f32>,
    // x, y: output size; z: instance mask; w: material count.
    size: vec4<u32>,
    // x: frame index, y: seed.
    frame: vec4<u32>,
}

@group(1) @binding(0) var refl_world_position: texture_2d<f32>;
@group(1) @binding(1) var refl_world_normal: texture_2d<f32>;
@group(1) @binding(2) var refl_roughness: texture_2d<f32>;
@group(1) @binding(3) var refl_output: texture_storage_2d<rgba16float, write>;
@group(1) @binding(4) var<storage, read> refl_materials: array<RtMaterialRecord>;
@group(1) @binding(5) var<uniform> refl_params: ReflParams;

fn refl_direction(v: vec3<f32>, n: vec3<f32>, roughness: f32) -> vec3<f32> {
    let mirror = reflect(-v, n);
    let alpha = roughness * roughness;
    if alpha < 1e-3 {
        return mirror;
    }
    let basis = rt_basis(n);
    var v_local = transpose(basis) * v;
    v_local.z = max(v_local.z, 1e-4);
    let h = basis * rt_ggx_vndf(normalize(v_local), alpha);
    let l = reflect(-v, h);
    if dot(l, n) <= 0.0 {
        return mirror;
    }
    return l;
}

@compute @workgroup_size(8, 8)
fn reflections(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = refl_params.size.xy;
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    let p = textureLoad(refl_world_position, id.xy, 0);
    let raw_n = textureLoad(refl_world_normal, id.xy, 0).xyz;
    if p.w < 0.5 || dot(raw_n, raw_n) <= 1e-12 {
        textureStore(refl_output, id.xy, vec4<f32>(0.0, 0.0, 0.0, REFL_NONE));
        return;
    }
    let roughness = clamp(textureLoad(refl_roughness, id.xy, 0).x, 0.0, 1.0);
    if roughness > refl_params.eye.w {
        textureStore(refl_output, id.xy, vec4<f32>(0.0, 0.0, 0.0, REFL_ROUGH));
        return;
    }
    rt_seed(id.xy, size.x, refl_params.frame.x, refl_params.frame.y);
    let n = normalize(raw_n);
    let v = normalize(refl_params.eye.xyz - p.xyz);
    let dir = refl_direction(v, n, roughness);
    let bias = refl_params.ray.x;
    let t_max = refl_params.ray.y;
    let mask = refl_params.size.z;
    let origin = p.xyz + n * bias;
    let hit = rt_closest_hit(origin, dir, 0.0, t_max, mask);
    var radiance: vec3<f32>;
    var distance = REFL_MISS;
    if hit.hit {
        let surface = rt_surface(hit, origin, dir);
        let material = refl_materials[min(surface.material_index, refl_params.size.w - 1u)];
        let hit_n = rt_face_forward(surface.geometric_normal, dir);
        radiance = material.emissive.xyz
            + rt_sun_diffuse(refl_params.lighting, surface.position, hit_n, material.albedo.xyz, bias, t_max, mask);
        distance = hit.t;
    } else {
        radiance = rt_sky(dir, refl_params.lighting.sky_zenith.xyz, refl_params.lighting.sky_horizon.xyz);
    }
    textureStore(refl_output, id.xy, vec4<f32>(radiance, distance));
}
