// Stochastic acoustic energy tracer shared by both GPU tiers
// (docs/spatial-audio-design.md section 5.4.4). An `intersect` function is
// supplied by intersect_rayquery.wgsl (tier 1) or intersect_bvh.wgsl (tier 2).
//
// One invocation traces one ray of one job: x = ray, y = job. Energies are
// accumulated with integer atomics as 64-bit fixed point (two u32 words with
// carry), so the sums do not depend on scheduling order and the histogram is
// deterministic for a fixed seed.

struct Settings {
    air: vec4<f32>,
    bin_s: f32,
    bins: u32,
    speed_of_sound: f32,
    job_count: u32,
    top_base: u32,
    instance_count: u32,
    probe_count: u32,
    _pad: u32,
}

struct Job {
    emitter: vec4<f32>,
    // xyz = receiver centre, w = receiver radius.
    receiver: vec4<f32>,
    // All rays of the job; deposits carry 1 / rays.
    rays: u32,
    max_bounces: u32,
    seed_lo: u32,
    seed_hi: u32,
    // This dispatch traces rays first_ray .. first_ray + count.
    first_ray: u32,
    count: u32,
    _pad0: u32,
    _pad1: u32,
}

struct Triangle {
    v0: vec4<f32>,
    v1: vec4<f32>,
    v2: vec4<f32>,
}

struct Instance {
    world_from_object: array<vec4<f32>, 3>,
    object_from_world: array<vec4<f32>, 3>,
    node_offset: u32,
    triangle_offset: u32,
    mask: u32,
    _pad: u32,
}

struct Material {
    reflect: vec4<f32>,
    transmission: vec4<f32>,
    scattering: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

struct Hit {
    kind: u32,
    t: f32,
    triangle: u32,
    instance: u32,
}

// Probe slot: rays in, nearest hits out (self-tests and parity tests).
struct ProbeSlot {
    // xyz origin, w = t_max.
    origin: vec4<f32>,
    dir: vec4<f32>,
    // x = kind, y = t bits, z = triangle, w = instance.
    result: vec4<u32>,
}

const HIT_MISS: u32 = 0u;
const HIT_TRIANGLE: u32 = 1u;
const HIT_ABORTED: u32 = 2u;

@group(0) @binding(0) var<uniform> settings: Settings;
@group(0) @binding(1) var<storage, read> jobs: array<Job>;
@group(0) @binding(2) var<storage, read> triangles: array<Triangle>;
@group(0) @binding(3) var<storage, read> instances: array<Instance>;
@group(0) @binding(4) var<storage, read> materials: array<Material>;
// [job][bin][band][lo, hi], 2^FIXED_SHIFT per unit of energy.
@group(0) @binding(5) var<storage, read_write> histogram: array<atomic<u32>>;
// STATS_WORDS per job, see the STAT_* offsets.
@group(0) @binding(6) var<storage, read_write> job_stats: array<atomic<u32>>;
@group(0) @binding(8) var<storage, read_write> probe: array<ProbeSlot>;

const STATS_WORDS: u32 = 16u;
const STAT_ABORTED: u32 = 0u;
const STAT_FLAGS: u32 = 1u;
const STAT_SEGMENTS: u32 = 2u;       // lo, hi: all traced segments
const STAT_FREE_PATHS: u32 = 4u;     // lo, hi: segments between two reflections
const STAT_FREE_LENGTH: u32 = 6u;    // lo, hi: their summed length, LENGTH_SCALE per metre
const STAT_ESCAPED: u32 = 8u;        // 4 x (lo, hi): escaped energy per band
const FLAG_SATURATED: u32 = 1u;
const FLAG_NONFINITE: u32 = 2u;

// 2^40 per unit of energy (per job, total emitted energy is 1).
const ENERGY_SCALE_HI: f32 = 256.0;  // 2^40 / 2^32
const LENGTH_SCALE_HI: f32 = 1.52587890625e-5; // 2^16 / 2^32: 2^16 per metre

const PI: f32 = 3.14159265358979;
const T_MAX: f32 = 1e30;
// engine_acoustics::ray_offset: RAY_OFFSET_ABS_M + RAY_OFFSET_REL * max |p|.
const RAY_OFFSET_ABS_M: f32 = 1e-4;
const RAY_OFFSET_REL: f32 = 4e-6;
const MIN_ENERGY: f32 = 1e-6;

// ---- RNG: PCG hash on wrapping u32 (identical to the CPU tracer) ----

fn pcg(v: u32) -> u32 {
    let s = v * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

// engine_acoustics::rng::ray_state: pcg(pcg(lo ^ pcg(hi)) ^ ray).
fn ray_seed(job: Job, ray: u32) -> u32 {
    return pcg(pcg(job.seed_lo ^ pcg(job.seed_hi)) ^ ray);
}

// Uniform in [0, 1) with 24 bits.
fn next_unit(state: ptr<function, u32>) -> f32 {
    *state = pcg(*state);
    return f32(*state >> 8u) * (1.0 / 16777216.0);
}

fn sample_sphere(state: ptr<function, u32>) -> vec3<f32> {
    let z = 1.0 - 2.0 * next_unit(state);
    let phi = 2.0 * PI * next_unit(state);
    let r = sqrt(max(0.0, 1.0 - z * z));
    return vec3<f32>(r * cos(phi), r * sin(phi), z);
}

fn orthonormal_basis(n: vec3<f32>) -> mat3x3<f32> {
    // Duff et al. 2017, "Building an orthonormal basis, revisited".
    let s = select(-1.0, 1.0, n.z >= 0.0);
    let a = -1.0 / (s + n.z);
    let b = n.x * n.y * a;
    let t = vec3<f32>(1.0 + s * n.x * n.x * a, s * b, -s * n.x);
    let bt = vec3<f32>(b, s + n.y * n.y * a, -n.y);
    return mat3x3<f32>(t, bt, n);
}

fn cosine_hemisphere(n: vec3<f32>, u1: f32, u2: f32) -> vec3<f32> {
    let r = sqrt(u1);
    let phi = 2.0 * PI * u2;
    let local = vec3<f32>(r * cos(phi), r * sin(phi), sqrt(max(0.0, 1.0 - u1)));
    return orthonormal_basis(n) * local;
}

// ---- Fixed-point atomics ----

// Splits `v` (in units of 2^32 counts, v >= 0) into (lo, hi) words of a 64-bit
// fixed-point value, rounding the low word to nearest (with carry).
fn split_fixed(v: f32) -> vec2<u32> {
    var hi = floor(v);
    let lo_f = round((v - hi) * 4294967296.0);
    var lo = 0u;
    if lo_f >= 4294967296.0 {
        hi += 1.0;
    } else {
        lo = u32(lo_f);
    }
    return vec2<u32>(lo, u32(hi));
}

// Adds `v` (in units of 2^32 counts) to the 64-bit stats counter at `index`.
fn add_stat_fixed(index: u32, v: f32) {
    let parts = split_fixed(v);
    let old = atomicAdd(&job_stats[index], parts.x);
    let add_hi = parts.y + select(0u, 1u, old > 0xffffffffu - parts.x);
    if add_hi != 0u {
        atomicAdd(&job_stats[index + 1u], add_hi);
    }
}

// Infinity or NaN in any lane, from the exponent bits (not `x != x`, which a
// compiler may fold away under fast-math assumptions).
fn not_finite(v: vec4<f32>) -> bool {
    let exponent = bitcast<vec4<u32>>(v) & vec4<u32>(0x7f800000u);
    return any(exponent == vec4<u32>(0x7f800000u));
}

fn flag(job_index: u32, bit: u32) {
    atomicOr(&job_stats[job_index * STATS_WORDS + STAT_FLAGS], bit);
}

fn add_count(job_index: u32, word: u32, n: u32) {
    let i = job_index * STATS_WORDS + word;
    let old = atomicAdd(&job_stats[i], n);
    if old > 0xffffffffu - n {
        atomicAdd(&job_stats[i + 1u], 1u);
    }
}

fn deposit_energy(job_index: u32, bin: u32, energy: vec4<f32>) {
    if not_finite(energy) || any(abs(energy) > vec4<f32>(1e30)) {
        flag(job_index, FLAG_NONFINITE);
        return;
    }
    let base = ((job_index * settings.bins + bin) * 4u) * 2u;
    for (var b = 0u; b < 4u; b += 1u) {
        let v = energy[b] * ENERGY_SCALE_HI;
        if v >= 4294967295.0 {
            flag(job_index, FLAG_SATURATED);
            continue;
        }
        if v > 0.0 {
            add_hi_checked(job_index, base + b * 2u, v);
        }
    }
}

// Histogram add that flags saturation of the high word.
fn add_hi_checked(job_index: u32, index: u32, v: f32) {
    let parts = split_fixed(v);
    let old = atomicAdd(&histogram[index], parts.x);
    let carry = select(0u, 1u, old > 0xffffffffu - parts.x);
    let add_hi = parts.y + carry;
    if add_hi != 0u {
        let old_hi = atomicAdd(&histogram[index + 1u], add_hi);
        if old_hi > 0xffffffffu - add_hi {
            flag(job_index, FLAG_SATURATED);
        }
    }
}

// ---- Geometry helpers ----

fn world_normal(hit: Hit) -> vec3<f32> {
    let tri = triangles[hit.triangle];
    let n = cross(tri.v1.xyz - tri.v0.xyz, tri.v2.xyz - tri.v0.xyz);
    // Normal matrix = transpose(object_from_world).
    let m = instances[hit.instance].object_from_world;
    let w = n.x * m[0].xyz + n.y * m[1].xyz + n.z * m[2].xyz;
    // As the CPU: a zero or non-finite normal falls back to +Y.
    let len = length(w);
    if not_finite(vec4<f32>(w, len)) || !(len > 0.0) {
        return vec3<f32>(0.0, 1.0, 0.0);
    }
    return w / len;
}

fn material_of(hit: Hit) -> Material {
    return materials[bitcast<u32>(triangles[hit.triangle].v0.w)];
}

// Deposits the part of segment [p, p + d * len] inside the receiver sphere.
// Mirrors engine_acoustics::trace_cpu (sphere_chord and the receiver deposit):
// e * exp(-2 m t_mid) * 4 pi l / V_rcv / rays into bin floor((path + t_mid) / c / bin_s).
fn receive(job_index: u32, job: Job, p: vec3<f32>, d: vec3<f32>, len: f32, travelled: f32, energy: vec4<f32>) {
    let r = job.receiver.w;
    let oc = p - job.receiver.xyz;
    let b = dot(d, oc);
    let c = dot(oc, oc) - r * r;
    let disc = b * b - c;
    if disc <= 0.0 {
        return;
    }
    let s = sqrt(disc);
    let t0 = max(-b - s, 0.0);
    let t1 = min(-b + s, len);
    if t1 <= t0 {
        return;
    }
    let inside = t1 - t0;
    let time = (travelled + t0 + 0.5 * inside) / settings.speed_of_sound;
    let bin_f = floor(time / settings.bin_s);
    if bin_f >= f32(settings.bins) {
        return;
    }
    let t_mid = t0 + 0.5 * inside;
    let attenuated = energy * exp(-2.0 * settings.air * t_mid);
    // 4 pi l / V_rcv with V_rcv = 4/3 pi r^3, per ray.
    let weight = 3.0 * inside / (r * r * r) / f32(job.rays);
    deposit_energy(job_index, u32(bin_f), attenuated * weight);
}

@compute @workgroup_size(64)
fn trace_energy(@builtin(global_invocation_id) gid: vec3<u32>) {
    let job_index = gid.y;
    if job_index >= settings.job_count {
        return;
    }
    let job = jobs[job_index];
    if gid.x >= job.count {
        return;
    }
    let ray = job.first_ray + gid.x;
    // The loop mirrors engine_acoustics::trace_cpu::trace_chunk step for step:
    // relative energy 1 per band (deposits carry 1 / rays), air absorption
    // exp(-2 m t) on energy, `max_bounces` reflections, then the hit ends the ray.
    var state = ray_seed(job, ray);
    var dir = sample_sphere(&state);
    var pos = job.emitter.xyz;
    let inv_rays = 1.0 / f32(job.rays);
    var energy = vec4<f32>(1.0);
    var travelled = 0.0;
    var bounces = 0u;
    var segments = 0u;
    var free_paths = 0u;
    var free_length = 0.0;
    loop {
        let hit = intersect(pos, dir, 0.0, T_MAX);
        if hit.kind == HIT_ABORTED {
            add_count(job_index, STAT_ABORTED, 1u);
            break;
        }
        segments += 1u;
        let len = select(T_MAX, hit.t, hit.kind == HIT_TRIANGLE);
        if bounces >= 1u {
            receive(job_index, job, pos, dir, len, travelled, energy);
        }
        if hit.kind == HIT_MISS {
            let escaped_base = job_index * STATS_WORDS + STAT_ESCAPED;
            for (var b = 0u; b < 4u; b += 1u) {
                add_stat_fixed(escaped_base + b * 2u, energy[b] * inv_rays * ENERGY_SCALE_HI);
            }
            break;
        }
        if bounces >= 1u {
            free_paths += 1u;
            free_length += hit.t;
        }
        travelled += hit.t;
        let m = material_of(hit);
        energy = energy * (exp(-2.0 * settings.air * hit.t) * m.reflect);
        if bounces >= job.max_bounces || max(max(energy.x, energy.y), max(energy.z, energy.w)) < MIN_ENERGY {
            break;
        }
        bounces += 1u;
        var n = world_normal(hit);
        if dot(n, dir) > 0.0 {
            n = -n;
        }
        let hit_point = pos + dir * hit.t;
        // Exactly three draws per reflection, used or not (rng.rs protocol).
        let u_scatter = next_unit(&state);
        let u1 = next_unit(&state);
        let u2 = next_unit(&state);
        if u_scatter < m.scattering {
            dir = cosine_hemisphere(n, u1, u2);
        } else {
            dir = dir - 2.0 * dot(dir, n) * n;
        }
        let a = abs(hit_point);
        pos = hit_point + n * (RAY_OFFSET_ABS_M + RAY_OFFSET_REL * max(a.x, max(a.y, a.z)));
    }
    add_count(job_index, STAT_SEGMENTS, segments);
    add_count(job_index, STAT_FREE_PATHS, free_paths);
    add_stat_fixed(job_index * STATS_WORDS + STAT_FREE_LENGTH, free_length * LENGTH_SCALE_HI);
}

@compute @workgroup_size(64)
fn cast_rays(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= settings.probe_count {
        return;
    }
    let slot = probe[i];
    let hit = intersect(slot.origin.xyz, slot.dir.xyz, 0.0, slot.origin.w);
    probe[i].result = vec4<u32>(hit.kind, bitcast<u32>(hit.t), hit.triangle, hit.instance);
}
