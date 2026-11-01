// engine.ssr: screen-space reflections over the opaque scene colour.
//
// Every pixel of a built-in lit surface at or below the roughness cutoff
// traces its mirror direction through the current frame's depth. A hit blends
// the opaque scene colour found there over the pixel's image-based specular:
//
//   result = scene + weight * (hit_radiance * split_sum - fallback_specular)
//
// `fallback_specular` is the specular term the opaque pass already added
// (probe / environment specular, zero without indirect lighting), written by
// the surface-material pass, so `weight = 0` is exactly the opaque image and
// `weight = 1` replaces that specular with the traced one. Pixels without
// surface-material coverage (custom shaders, unlit materials, sky) and pixels
// above the roughness cutoff are copied unchanged.
#import engine::fullscreen

struct SsrParams {
    // Inverse and forward of the (jittered) projection that rasterized the depth.
    view_from_clip: mat4x4<f32>,
    clip_from_view: mat4x4<f32>,
    view_from_world: mat4x4<f32>,
    // Width, height, 1 / width, 1 / height of the render-size targets.
    size: vec4<f32>,
    // Step count, longest ray (view units), thickness (view units), roughness cutoff.
    trace: vec4<f32>,
    // Hit-position edge fade, intensity, 1 for an orthographic projection, unused.
    shade: vec4<f32>,
}

@group(0) @binding(0) var scene_color: texture_2d<f32>;
@group(0) @binding(1) var scene_depth: texture_depth_2d;
// World-space shaded normal (xyz) and perceptual roughness (w).
@group(0) @binding(2) var surface_normal: texture_2d<f32>;
// Dielectric / metal F0 (rgb) and coverage (a, 1 where a built-in lit surface drew).
@group(0) @binding(3) var surface_f0: texture_2d<f32>;
// The specular term already present in the opaque scene colour (rgb).
@group(0) @binding(4) var surface_fallback: texture_2d<f32>;
@group(0) @binding(5) var<uniform> params: SsrParams;

@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    return engine_fullscreen_position(index);
}

fn pixel_to_ndc(pixel: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(pixel.x * params.size.z * 2.0 - 1.0, 1.0 - pixel.y * params.size.w * 2.0);
}

fn ndc_to_pixel(ndc: vec2<f32>) -> vec2<f32> {
    return vec2<f32>((ndc.x * 0.5 + 0.5) * params.size.x, (0.5 - ndc.y * 0.5) * params.size.y);
}

// View-space position (x right, y up, -z forward) of the depth sample at a
// pixel centre.
fn view_position(centre: vec2<f32>, depth: f32) -> vec3<f32> {
    let h = params.view_from_clip * vec4<f32>(pixel_to_ndc(centre), depth, 1.0);
    return h.xyz / h.w;
}

// Distance along the view axis of the opaque surface at an integer pixel;
// negative where the pixel holds no geometry (cleared depth).
fn surface_distance(pixel: vec2<i32>) -> f32 {
    let depth = textureLoad(scene_depth, pixel, 0);
    if depth >= 1.0 { return -1.0; }
    return -view_position(vec2<f32>(pixel) + vec2<f32>(0.5), depth).z;
}

struct Hit {
    found: bool,
    // Integer pixel of the hit surface.
    pixel: vec2<f32>,
    // View-space distance from the ray start to the hit.
    travelled: f32,
    // Screen pixels covered by the ray before the hit.
    pixels: f32,
}

fn miss() -> Hit {
    var hit: Hit;
    hit.found = false;
    hit.pixel = vec2<f32>(0.0);
    hit.travelled = 0.0;
    hit.pixels = 0.0;
    return hit;
}

// Screen-space march of `start + dir * t` for t in 0..ray_length, with perspective
// correct depth (1 / w and position / w interpolate linearly on screen).
fn march(start: vec3<f32>, dir: vec3<f32>, ray_length: f32) -> Hit {
    let end = start + dir * ray_length;
    let h0 = params.clip_from_view * vec4<f32>(start, 1.0);
    let h1 = params.clip_from_view * vec4<f32>(end, 1.0);
    let k0 = 1.0 / h0.w;
    let k1 = 1.0 / h1.w;
    let p0 = ndc_to_pixel(h0.xy * k0);
    let p1 = ndc_to_pixel(h1.xy * k1);
    let q0 = start * k0;
    let q1 = end * k1;
    let delta = p1 - p0;
    let covered = max(abs(delta.x), abs(delta.y));
    if covered < 1.0 { return miss(); }
    let steps = min(params.trace.x, ceil(covered));
    let count = i32(steps);
    let thickness = params.trace.z;
    var previous_t = 0.0;
    var previous_distance = -start.z;
    for (var i = 1; i <= count; i++) {
        let t = f32(i) / steps;
        let position = p0 + delta * t;
        if position.x < 0.0 || position.y < 0.0 || position.x >= params.size.x || position.y >= params.size.y {
            break;
        }
        let k = mix(k0, k1, t);
        let ray_distance = -(mix(q0.z, q1.z, t) / k);
        let scene_distance = surface_distance(vec2<i32>(position));
        let near = min(previous_distance, ray_distance);
        let far = max(previous_distance, ray_distance);
        if scene_distance > 0.0 && scene_distance <= far && scene_distance >= near - thickness {
            // Bisect the step for the first crossing of the surface.
            var low = previous_t;
            var high = t;
            for (var j = 0; j < 5; j++) {
                let middle = 0.5 * (low + high);
                let at = p0 + delta * middle;
                let km = mix(k0, k1, middle);
                let ray_m = -(mix(q0.z, q1.z, middle) / km);
                let surface_m = surface_distance(vec2<i32>(at));
                if surface_m > 0.0 && surface_m <= ray_m {
                    high = middle;
                } else {
                    low = middle;
                }
            }
            let at = p0 + delta * high;
            let kh = mix(k0, k1, high);
            let ray_h = -(mix(q0.z, q1.z, high) / kh);
            let surface_h = surface_distance(vec2<i32>(at));
            if surface_h > 0.0 && abs(ray_h - surface_h) <= thickness {
                var hit: Hit;
                hit.found = true;
                hit.pixel = floor(at);
                hit.pixels = covered * high;
                let point = mix(q0, q1, high) / kh;
                hit.travelled = distance(point, start);
                return hit;
            }
        }
        previous_t = t;
        previous_distance = ray_distance;
    }
    return miss();
}

fn scene_at(position: vec2<f32>) -> vec3<f32> {
    let limit = vec2<i32>(params.size.xy) - vec2<i32>(1);
    let pixel = clamp(vec2<i32>(floor(position)), vec2<i32>(0), limit);
    return textureLoad(scene_color, pixel, 0).rgb;
}

// Mirror-direction radiance at a hit pixel. Rougher surfaces average a small
// cross of taps whose spread grows with the roughness and the screen distance
// the ray travelled (a cheap cone approximation; there is no colour pyramid).
fn hit_radiance(pixel: vec2<f32>, roughness: f32, pixels: f32) -> vec3<f32> {
    let centre = pixel + vec2<f32>(0.5);
    let spread = min(roughness * pixels * 0.5, 12.0);
    if spread < 0.5 { return scene_at(centre); }
    var sum = scene_at(centre) * 0.4;
    sum += scene_at(centre + vec2<f32>(spread, spread)) * 0.15;
    sum += scene_at(centre + vec2<f32>(-spread, spread)) * 0.15;
    sum += scene_at(centre + vec2<f32>(spread, -spread)) * 0.15;
    sum += scene_at(centre + vec2<f32>(-spread, -spread)) * 0.15;
    return sum;
}

// Analytic split-sum environment BRDF (Karis): scale and bias of F0.
fn environment_brdf(f0: vec3<f32>, roughness: f32, n_dot_v: f32) -> vec3<f32> {
    let c0 = vec4<f32>(-1.0, -0.0275, -0.572, 0.022);
    let c1 = vec4<f32>(1.0, 0.0425, 1.04, -0.04);
    let r = roughness * c0 + c1;
    let a004 = min(r.x * r.x, exp2(-9.28 * n_dot_v)) * r.x + r.y;
    let ab = vec2<f32>(-1.04, 1.04) * a004 + r.zw;
    return f0 * ab.x + vec3<f32>(ab.y);
}

@fragment fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(position.xy);
    let scene = textureLoad(scene_color, pixel, 0);
    let f0_coverage = textureLoad(surface_f0, pixel, 0);
    if f0_coverage.a < 0.5 { return scene; }
    let normal_roughness = textureLoad(surface_normal, pixel, 0);
    let roughness = normal_roughness.a;
    let cutoff = params.trace.w;
    if roughness >= cutoff { return scene; }
    let depth = textureLoad(scene_depth, pixel, 0);
    if depth >= 1.0 { return scene; }

    let origin = view_position(vec2<f32>(pixel) + vec2<f32>(0.5), depth);
    let orthographic = params.shade.z > 0.5;
    let view_dir = select(normalize(-origin), vec3<f32>(0.0, 0.0, 1.0), orthographic);
    let normal = normalize((params.view_from_world * vec4<f32>(normal_roughness.xyz, 0.0)).xyz);
    let n_dot_v = clamp(dot(normal, view_dir), 0.0, 1.0);
    let dir = reflect(-view_dir, normal);

    // Lift the start off the surface, then clip the ray to the camera's front
    // half space (perspective w = -z) and the configured length.
    let start = origin + normal * (0.01 + 0.0005 * -origin.z);
    var ray_length = params.trace.y;
    if !orthographic && dir.z > 0.0 {
        ray_length = min(ray_length, max((-start.z - 0.01) / dir.z, 0.0));
    }
    if ray_length <= 0.0 { return scene; }
    let hit = march(start, dir, ray_length);
    if !hit.found { return scene; }

    var weight = 1.0 - smoothstep(0.5 * cutoff, cutoff, roughness);
    let uv = (hit.pixel + vec2<f32>(0.5)) * params.size.zw;
    let border = min(min(uv.x, 1.0 - uv.x), min(uv.y, 1.0 - uv.y));
    let fade = params.shade.x;
    if fade > 0.0 { weight *= smoothstep(0.0, fade, border); }
    weight *= 1.0 - smoothstep(0.75, 1.0, hit.travelled / params.trace.y);
    // Rays heading back towards the camera cannot be validated on screen.
    weight *= 1.0 - smoothstep(0.0, 0.5, dir.z);
    if weight <= 0.0 { return scene; }

    let radiance = hit_radiance(hit.pixel, roughness, hit.pixels) * params.shade.y;
    let reflected = radiance * environment_brdf(f0_coverage.rgb, roughness, n_dot_v);
    let fallback = textureLoad(surface_fallback, pixel, 0).rgb;
    let result = max(scene.rgb + weight * (reflected - fallback), vec3<f32>(0.0));
    return vec4<f32>(result, scene.a);
}
