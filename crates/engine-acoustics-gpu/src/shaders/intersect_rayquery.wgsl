enable wgpu_ray_query;

// Tier 1: hardware ray query against the TLAS (one BLAS per mesh, instance
// custom data = instance index). Prepended to common.wgsl, which defines
// `Hit`, the scene buffers and the energy kernel.

@group(0) @binding(7) var scene_tlas: acceleration_structure;

fn intersect(origin: vec3<f32>, dir: vec3<f32>, t_min: f32, t_max: f32) -> Hit {
    var rq: ray_query;
    rayQueryInitialize(&rq, scene_tlas, RayDesc(RAY_FLAG_FORCE_OPAQUE, 0xffu, t_min, t_max, origin, dir));
    rayQueryProceed(&rq);
    let x = rayQueryGetCommittedIntersection(&rq);
    var hit: Hit;
    hit.kind = HIT_MISS;
    hit.t = t_max;
    hit.triangle = 0u;
    hit.instance = 0u;
    if x.kind == RAY_QUERY_INTERSECTION_TRIANGLE {
        let instance = x.instance_custom_data;
        hit.kind = HIT_TRIANGLE;
        hit.t = x.t;
        hit.instance = instance;
        hit.triangle = instances[instance].triangle_offset + x.primitive_index;
    }
    return hit;
}
