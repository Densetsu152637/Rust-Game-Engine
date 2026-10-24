// RT shadow mask for one directional light. Appended to the ray-query library
// at group 0. STORAGE_FORMAT is replaced by the output format (r8unorm,
// r16float or r32float).
//
// Visibility is 1 (lit) or 0 (shadowed). Pixels without geometry
// (world_position.w < 0.5) are 1; surfaces facing away from the light are 0.

struct ShadowParams {
    // xyz: unit direction towards the light.
    direction_to_light: vec4<f32>,
    // x: normal bias (world units), y: t_min, z: t_max.
    ray: vec4<f32>,
    // x, y: output size; z: instance mask.
    size_mask: vec4<u32>,
}

@group(1) @binding(0) var shadow_world_position: texture_2d<f32>;
@group(1) @binding(1) var shadow_world_normal: texture_2d<f32>;
@group(1) @binding(2) var shadow_visibility: texture_storage_2d<STORAGE_FORMAT, write>;
@group(1) @binding(3) var<uniform> shadow_params: ShadowParams;

@compute @workgroup_size(8, 8)
fn shadow_mask(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = shadow_params.size_mask.xy;
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    let p = textureLoad(shadow_world_position, id.xy, 0);
    var visibility = 1.0;
    if p.w >= 0.5 {
        let l = shadow_params.direction_to_light.xyz;
        let raw_n = textureLoad(shadow_world_normal, id.xy, 0).xyz;
        var origin = p.xyz;
        var facing = true;
        if dot(raw_n, raw_n) > 1e-12 {
            let n = normalize(raw_n);
            facing = dot(n, l) > 0.0;
            origin = origin + n * shadow_params.ray.x;
        }
        if !facing || rt_occluded(origin, l, shadow_params.ray.y, shadow_params.ray.z, shadow_params.size_mask.z) {
            visibility = 0.0;
        }
    }
    textureStore(shadow_visibility, id.xy, vec4<f32>(visibility, 0.0, 0.0, 1.0));
}
