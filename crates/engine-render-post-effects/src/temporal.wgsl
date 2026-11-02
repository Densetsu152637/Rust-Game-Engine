// Native temporal resolve (anti-aliasing at 1:1, upscaling below it).
//
// One fragment per output pixel:
//   1. Reconstruct the current colour from the 3x3 jittered render-size texels
//      around the pixel with a Gaussian weight on the distance between the
//      pixel centre and each texel's *unjittered* position. Texel `t` holds the
//      scene at unjittered position `t + 0.5 - jitter` (render pixels).
//   2. Take depth and motion from the closest texel of that 3x3 (so a
//      silhouette pixel follows the foreground), reproject into the history
//      (current-to-previous motion in render pixels; previous = current + m).
//   3. Reject the history outside the image or when its stored view distance
//      differs from the distance this surface had under pure camera motion by
//      more than a relative tolerance plus three pixels of the surface's slope
//      (disocclusion); otherwise sample it with a Catmull-Rom filter.
//   4. Clip the history towards the box of the 3x3 neighbourhood in YCoCg
//      (variance box intersected with min/max).
//   5. Blend with a weight of 1 / n (n = accumulated frames, at most the
//      configured history length, shortened by screen-space speed).
// Colour is handled as c / (1 + luma(c)), a reversible compression, so that
// bright HDR samples cannot dominate the neighbourhood or the blend.
// Outputs: resolved HDR colour, the compressed history colour and
// (view distance, accumulated frames) for the next frame's checks.

struct Params {
    inverse_projection: mat4x4<f32>,
    previous_view_from_view: mat4x4<f32>,
    // render size, output size.
    sizes: vec4<f32>,
    // jitter (render pixels), history valid (0 or 1), history frames.
    jitter: vec4<f32>,
    // clip gamma, depth tolerance (relative), speed that halves history (px),
    // Gaussian reconstruction sharpness.
    tuning: vec4<f32>,
}

@group(0) @binding(0) var scene: texture_2d<f32>;
@group(0) @binding(1) var scene_depth: texture_depth_2d;
@group(0) @binding(2) var motion: texture_2d<f32>;
@group(0) @binding(3) var history: texture_2d<f32>;
@group(0) @binding(4) var history_aux: texture_2d<f32>;
@group(0) @binding(5) var history_sampler: sampler;
@group(0) @binding(6) var<uniform> p: Params;

@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// Non-negative and bounded: keeps one bad texel from poisoning the history.
// NaN is zeroed where the implementation preserves NaNs (WGSL does not require
// it to), so this is best effort for NaN and exact for infinities and negatives.
fn sanitize(c: vec4<f32>) -> vec4<f32> {
    let bounded = clamp(c, vec4<f32>(0.0), vec4<f32>(60000.0));
    return select(bounded, vec4<f32>(0.0), c != c);
}

// View distance of a depth sample; for a symmetric or orthographic projection
// it depends on the depth value alone.
fn depth_distance(depth: f32) -> f32 {
    let h = p.inverse_projection * vec4<f32>(0.0, 0.0, depth, 1.0);
    return abs(h.z / h.w);
}

// Depth slope of the surface at `texel` in view distance per render pixel:
// the smaller one-sided difference per axis, so a silhouette (one side is a
// different surface) does not widen the tolerance across the edge.
fn distance_slope(texel: vec2<i32>, centre: f32, last: vec2<i32>) -> f32 {
    let left = depth_distance(textureLoad(scene_depth, clamp(texel + vec2<i32>(-1, 0), vec2<i32>(0), last), 0));
    let right = depth_distance(textureLoad(scene_depth, clamp(texel + vec2<i32>(1, 0), vec2<i32>(0), last), 0));
    let up = depth_distance(textureLoad(scene_depth, clamp(texel + vec2<i32>(0, -1), vec2<i32>(0), last), 0));
    let down = depth_distance(textureLoad(scene_depth, clamp(texel + vec2<i32>(0, 1), vec2<i32>(0), last), 0));
    return min(abs(right - centre), abs(left - centre)) + min(abs(down - centre), abs(up - centre));
}

fn compress(c: vec4<f32>) -> vec4<f32> {
    let s = sanitize(c);
    return vec4<f32>(s.rgb / (1.0 + luma(s.rgb)), s.a);
}

fn decompress(c: vec4<f32>) -> vec4<f32> {
    let l = min(luma(c.rgb), 0.99998);
    return vec4<f32>(c.rgb / (1.0 - l), c.a);
}

fn to_ycocg(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        0.25 * c.r + 0.5 * c.g + 0.25 * c.b,
        0.5 * c.r - 0.5 * c.b,
        -0.25 * c.r + 0.5 * c.g - 0.25 * c.b,
    );
}

fn from_ycocg(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(c.x + c.y - c.z, c.x + c.z, c.x - c.y - c.z);
}

fn clip_to_box(box_min: vec3<f32>, box_max: vec3<f32>, h: vec3<f32>) -> vec3<f32> {
    let centre = 0.5 * (box_max + box_min);
    let extent = 0.5 * (box_max - box_min) + vec3<f32>(1e-5);
    let offset = h - centre;
    let unit = abs(offset / extent);
    let m = max(unit.x, max(unit.y, unit.z));
    if m > 1.0 {
        return centre + offset / m;
    }
    return h;
}

// Catmull-Rom history filter in five bilinear taps. `position` is in output
// pixels with the pixel centre at +0.5.
fn sample_history(position: vec2<f32>) -> vec4<f32> {
    let size = p.sizes.zw;
    let rounded = round(position - vec2<f32>(0.5));
    if all(abs(position - vec2<f32>(0.5) - rounded) < vec2<f32>(0.002)) {
        let last = vec2<i32>(size) - vec2<i32>(1);
        return textureLoad(history, clamp(vec2<i32>(rounded), vec2<i32>(0), last), 0);
    }
    let centre = floor(position - vec2<f32>(0.5)) + vec2<f32>(0.5);
    let f = position - centre;
    let w0 = f * (vec2<f32>(-0.5) + f * (vec2<f32>(1.0) - 0.5 * f));
    let w1 = vec2<f32>(1.0) + f * f * (vec2<f32>(-2.5) + 1.5 * f);
    let w2 = f * (vec2<f32>(0.5) + f * (vec2<f32>(2.0) - 1.5 * f));
    let w3 = f * f * (vec2<f32>(-0.5) + 0.5 * f);
    let w12 = w1 + w2;
    let t0 = (centre - vec2<f32>(1.0)) / size;
    let t3 = (centre + vec2<f32>(2.0)) / size;
    let t12 = (centre + w2 / w12) / size;
    var sum = textureSampleLevel(history, history_sampler, vec2<f32>(t12.x, t0.y), 0.0) * (w12.x * w0.y);
    sum += textureSampleLevel(history, history_sampler, vec2<f32>(t0.x, t12.y), 0.0) * (w0.x * w12.y);
    sum += textureSampleLevel(history, history_sampler, vec2<f32>(t12.x, t12.y), 0.0) * (w12.x * w12.y);
    sum += textureSampleLevel(history, history_sampler, vec2<f32>(t3.x, t12.y), 0.0) * (w3.x * w12.y);
    sum += textureSampleLevel(history, history_sampler, vec2<f32>(t12.x, t3.y), 0.0) * (w12.x * w3.y);
    let total = w12.x * w0.y + w0.x * w12.y + w12.x * w12.y + w3.x * w12.y + w12.x * w3.y;
    return max(sum / total, vec4<f32>(0.0));
}

struct Resolved {
    @location(0) color: vec4<f32>,
    @location(1) history: vec4<f32>,
    @location(2) aux: vec4<f32>,
}

@fragment fn fs_main(@builtin(position) position: vec4<f32>) -> Resolved {
    let render = p.sizes.xy;
    let output = p.sizes.zw;
    let ratio = render / output;
    let jitter = p.jitter.xy;
    let pixel = vec2<i32>(position.xy);
    let centre = vec2<f32>(pixel) + vec2<f32>(0.5);
    let render_position = centre * ratio;
    let render_last = vec2<i32>(render) - vec2<i32>(1);
    let base = clamp(vec2<i32>(floor(render_position)), vec2<i32>(0), render_last);

    var sum = vec4<f32>(0.0);
    var weight_sum = 0.0;
    var box_min = vec3<f32>(1e9);
    var box_max = vec3<f32>(-1e9);
    var moment1 = vec3<f32>(0.0);
    var moment2 = vec3<f32>(0.0);
    var closest = vec2<i32>(base);
    var closest_depth = 2.0;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            let texel = clamp(base + vec2<i32>(dx, dy), vec2<i32>(0), render_last);
            let color = compress(textureLoad(scene, texel, 0));
            let offset = vec2<f32>(texel) + vec2<f32>(0.5) - jitter - render_position;
            let weight = exp(-p.tuning.w * dot(offset, offset));
            sum += color * weight;
            weight_sum += weight;
            let ycocg = to_ycocg(color.rgb);
            box_min = min(box_min, ycocg);
            box_max = max(box_max, ycocg);
            moment1 += ycocg;
            moment2 += ycocg * ycocg;
            let depth = textureLoad(scene_depth, texel, 0);
            if depth < closest_depth {
                closest_depth = depth;
                closest = texel;
            }
        }
    }
    let current = sum / weight_sum;

    // View distance of the closest surface, from the unjittered texel position.
    let unjittered = (vec2<f32>(closest) + vec2<f32>(0.5) - jitter) / render;
    let clip = vec4<f32>(unjittered.x * 2.0 - 1.0, 1.0 - unjittered.y * 2.0, closest_depth, 1.0);
    let view_h = p.inverse_projection * clip;
    let view_position = view_h.xyz / view_h.w;
    let distance = abs(view_position.z);

    var count = 0.0;
    var blended = current;
    let speed_vector = textureLoad(motion, closest, 0).xy / ratio;
    if p.jitter.z > 0.5 {
        let history_position = centre + speed_vector;
        var valid = all(history_position > vec2<f32>(0.0)) && all(history_position < output);
        if valid {
            let aux_pixel = clamp(vec2<i32>(floor(history_position)), vec2<i32>(0), vec2<i32>(output) - vec2<i32>(1));
            let aux = textureLoad(history_aux, aux_pixel, 0);
            let previous_view = p.previous_view_from_view * vec4<f32>(view_position, 1.0);
            let expected = abs(previous_view.z);
            // Relative tolerance plus slack for the local slope: the history is
            // read from the nearest texel and both distances come from the
            // closest texel of a 3x3, so a surface receding at a grazing angle
            // legitimately differs by up to a few pixels of its own slope.
            let slope = distance_slope(closest, distance, render_last);
            let tolerance = p.tuning.y * max(expected, aux.x) + 3.0 * slope + 1e-4;
            if aux.y < 0.5 || abs(aux.x - expected) > tolerance {
                valid = false;
            }
            count = aux.y;
        }
        if valid {
            let mean = moment1 / 9.0;
            let sigma = sqrt(max(moment2 / 9.0 - mean * mean, vec3<f32>(0.0)));
            let gamma = p.tuning.x;
            let lo = max(box_min, mean - gamma * sigma);
            let hi = min(box_max, mean + gamma * sigma);
            let sampled = sample_history(history_position);
            let clipped = clip_to_box(lo, hi, to_ycocg(sampled.rgb));
            let history_color = vec4<f32>(max(from_ycocg(clipped), vec3<f32>(0.0)), sampled.a);
            // Long histories blur moving content: shorten them with speed.
            let speed = length(speed_vector);
            let cap = mix(p.jitter.w, min(p.jitter.w, 3.0), clamp(speed / p.tuning.z, 0.0, 1.0));
            count = min(count, cap);
            let alpha = 1.0 / (count + 1.0);
            blended = mix(history_color, current, alpha);
        } else {
            count = 0.0;
        }
    }
    let new_count = min(count + 1.0, p.jitter.w);
    var out: Resolved;
    out.color = decompress(blended);
    out.history = blended;
    out.aux = vec4<f32>(distance, new_count, 0.0, 0.0);
    return out;
}
