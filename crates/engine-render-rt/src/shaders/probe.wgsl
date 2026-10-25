// Probe kernel: one invocation per ray, used by the GPU self-test and
// RtDevice::cast_rays. Appended to the ray-query library at group 0.

struct ProbeRay {
    origin: vec3<f32>,
    t_min: f32,
    dir: vec3<f32>,
    t_max: f32,
    mask: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

struct ProbeHit {
    t: f32,
    u: f32,
    v: f32,
    flags: u32,
    custom_index: u32,
    instance_index: u32,
    primitive_index: u32,
    material_index: u32,
    geometric_normal: vec3<f32>,
    _pad0: f32,
    shading_normal: vec3<f32>,
    _pad1: f32,
    position: vec3<f32>,
    _pad2: f32,
}

struct ProbeParams {
    count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(1) @binding(0) var<storage, read> probe_rays: array<ProbeRay>;
@group(1) @binding(1) var<storage, read_write> probe_hits: array<ProbeHit>;
@group(1) @binding(2) var<uniform> probe_params: ProbeParams;

@compute @workgroup_size(64)
fn probe(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= probe_params.count {
        return;
    }
    let ray = probe_rays[i];
    let hit = rt_closest_hit(ray.origin, ray.dir, ray.t_min, ray.t_max, ray.mask);
    var out: ProbeHit;
    out.t = hit.t;
    if hit.hit {
        let surface = rt_surface(hit, ray.origin, ray.dir);
        out.u = hit.barycentrics.x;
        out.v = hit.barycentrics.y;
        out.flags = 1u;
        if hit.front_face {
            out.flags = out.flags | 2u;
        }
        out.custom_index = hit.custom_index;
        out.instance_index = hit.instance_index;
        out.primitive_index = hit.primitive_index;
        out.material_index = surface.material_index;
        out.geometric_normal = surface.geometric_normal;
        out.shading_normal = surface.shading_normal;
        out.position = surface.position;
    }
    if rt_occluded(ray.origin, ray.dir, ray.t_min, ray.t_max, ray.mask) {
        out.flags = out.flags | 4u;
    }
    probe_hits[i] = out;
}
