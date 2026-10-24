// engine::brdf - the engine's light-independent shading math, shared by the
// 3D and sprite implementations of engine::material.
const ENGINE_PI: f32 = 3.14159265;

// The surface a material lights.
struct EngineSurface {
    // Linear base colour.
    albedo: vec3<f32>,
    metallic: f32,
    // Unit world-space normal (facing the viewer for two-sided surfaces).
    normal: vec3<f32>,
    // Perceptual roughness 0..1 (squared for GGX; floored at 0.045).
    roughness: f32,
}

// (1 - (d / range)^4)^2: 1 near the light, 0 at and beyond `range`.
fn engine_range_window(distance: f32, range: f32) -> f32 {
    let ratio = distance / range;
    let square = ratio * ratio;
    let window = clamp(1.0 - square * square, 0.0, 1.0);
    return window * window;
}

// Windowed inverse-square falloff (distances below 1 cm are clamped).
fn engine_distance_falloff(distance: f32, range: f32) -> f32 {
    return engine_range_window(distance, range) / max(distance * distance, 0.0001);
}

struct EngineBrdf {
    diffuse: vec3<f32>,
    specular: vec3<f32>,
}

// GGX / Trowbridge-Reitz, separable Smith, Schlick Fresnel and
// energy-weighted Lambert, for light direction `l`.
fn engine_brdf(surface: EngineSurface, v: vec3<f32>, l: vec3<f32>) -> EngineBrdf {
    let n = surface.normal;
    let roughness = max(surface.roughness, 0.045);
    let alpha = roughness * roughness;
    let a2 = alpha * alpha;
    let nv = max(dot(n, v), 0.0001);
    let nl = max(dot(n, l), 0.0);
    let half_vector = l + v;
    let h = half_vector / max(length(half_vector), 0.000001);
    let nh = max(dot(n, h), 0.0);
    let vh = max(dot(v, h), 0.0);
    let f0 = mix(vec3<f32>(0.04), surface.albedo, surface.metallic);
    let denom = nh * nh * (a2 - 1.0) + 1.0;
    let distribution = a2 / max(ENGINE_PI * denom * denom, 0.0000001);
    let gv = 2.0 * nv / (nv + sqrt(a2 + (1.0 - a2) * nv * nv));
    let gl = 2.0 * nl / max(nl + sqrt(a2 + (1.0 - a2) * nl * nl), 0.000001);
    let fresnel = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - vh, 5.0);
    var brdf: EngineBrdf;
    brdf.specular = distribution * gv * gl * fresnel / max(4.0 * nv * nl, 0.000001);
    brdf.diffuse = (vec3<f32>(1.0) - fresnel) * (1.0 - surface.metallic) * surface.albedo / ENGINE_PI;
    return brdf;
}
