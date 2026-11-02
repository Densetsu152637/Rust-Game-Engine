// Single-scattering atmosphere shared by the sky and aerial-perspective
// passes (docs/rendering-atmosphere.md). CPU twin: atmosphere/model.rs.
//
// Needs `#import engine::view`, the SKY_STEPS / AERIAL_STEPS / SUN_STEPS
// constants and a `var<uniform> atmosphere: Atmosphere` declared by the pass.
//
// Heights. A point P with |P|^2 - R^2 = u has altitude u / (|P| + R), which
// avoids the |P| - R cancellation at planetary radii. A ray from altitude a
// with direction d has u(t) = a (2R + a) + 2 (R + a) d.y t + t^2.
struct Atmosphere {
    // rgb Rayleigh scattering at the ground (1/m), w scale height (m).
    rayleigh: vec4<f32>,
    // x Mie scattering, y Mie absorption (1/m), z scale height (m), w anisotropy g.
    mie: vec4<f32>,
    // rgb ozone absorption (1/m).
    ozone: vec4<f32>,
    // x ozone layer centre, y ozone half width (m), z aerial distance scale,
    // w sun angular radius (radians).
    layer: vec4<f32>,
    // rgb ground albedo, w intensity multiplier.
    ground: vec4<f32>,
    // x planet radius, y atmosphere thickness, z camera altitude (m).
    geometry: vec4<f32>,
}

const ATM_PI: f32 = 3.14159265358979;
const ATM_MAX_RADIANCE: f32 = 60000.0;
const ATM_BLOCKED: f32 = 1.0e6;
// ATM_DISK_NORMALISATION (solid-angle fraction covered by the soft limb) is
// injected from SUN_DISK_NORMALISATION in model.rs by pass_source.

struct AtmMarch {
    inscatter: vec3<f32>,
    transmittance: vec3<f32>,
}

fn atm_height(u: f32) -> f32 {
    let planet = atmosphere.geometry.x;
    return u / (sqrt(max(planet * planet + u, 0.0)) + planet);
}

fn atm_rayleigh_density(h: f32) -> f32 {
    return exp(-h / atmosphere.rayleigh.w);
}

fn atm_mie_density(h: f32) -> f32 {
    return exp(-h / atmosphere.mie.z);
}

fn atm_ozone_density(h: f32) -> f32 {
    return max(0.0, 1.0 - abs(h - atmosphere.layer.x) / atmosphere.layer.y);
}

fn atm_extinction(h: f32) -> vec3<f32> {
    let hc = max(h, 0.0);
    return atmosphere.rayleigh.rgb * atm_rayleigh_density(hc)
        + vec3<f32>(atmosphere.mie.x + atmosphere.mie.y) * atm_mie_density(hc)
        + atmosphere.ozone.rgb * atm_ozone_density(hc);
}

fn atm_rayleigh_phase(mu: f32) -> f32 {
    return 3.0 / (16.0 * ATM_PI) * (1.0 + mu * mu);
}

// Cornette-Shanks; integrates to one over the sphere.
fn atm_mie_phase(mu: f32) -> f32 {
    let g = atmosphere.mie.w;
    let g2 = g * g;
    let denom = pow(max(1.0 + g2 - 2.0 * g * mu, 1.0e-4), 1.5);
    return 3.0 / (8.0 * ATM_PI) * (1.0 - g2) * (1.0 + mu * mu) / ((2.0 + g2) * denom);
}

// Optical depth from altitude h along the sun direction to the top of the
// atmosphere; ps = dot(P, sun). Rays that reach the planet are blocked.
fn atm_sun_depth(h: f32, ps: f32) -> vec3<f32> {
    let planet = atmosphere.geometry.x;
    let thick = atmosphere.geometry.y;
    let hc = clamp(h, 0.0, thick);
    let u = hc * (2.0 * planet + hc);
    if ps < 0.0 && ps * ps > u {
        return vec3<f32>(ATM_BLOCKED);
    }
    let c = (thick - hc) * (2.0 * planet + thick + hc);
    var top: f32;
    if ps > 0.0 {
        top = c / (ps + sqrt(ps * ps + c));
    } else {
        top = -ps + sqrt(ps * ps + c);
    }
    var depth = vec3<f32>(0.0);
    var previous = 0.0;
    for (var i = 1u; i <= SUN_STEPS; i++) {
        let f = f32(i) / f32(SUN_STEPS);
        let edge = top * f * f;
        let dt = edge - previous;
        let mid = previous + 0.5 * dt;
        previous = edge;
        depth += atm_extinction(atm_height(u + 2.0 * ps * mid + mid * mid)) * dt;
    }
    return depth;
}

// x: start, y: end of the atmosphere along a ray with vertical cosine mu
// (y <= x: none); z: distance to the planet, or -1.
fn atm_interval(mu: f32) -> vec3<f32> {
    let planet = atmosphere.geometry.x;
    let thick = atmosphere.geometry.y;
    let a = atmosphere.geometry.z;
    let b = (planet + a) * mu;
    let disc = b * b + (thick - a) * (2.0 * planet + thick + a);
    var result = vec3<f32>(0.0, 0.0, -1.0);
    if disc < 0.0 {
        return result;
    }
    let root = sqrt(disc);
    result.x = max(-b - root, 0.0);
    result.y = -b + root;
    let disc_ground = b * b - a * (2.0 * planet + a);
    if b < 0.0 && disc_ground > 0.0 {
        result.z = -b - sqrt(disc_ground);
    }
    return result;
}

// Single-scattered radiance and transmittance along dir between t0 and t1.
fn atm_march(dir: vec3<f32>, sun: vec3<f32>, irradiance: vec3<f32>, t0: f32, t1: f32, steps: u32) -> AtmMarch {
    let planet = atmosphere.geometry.x;
    let altitude = atmosphere.geometry.z;
    let r0 = planet + altitude;
    let u0 = altitude * (2.0 * planet + altitude);
    let ds = dot(dir, sun);
    let phase_r = atm_rayleigh_phase(ds);
    let phase_m = atm_mie_phase(ds);
    var result: AtmMarch;
    result.inscatter = vec3<f32>(0.0);
    result.transmittance = vec3<f32>(1.0);
    var previous = t0;
    for (var i = 1u; i <= steps; i++) {
        let f = f32(i) / f32(steps);
        let edge = t0 + (t1 - t0) * f * f;
        let dt = edge - previous;
        let t = previous + 0.5 * dt;
        previous = edge;
        let h = max(atm_height(u0 + 2.0 * r0 * dir.y * t + t * t), 0.0);
        let ps = r0 * sun.y + t * ds;
        let sun_t = exp(-atm_sun_depth(h, ps));
        let scatter = (atmosphere.rayleigh.rgb * (atm_rayleigh_density(h) * phase_r)
            + vec3<f32>(atmosphere.mie.x * atm_mie_density(h) * phase_m)) * irradiance * sun_t;
        let extinction = atm_extinction(h);
        let step_t = exp(-extinction * dt);
        let safe = max(extinction, vec3<f32>(1.0e-12));
        // Source constant over the step, exponential extinction inside it.
        let integral = select(scatter * dt, scatter * (vec3<f32>(1.0) - step_t) / safe, extinction > vec3<f32>(1.0e-12));
        result.inscatter += result.transmittance * integral;
        result.transmittance *= step_t;
    }
    return result;
}

// Radiance seen along dir where no geometry covers the pixel: scattered
// light, the Lambertian planet below the horizon and the sun disk.
fn atm_sky(dir: vec3<f32>, sun: vec3<f32>, irradiance: vec3<f32>) -> vec3<f32> {
    let planet = atmosphere.geometry.x;
    let altitude = atmosphere.geometry.z;
    let range = atm_interval(dir.y);
    let hit = range.z > 0.0;
    var radiance = vec3<f32>(0.0);
    var transmittance = vec3<f32>(1.0);
    if range.y > range.x {
        let stop = select(range.y, min(range.y, range.z), hit);
        let m = atm_march(dir, sun, irradiance, range.x, stop, SKY_STEPS);
        radiance = m.inscatter;
        transmittance = m.transmittance;
        if hit {
            let ps = (planet + altitude) * sun.y + range.z * dot(dir, sun);
            let cosine = max(ps / planet, 0.0);
            let lit = exp(-atm_sun_depth(0.0, ps));
            radiance += transmittance * atmosphere.ground.rgb / ATM_PI * irradiance * lit * cosine;
        }
    }
    let radius = atmosphere.layer.w;
    if !hit && radius > 0.0 {
        let angle = atan2(length(cross(dir, sun)), dot(dir, sun));
        let coverage = 1.0 - smoothstep(0.8 * radius, radius, angle);
        if coverage > 0.0 {
            let half_sine = sin(0.5 * radius);
            let solid = 4.0 * ATM_PI * half_sine * half_sine;
            radiance += transmittance * irradiance * (coverage / (solid * ATM_DISK_NORMALISATION));
        }
    }
    return clamp(radiance, vec3<f32>(0.0), vec3<f32>(ATM_MAX_RADIANCE));
}

// In-scattered radiance and transmittance toward geometry `distance` metres
// away (already multiplied by the aerial distance scale).
fn atm_aerial(dir: vec3<f32>, sun: vec3<f32>, irradiance: vec3<f32>, distance: f32) -> AtmMarch {
    var none: AtmMarch;
    none.inscatter = vec3<f32>(0.0);
    none.transmittance = vec3<f32>(1.0);
    let range = atm_interval(dir.y);
    if range.y <= range.x {
        return none;
    }
    let stop = min(select(range.y, min(range.y, range.z), range.z > 0.0), distance);
    if stop <= range.x {
        return none;
    }
    return atm_march(dir, sun, irradiance, range.x, stop, AERIAL_STEPS);
}

// World-space direction of the ray through render-target pixel `frag`.
// Perspective rays pass through the near-plane point, which stays accurate
// for any far plane; orthographic views look along view -Z.
fn atm_view_direction(frag: vec2<f32>) -> vec3<f32> {
    let ndc = vec2<f32>(
        frag.x * engine_view.render_size.z * 2.0 - 1.0,
        1.0 - frag.y * engine_view.render_size.w * 2.0,
    );
    var view_dir = vec3<f32>(0.0, 0.0, -1.0);
    if engine_view.clip_from_view[3][3] < 0.5 {
        let p = engine_view.view_from_clip * vec4<f32>(ndc, 0.0, 1.0);
        view_dir = normalize(p.xyz / p.w);
    }
    return normalize((engine_view.world_from_view * vec4<f32>(view_dir, 0.0)).xyz);
}

// The first directional light is the sun. Returns false without one.
fn atm_sun_irradiance(sun: ptr<function, vec3<f32>>, irradiance: ptr<function, vec3<f32>>) -> bool {
    if engine_lights.counts.y == 0u {
        return false;
    }
    let light = engine_light(0u);
    *sun = normalize(light.direction);
    *irradiance = light.color * (light.intensity * atmosphere.ground.w);
    return true;
}
