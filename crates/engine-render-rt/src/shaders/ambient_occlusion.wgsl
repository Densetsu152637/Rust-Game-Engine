// RT ambient occlusion. Appended to the ray-query and sampling libraries at
// group 0. STORAGE_FORMAT is replaced by the output format.
//
// AO = 1 - (occluded rays / rays): the cosine-weighted visibility of the
// hemisphere within `radius` (rays are cosine-distributed, so each counts
// equally). Pixels without geometry or without a normal are 1.

struct AoParams {
    // x: radius (ray length), y: normal bias.
    ray: vec4<f32>,
    // x, y: output size; z: rays per pixel; w: instance mask.
    size: vec4<u32>,
    // x: frame index, y: seed.
    frame: vec4<u32>,
}

@group(1) @binding(0) var ao_world_position: texture_2d<f32>;
@group(1) @binding(1) var ao_world_normal: texture_2d<f32>;
@group(1) @binding(2) var ao_output: texture_storage_2d<STORAGE_FORMAT, write>;
@group(1) @binding(3) var<uniform> ao_params: AoParams;

@compute @workgroup_size(8, 8)
fn ambient_occlusion(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = ao_params.size.xy;
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    let p = textureLoad(ao_world_position, id.xy, 0);
    let raw_n = textureLoad(ao_world_normal, id.xy, 0).xyz;
    var ao = 1.0;
    if p.w >= 0.5 && dot(raw_n, raw_n) > 1e-12 {
        let n = normalize(raw_n);
        rt_seed(id.xy, size.x, ao_params.frame.x, ao_params.frame.y);
        let origin = p.xyz + n * ao_params.ray.y;
        let rays = ao_params.size.z;
        var occluded = 0u;
        for (var i = 0u; i < rays; i += 1u) {
            if rt_occluded(origin, rt_cosine_sample(n), 0.0, ao_params.ray.x, ao_params.size.w) {
                occluded += 1u;
            }
        }
        ao = 1.0 - f32(occluded) / f32(rays);
    }
    textureStore(ao_output, id.xy, vec4<f32>(ao, 0.0, 0.0, 1.0));
}
