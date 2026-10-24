// Reference progressive path tracer. Appended to the ray-query library at
// group 0.
//
// Lambertian albedo and emission per material, a sky whose radiance is
// horizon + (zenith - horizon) * max(dir.y, 0) (constant when both are equal),
// cosine-weighted hemisphere sampling around the face-forwarded geometric
// normal, Russian roulette from bounce rr_start, and a running mean in
// Rgba32Float (w = accumulated sample count).

struct PtMaterial {
    albedo: vec4<f32>,
    emissive: vec4<f32>,
}

struct PtParams {
    inv_view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    sky_zenith: vec4<f32>,
    sky_horizon: vec4<f32>,
    // width, height, frames already accumulated, max bounces
    a: vec4<u32>,
    // rr_start, seed, instance mask, material count
    b: vec4<u32>,
    // x: t_max, y: jitter (0 or 1), z: ray offset scale
    c: vec4<f32>,
}

@group(1) @binding(0) var pt_previous: texture_2d<f32>;
@group(1) @binding(1) var pt_output: texture_storage_2d<rgba32float, write>;
@group(1) @binding(2) var<storage, read> pt_materials: array<PtMaterial>;
@group(1) @binding(3) var<uniform> pt_params: PtParams;
// [0]: samples discarded because they were not finite.
@group(1) @binding(4) var<storage, read_write> pt_stats: array<atomic<u32>, 4>;

var<private> pt_rng: u32;

fn pt_pcg(v: u32) -> u32 {
    let state = v * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

fn pt_rand() -> f32 {
    pt_rng = pt_pcg(pt_rng);
    return f32(pt_rng >> 8u) * (1.0 / 16777216.0);
}

fn pt_sky(dir: vec3<f32>) -> vec3<f32> {
    let h = pt_params.sky_horizon.xyz;
    return h + (pt_params.sky_zenith.xyz - h) * max(dir.y, 0.0);
}

// Cosine-weighted direction about unit n (Duff et al. 2017 basis).
fn pt_cosine_sample(n: vec3<f32>) -> vec3<f32> {
    let r1 = pt_rand();
    let r2 = pt_rand();
    let phi = 6.283185307179586 * r1;
    let r = sqrt(r2);
    let local = vec3<f32>(r * cos(phi), r * sin(phi), sqrt(max(0.0, 1.0 - r2)));
    let s = select(-1.0, 1.0, n.z >= 0.0);
    let a = -1.0 / (s + n.z);
    let b = n.x * n.y * a;
    let t = vec3<f32>(1.0 + s * n.x * n.x * a, s * b, -s * n.x);
    let bt = vec3<f32>(b, s + n.y * n.y * a, -n.y);
    return normalize(t * local.x + bt * local.y + n * local.z);
}

fn pt_material(index: u32) -> PtMaterial {
    return pt_materials[min(index, pt_params.b.w - 1u)];
}

fn pt_radiance(origin_in: vec3<f32>, dir_in: vec3<f32>) -> vec3<f32> {
    var origin = origin_in;
    var dir = dir_in;
    var radiance = vec3<f32>(0.0);
    var throughput = vec3<f32>(1.0);
    let max_bounces = pt_params.a.w;
    let rr_start = pt_params.b.x;
    let mask = pt_params.b.z;
    let t_max = pt_params.c.x;
    for (var bounce = 0u; bounce <= max_bounces; bounce += 1u) {
        let hit = rt_closest_hit(origin, dir, 0.0, t_max, mask);
        if !hit.hit {
            radiance += throughput * pt_sky(dir);
            break;
        }
        let surface = rt_surface(hit, origin, dir);
        let material = pt_material(surface.material_index);
        radiance += throughput * material.emissive.xyz;
        if bounce == max_bounces {
            break;
        }
        var n = surface.geometric_normal;
        if dot(n, dir) > 0.0 {
            n = -n;
        }
        // Lambertian with cosine sampling: f cos / pdf = albedo.
        throughput *= material.albedo.xyz;
        if bounce + 1u >= rr_start {
            let p = clamp(max(throughput.x, max(throughput.y, throughput.z)), 0.05, 0.95);
            if pt_rand() >= p {
                break;
            }
            throughput /= p;
        }
        let offset = pt_params.c.z * (1.0 + max(abs(surface.position.x), max(abs(surface.position.y), abs(surface.position.z))));
        origin = surface.position + n * offset;
        dir = pt_cosine_sample(n);
    }
    return radiance;
}

@compute @workgroup_size(8, 8)
fn path_trace(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = pt_params.a.xy;
    if id.x >= size.x || id.y >= size.y {
        return;
    }
    let frame = pt_params.a.z;
    pt_rng = pt_pcg(id.x + id.y * size.x + pt_pcg(frame ^ pt_pcg(pt_params.b.y)));
    var jitter = vec2<f32>(0.5);
    if pt_params.c.y > 0.5 {
        jitter = vec2<f32>(pt_rand(), pt_rand());
    }
    let pixel = vec2<f32>(id.xy) + jitter;
    let ndc = vec2<f32>(pixel.x / f32(size.x) * 2.0 - 1.0, 1.0 - pixel.y / f32(size.y) * 2.0);
    let far = pt_params.inv_view_proj * vec4<f32>(ndc, 0.5, 1.0);
    let eye = pt_params.eye.xyz;
    let dir = normalize(far.xyz / far.w - eye);
    var sample = pt_radiance(eye, dir);
    let finite = all(sample == sample) && all(abs(sample) <= vec3<f32>(3.0e38));
    var previous = vec4<f32>(0.0);
    if frame > 0u {
        previous = textureLoad(pt_previous, id.xy, 0);
    }
    if !finite {
        atomicAdd(&pt_stats[0], 1u);
        sample = previous.xyz;
    }
    let count = f32(frame + 1u);
    let mean = previous.xyz + (sample - previous.xyz) / count;
    textureStore(pt_output, id.xy, vec4<f32>(mean, count));
}
