// Helpers shared by the scattering passes (atmosphere, fog): the Cornette-
// Shanks phase function, the world-space ray through a pixel, the view
// distance to a depth sample and the sun light. CPU twins:
// atmosphere/model.rs (`mie_phase`) and the pass tests.
//
// Needs `#import engine::view`.
const SCATTER_PI: f32 = 3.14159265358979;

// Cornette-Shanks with asymmetry g; integrates to one over the sphere.
fn scatter_cornette_shanks(mu: f32, g: f32) -> f32 {
    let g2 = g * g;
    let denom = pow(max(1.0 + g2 - 2.0 * g * mu, 1.0e-4), 1.5);
    return 3.0 / (8.0 * SCATTER_PI) * (1.0 - g2) * (1.0 + mu * mu) / ((2.0 + g2) * denom);
}

// World-space direction of the ray through render-target pixel `frag`.
// Perspective rays pass through the near-plane point, which stays accurate
// for any far plane; orthographic views look along view -Z.
fn scatter_view_direction(frag: vec2<f32>) -> vec3<f32> {
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

// Distance from the camera to the geometry at `depth` under pixel `frag`.
// Orthographic views measure along the view axis; perspective views use the
// euclidean distance to the camera.
fn scatter_view_distance(frag: vec2<f32>, depth: f32) -> f32 {
    let ndc = vec2<f32>(
        frag.x * engine_view.render_size.z * 2.0 - 1.0,
        1.0 - frag.y * engine_view.render_size.w * 2.0,
    );
    let clip = engine_view.view_from_clip * vec4<f32>(ndc, depth, 1.0);
    let position = clip.xyz / clip.w;
    if engine_view.clip_from_view[3][3] > 0.5 {
        return -position.z;
    }
    return length(position);
}

// The first directional light is the sun: direction toward it and its
// irradiance (lux times view exposure). Returns false without one.
fn scatter_sun(sun: ptr<function, vec3<f32>>, irradiance: ptr<function, vec3<f32>>) -> bool {
    if engine_lights.counts.y == 0u {
        return false;
    }
    let light = engine_light(0u);
    *sun = normalize(light.direction);
    *irradiance = light.color * light.intensity;
    return true;
}
