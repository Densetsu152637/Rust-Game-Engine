// engine-render-rt shading library (docs/rendering-raytracing.md, "WGSL library").
//
// Random numbers, hemisphere/cone/GGX sampling, the analytic sky, hit-material
// records and sun lighting shared by the RT kernels. `wgsl::compose_kernel`
// appends it after the ray-query library (it calls `rt_occluded`); it declares
// no bindings of its own.

const RT_PI: f32 = 3.141592653589793;
const RT_TAU: f32 = 6.283185307179586;

// `RtMaterial` records (engine_render_rt::RtMaterial).
struct RtMaterialRecord {
    albedo: vec4<f32>,
    emissive: vec4<f32>,
}

var<private> rt_rng_state: u32;

// PCG hash (Jarzynski and Olano 2020).
fn rt_pcg(v: u32) -> u32 {
    let state = v * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

// Seeds the per-invocation generator from a pixel, the frame index and a seed.
fn rt_seed(pixel: vec2<u32>, width: u32, frame: u32, seed: u32) {
    rt_rng_state = rt_pcg(pixel.x + pixel.y * width + rt_pcg(frame ^ rt_pcg(seed)));
}

// Uniform in [0, 1).
fn rt_rand() -> f32 {
    rt_rng_state = rt_pcg(rt_rng_state);
    return f32(rt_rng_state >> 8u) * (1.0 / 16777216.0);
}

// Orthonormal basis whose third column is unit `n` (Duff et al. 2017).
fn rt_basis(n: vec3<f32>) -> mat3x3<f32> {
    let s = select(-1.0, 1.0, n.z >= 0.0);
    let a = -1.0 / (s + n.z);
    let b = n.x * n.y * a;
    let t = vec3<f32>(1.0 + s * n.x * n.x * a, s * b, -s * n.x);
    let bt = vec3<f32>(b, s + n.y * n.y * a, -n.y);
    return mat3x3<f32>(t, bt, n);
}

// Cosine-weighted direction about unit `n` (pdf = cos / pi).
fn rt_cosine_sample(n: vec3<f32>) -> vec3<f32> {
    let r1 = rt_rand();
    let r2 = rt_rand();
    let phi = RT_TAU * r1;
    let r = sqrt(r2);
    let local = vec3<f32>(r * cos(phi), r * sin(phi), sqrt(max(0.0, 1.0 - r2)));
    return normalize(rt_basis(n) * local);
}

// Uniform direction inside the cone of half-angle acos(cos_max) about unit `d`.
fn rt_cone_sample(d: vec3<f32>, cos_max: f32) -> vec3<f32> {
    let cos_t = 1.0 - rt_rand() * (1.0 - cos_max);
    let sin_t = sqrt(max(0.0, 1.0 - cos_t * cos_t));
    let phi = RT_TAU * rt_rand();
    return normalize(rt_basis(d) * vec3<f32>(sin_t * cos(phi), sin_t * sin(phi), cos_t));
}

// GGX visible-normal sample (Heitz 2018) in the local frame (normal = +z):
// `v` is the unit view direction, `alpha` the GGX roughness (perceptual^2).
fn rt_ggx_vndf(v: vec3<f32>, alpha: f32) -> vec3<f32> {
    let u1 = rt_rand();
    let u2 = rt_rand();
    let vh = normalize(vec3<f32>(alpha * v.x, alpha * v.y, v.z));
    let lensq = vh.x * vh.x + vh.y * vh.y;
    var t1 = vec3<f32>(1.0, 0.0, 0.0);
    if lensq > 0.0 {
        t1 = vec3<f32>(-vh.y, vh.x, 0.0) * inverseSqrt(lensq);
    }
    let t2 = cross(vh, t1);
    let r = sqrt(u1);
    let phi = RT_TAU * u2;
    let p1 = r * cos(phi);
    let s = 0.5 * (1.0 + vh.z);
    let p2 = (1.0 - s) * sqrt(max(0.0, 1.0 - p1 * p1)) + s * r * sin(phi);
    let nh = p1 * t1 + p2 * t2 + sqrt(max(0.0, 1.0 - p1 * p1 - p2 * p2)) * vh;
    return normalize(vec3<f32>(alpha * nh.x, alpha * nh.y, max(0.0, nh.z)));
}

// Analytic sky radiance: horizon + (zenith - horizon) * max(dir.y, 0).
fn rt_sky(dir: vec3<f32>, zenith: vec3<f32>, horizon: vec3<f32>) -> vec3<f32> {
    return horizon + (zenith - horizon) * max(dir.y, 0.0);
}

// Ray-origin offset that keeps `bias` meaningful far from the origin.
fn rt_offset(position: vec3<f32>, bias: f32) -> f32 {
    return bias * (1.0 + max(abs(position.x), max(abs(position.y), abs(position.z))));
}

fn rt_all_finite(v: vec3<f32>) -> bool {
    return all(v == v) && all(abs(v) <= vec3<f32>(3.0e38));
}

// `RtLighting` (engine_render_rt::RtLighting): xyz used, w unused.
struct RtLightingRecord {
    // Unit direction towards the sun.
    sun_direction: vec4<f32>,
    // Sun irradiance on a surface facing it.
    sun_illuminance: vec4<f32>,
    sky_zenith: vec4<f32>,
    sky_horizon: vec4<f32>,
}

// Unit `n` flipped to face against `dir` (the incoming ray direction).
fn rt_face_forward(n: vec3<f32>, dir: vec3<f32>) -> vec3<f32> {
    return select(n, -n, dot(n, dir) > 0.0);
}

// Sun light reflected by a Lambertian surface of `albedo` at `position` with
// unit normal `n`: albedo / pi * E_sun * max(n . l, 0) * visibility (one
// shadow ray towards the sun centre, origin offset by `bias` along `n`).
fn rt_sun_diffuse(lighting: RtLightingRecord, position: vec3<f32>, n: vec3<f32>, albedo: vec3<f32>, bias: f32, t_max: f32, mask: u32) -> vec3<f32> {
    let l = lighting.sun_direction.xyz;
    let cos_l = dot(n, l);
    let reflected = albedo * lighting.sun_illuminance.xyz;
    if cos_l <= 0.0 || all(reflected == vec3<f32>(0.0)) {
        return vec3<f32>(0.0);
    }
    if rt_occluded(position + n * bias, l, 0.0, t_max, mask) {
        return vec3<f32>(0.0);
    }
    return reflected * (cos_l / RT_PI);
}
