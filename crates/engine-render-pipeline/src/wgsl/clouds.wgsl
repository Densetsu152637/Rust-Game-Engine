// engine.clouds: a ray-marched flat cloud layer over sky and geometry
// (docs/rendering-clouds.md). CPU twin: clouds/model.rs.
//
// Composed after scatter.wgsl with `#import engine::view` and the CLOUD_*
// constants injected by clouds/mod.rs from the CPU reference. The extinction is
//   sigma_t = density * remap(mix(1, fbm(c), amount), coverage) * profile(h)
// and the pixel is scene * T' + inscatter * fade (see model.rs).
struct Clouds {
    // rgb scattering albedo, w forward Cornette-Shanks anisotropy.
    albedo: vec4<f32>,
    // x layer thickness (m), y camera height above the base (m), z maximum
    // march distance (m), w horizon fade distance (m, 0 = off).
    layer: vec4<f32>,
    // x density (1/m), y coverage, z noise amount, w noise frequency (cells/m).
    density: vec4<f32>,
    // x octaves, y primary steps, z light steps, w light distance (m).
    steps: vec4<f32>,
    // xyz camera in noise space (cells, wrapped; wind already applied),
    // w multiple scattering flag.
    origin: vec4<f32>,
    // x sun intensity multiplier, y ambient intensity multiplier.
    lighting: vec4<f32>,
    // x noise seed.
    seed: vec4<u32>,
}

@group(1) @binding(0) var<uniform> clouds: Clouds;
@group(1) @binding(1) var scene_color: texture_2d<f32>;
@group(1) @binding(2) var scene_depth: texture_depth_2d;

const CLOUD_MAX_RADIANCE: f32 = 60000.0;

@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

// Hash of a lattice cell (wrapped to the noise period) to [0, 1).
fn cloud_cell_value(cell: vec3<i32>, seed: u32) -> f32 {
    let mask = i32(CLOUD_NOISE_MASK);
    var v = vec3<u32>(u32(cell.x & mask), u32(cell.y & mask), u32(cell.z & mask));
    v = v * 1664525u + vec3<u32>(1013904223u + seed * 747796405u + 2891336453u);
    v.x += v.y * v.z;
    v.y += v.z * v.x;
    v.z += v.x * v.y;
    v = v ^ (v >> vec3<u32>(16u));
    v.x += v.y * v.z;
    v.y += v.z * v.x;
    v.z += v.x * v.y;
    return f32(v.x >> 8u) * (1.0 / 16777216.0);
}

// Value noise in [0, 1] with quintic interpolation (the fog's noise).
fn cloud_noise(c: vec3<f32>, seed: u32) -> f32 {
    let base = floor(c);
    let f = c - base;
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let b = vec3<i32>(base);
    let x00 = mix(cloud_cell_value(b, seed), cloud_cell_value(b + vec3<i32>(1, 0, 0), seed), u.x);
    let x10 = mix(cloud_cell_value(b + vec3<i32>(0, 1, 0), seed), cloud_cell_value(b + vec3<i32>(1, 1, 0), seed), u.x);
    let x01 = mix(cloud_cell_value(b + vec3<i32>(0, 0, 1), seed), cloud_cell_value(b + vec3<i32>(1, 0, 1), seed), u.x);
    let x11 = mix(cloud_cell_value(b + vec3<i32>(0, 1, 1), seed), cloud_cell_value(b + vec3<i32>(1, 1, 1), seed), u.x);
    return mix(mix(x00, x10, u.y), mix(x01, x11, u.y), u.z);
}

// Normalised fractal noise in [0, 1].
fn cloud_fbm(c: vec3<f32>) -> f32 {
    let octaves = u32(clouds.steps.x);
    var sum = 0.0;
    var norm = 0.0;
    var amplitude = 1.0;
    var frequency = 1.0;
    for (var i = 0u; i < octaves; i++) {
        sum += amplitude * cloud_noise(c * frequency, clouds.seed.x + i);
        norm += amplitude;
        amplitude *= 0.5;
        frequency *= 2.0;
    }
    return sum / norm;
}

// Extinction (1/m) at noise-space point c and y metres above the base.
fn cloud_extinction(c: vec3<f32>, y: f32) -> f32 {
    let h = y / clouds.layer.x;
    if h <= 0.0 || h >= 1.0 {
        return 0.0;
    }
    let profile = smoothstep(0.0, CLOUD_PROFILE_RISE, h) * (1.0 - smoothstep(CLOUD_PROFILE_FALL, 1.0, h));
    var n = 1.0;
    if clouds.density.z > 0.0 {
        n = 1.0 + clouds.density.z * (cloud_fbm(c) - 1.0);
    }
    let coverage = clouds.density.y;
    let shaped = clamp((n - (1.0 - coverage)) / max(coverage, 1.0e-3), 0.0, 1.0);
    return clouds.density.x * shaped * profile;
}

// Dual-lobe Cornette-Shanks phase; `scale` shrinks both asymmetries.
fn cloud_phase(mu: f32, scale: f32) -> f32 {
    return (1.0 - CLOUD_BACK_WEIGHT) * scatter_cornette_shanks(mu, clouds.albedo.w * scale)
        + CLOUD_BACK_WEIGHT * scatter_cornette_shanks(mu, CLOUD_BACK_ANISOTROPY * scale);
}

// Light scattered at a sample per unit extinction: sun (light march toward it,
// phase, multiple scattering) plus ambient, times the albedo.
fn cloud_source(c: vec3<f32>, y: f32, dir: vec3<f32>, has_sun: bool, sun: vec3<f32>, irradiance: vec3<f32>) -> vec3<f32> {
    let h = y / clouds.layer.x;
    var radiance = engine_view.ambient.rgb * (clouds.lighting.y * (CLOUD_AMBIENT_BASE + (1.0 - CLOUD_AMBIENT_BASE) * h));
    let visibility = smoothstep(0.0, CLOUD_SUN_HORIZON, sun.y);
    if has_sun && visibility > 0.0 {
        let steps = u32(clouds.steps.z);
        let dl = clouds.steps.w / f32(steps);
        var tau = 0.0;
        for (var j = 0u; j < steps; j++) {
            let d = (f32(j) + 0.5) * dl;
            tau += cloud_extinction(c + sun * (d * clouds.density.w), y + sun.y * d) * dl;
        }
        let mu = dot(dir, sun);
        var octaves = 1u;
        if clouds.origin.w > 0.5 {
            octaves = CLOUD_MS_OCTAVES;
        }
        var amplitude = 1.0;
        var attenuation = 1.0;
        var scale = 1.0;
        var sun_term = 0.0;
        for (var k = 0u; k < octaves; k++) {
            sun_term += amplitude * cloud_phase(mu, scale) * exp(-tau * attenuation);
            amplitude *= CLOUD_MS_FACTOR;
            attenuation *= CLOUD_MS_FACTOR;
            scale *= CLOUD_MS_FACTOR;
        }
        radiance += irradiance * (visibility * sun_term * clouds.lighting.x);
    }
    return min(clouds.albedo.rgb * radiance, vec3<f32>(CLOUD_MAX_RADIANCE));
}

// x: entry, y: exit of the layer along a ray with vertical cosine dy, bounded
// by `limit` (y <= x: none).
fn cloud_interval(dy: f32, limit: f32) -> vec2<f32> {
    let h0 = clouds.layer.y;
    let thickness = clouds.layer.x;
    if abs(dy) < CLOUD_FLAT_DIRECTION {
        if h0 >= 0.0 && h0 <= thickness {
            return vec2<f32>(0.0, limit);
        }
        return vec2<f32>(0.0, 0.0);
    }
    let a = -h0 / dy;
    let b = (thickness - h0) / dy;
    return vec2<f32>(max(min(a, b), 0.0), min(max(a, b), limit));
}

@fragment fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(frag.xy);
    let color = textureLoad(scene_color, pixel, 0);
    let depth = textureLoad(scene_depth, pixel, 0);
    var limit = clouds.layer.z;
    if depth < 1.0 {
        limit = min(limit, scatter_view_distance(frag.xy, depth));
    }
    let dir = scatter_view_direction(frag.xy);
    let range = cloud_interval(dir.y, limit);
    if range.y <= range.x {
        return color;
    }
    var sun = vec3<f32>(0.0, 1.0, 0.0);
    var irradiance = vec3<f32>(0.0);
    let has_sun = scatter_sun(&sun, &irradiance);
    let steps = u32(clouds.steps.y);
    let dt = (range.y - range.x) / f32(steps);
    var transmittance = 1.0;
    var inscatter = vec3<f32>(0.0);
    for (var i = 0u; i < steps; i++) {
        let t = range.x + (f32(i) + 0.5) * dt;
        let c = clouds.origin.xyz + dir * (t * clouds.density.w);
        let y = clouds.layer.y + dir.y * t;
        let sigma = cloud_extinction(c, y);
        if sigma <= 0.0 {
            continue;
        }
        let light = cloud_source(c, y, dir, has_sun, sun, irradiance);
        let step_transmittance = exp(-sigma * dt);
        inscatter += transmittance * light * (1.0 - step_transmittance);
        transmittance *= step_transmittance;
        if transmittance < CLOUD_CUTOFF {
            break;
        }
    }
    var fade = 1.0;
    if clouds.layer.w > 0.0 {
        fade = exp(-range.x / clouds.layer.w);
    }
    let visible = 1.0 - fade * (1.0 - transmittance);
    return vec4<f32>(color.rgb * visible + inscatter * fade, color.a);
}
