// engine.fog: height fog with wind-advected 3D noise on opaque geometry and
// sky pixels (docs/rendering-fog.md). CPU twin: fog/model.rs.
//
// Composed after scatter.wgsl with `#import engine::view` and the
// FOG_NOISE_MASK constant. The medium has extinction
//   sigma_t(p) = density * exp(-falloff * (y - base)) * (1 + amount * (2 n(p) - 1))
// and scatters with albedo and a Cornette-Shanks phase, so along a ray
//   L = scene * T + albedo * (sun * phase + ambient) * (1 - T),   T = exp(-tau)
// because the source term is constant along the ray.
struct Fog {
    // rgb scattering albedo, w Cornette-Shanks anisotropy.
    albedo: vec4<f32>,
    // x density at the base height (1/m), y height falloff (1/m), z camera
    // height above the base (m), w distance integrated for sky pixels (m).
    medium: vec4<f32>,
    // x noise amount, y noise frequency (cells per metre), z distance marched
    // with noise (m), w step count.
    noise: vec4<f32>,
    // xyz camera position in noise space (cells, wrapped; wind already applied).
    origin: vec4<f32>,
    // x sun intensity multiplier, y ambient intensity multiplier.
    lighting: vec4<f32>,
    // x noise seed.
    seed: vec4<u32>,
}

@group(1) @binding(0) var<uniform> fog: Fog;
@group(1) @binding(1) var scene_color: texture_2d<f32>;
@group(1) @binding(2) var scene_depth: texture_depth_2d;

const FOG_EXPONENT_LIMIT: f32 = 80.0;
const FOG_MAX_RADIANCE: f32 = 60000.0;

@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

// Optical depth of density * exp(-falloff * h) along a segment that starts at
// height h0 above the base and spans `len` metres with vertical cosine dy:
// d * (e^e0 - e^e1) / (k dy), and the series of (1 - e^-x) / x for short x.
fn fog_height_depth(h0: f32, dy: f32, len: f32) -> f32 {
    let d = fog.medium.x;
    let k = fog.medium.y;
    let e0 = min(-k * h0, FOG_EXPONENT_LIMIT);
    let x = k * dy * len;
    if abs(x) < 0.01 {
        let f = 1.0 - x * (0.5 - x * (1.0 / 6.0 - x * (1.0 / 24.0 - x / 120.0)));
        return d * len * exp(e0) * f;
    }
    let e1 = min(e0 - x, FOG_EXPONENT_LIMIT);
    return max(d * (exp(e0) - exp(e1)) / (k * dy), 0.0);
}

// Hash of a lattice cell (wrapped to the noise period) to [0, 1).
fn fog_cell_value(cell: vec3<i32>) -> f32 {
    let mask = i32(FOG_NOISE_MASK);
    var v = vec3<u32>(u32(cell.x & mask), u32(cell.y & mask), u32(cell.z & mask));
    v = v * 1664525u + vec3<u32>(1013904223u + fog.seed.x * 747796405u + 2891336453u);
    v.x += v.y * v.z;
    v.y += v.z * v.x;
    v.z += v.x * v.y;
    v = v ^ (v >> vec3<u32>(16u));
    v.x += v.y * v.z;
    v.y += v.z * v.x;
    v.z += v.x * v.y;
    return f32(v.x >> 8u) * (1.0 / 16777216.0);
}

// Value noise in [0, 1] with quintic interpolation; period FOG_NOISE_MASK + 1 cells.
fn fog_noise(c: vec3<f32>) -> f32 {
    let base = floor(c);
    let f = c - base;
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let b = vec3<i32>(base);
    let x00 = mix(fog_cell_value(b), fog_cell_value(b + vec3<i32>(1, 0, 0)), u.x);
    let x10 = mix(fog_cell_value(b + vec3<i32>(0, 1, 0)), fog_cell_value(b + vec3<i32>(1, 1, 0)), u.x);
    let x01 = mix(fog_cell_value(b + vec3<i32>(0, 0, 1)), fog_cell_value(b + vec3<i32>(1, 0, 1)), u.x);
    let x11 = mix(fog_cell_value(b + vec3<i32>(0, 1, 1)), fog_cell_value(b + vec3<i32>(1, 1, 1)), u.x);
    return mix(mix(x00, x10, u.y), mix(x01, x11, u.y), u.z);
}

// Optical depth along dir over `distance` metres: noise-modulated midpoint
// segments over the first `noise.z` metres, the analytic height integral beyond.
fn fog_optical_depth(dir: vec3<f32>, distance: f32) -> f32 {
    let h0 = fog.medium.z;
    let amount = fog.noise.x;
    var tau = 0.0;
    var marched = 0.0;
    if amount > 0.0 {
        let steps = u32(fog.noise.w);
        marched = min(distance, fog.noise.z);
        let dt = marched / f32(steps);
        for (var i = 0u; i < steps; i++) {
            let start = f32(i) * dt;
            let n = fog_noise(fog.origin.xyz + dir * ((start + 0.5 * dt) * fog.noise.y));
            let modulation = 1.0 + amount * (2.0 * n - 1.0);
            tau += fog_height_depth(h0 + dir.y * start, dir.y, dt) * modulation;
        }
    }
    if distance > marched {
        tau += fog_height_depth(h0 + dir.y * marched, dir.y, distance - marched);
    }
    return tau;
}

@fragment fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(frag.xy);
    let color = textureLoad(scene_color, pixel, 0);
    let depth = textureLoad(scene_depth, pixel, 0);
    var distance = fog.medium.w;
    if depth < 1.0 {
        distance = scatter_view_distance(frag.xy, depth);
    }
    if distance <= 0.0 {
        return color;
    }
    let dir = scatter_view_direction(frag.xy);
    let transmittance = exp(-fog_optical_depth(dir, distance));
    var source = engine_view.ambient.rgb * fog.lighting.y;
    var sun = vec3<f32>(0.0, 1.0, 0.0);
    var irradiance = vec3<f32>(0.0);
    if scatter_sun(&sun, &irradiance) {
        source += irradiance * (fog.lighting.x * scatter_cornette_shanks(dot(dir, sun), fog.albedo.w));
    }
    let inscatter = min(fog.albedo.rgb * source, vec3<f32>(FOG_MAX_RADIANCE)) * (1.0 - transmittance);
    return vec4<f32>(color.rgb * transmittance + inscatter, color.a);
}
