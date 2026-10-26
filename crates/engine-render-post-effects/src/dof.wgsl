@group(0) @binding(0) var hdr: texture_2d<f32>;
@group(0) @binding(1) var depth: texture_depth_2d;
struct Params {
    inverse_projection: mat4x4<f32>,
    geometry: vec4<f32>,
    focus: vec4<f32>,
}
@group(0) @binding(2) var<uniform> p: Params;

@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

fn clamp_output(pixel: vec2<i32>) -> vec2<i32> {
    return clamp(pixel, vec2<i32>(0), vec2<i32>(p.geometry.xy) - vec2<i32>(1));
}

fn hdr_at(pixel: vec2<i32>) -> vec4<f32> {
    let uv = (vec2<f32>(clamp_output(pixel)) + vec2<f32>(0.5)) / p.geometry.xy;
    let source_size = vec2<i32>(textureDimensions(hdr));
    let source_pixel = min(vec2<i32>(uv * vec2<f32>(source_size)), source_size - vec2<i32>(1));
    return textureLoad(hdr, source_pixel, 0);
}

fn depth_at(pixel: vec2<i32>) -> f32 {
    let uv = (vec2<f32>(clamp_output(pixel)) + vec2<f32>(0.5)) / p.geometry.xy;
    let coord = min(vec2<i32>(uv * p.geometry.zw), vec2<i32>(p.geometry.zw) - vec2<i32>(1));
    return textureLoad(depth, coord, 0);
}

fn view_distance(pixel: vec2<i32>) -> f32 {
    let uv = (vec2<f32>(clamp_output(pixel)) + vec2<f32>(0.5)) / p.geometry.xy;
    let clip = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), depth_at(pixel), 1.0);
    let view = p.inverse_projection * clip;
    return abs(view.z / view.w);
}

@fragment fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(position.xy);
    let center = hdr_at(pixel);
    let center_distance = view_distance(pixel);
    let radius = min(abs(center_distance - p.focus.x) / p.focus.y, 1.0) * p.focus.z;
    if radius < 0.75 {
        return center;
    }
    // Two interleaved rings keep the shader to sixteen bounded taps.
    let offsets = array<vec2<f32>, 16>(
        vec2<f32>(1.0, 0.0), vec2<f32>(0.7071, 0.7071),
        vec2<f32>(0.0, 1.0), vec2<f32>(-0.7071, 0.7071),
        vec2<f32>(-1.0, 0.0), vec2<f32>(-0.7071, -0.7071),
        vec2<f32>(0.0, -1.0), vec2<f32>(0.7071, -0.7071),
        vec2<f32>(0.4619, 0.1913), vec2<f32>(0.1913, 0.4619),
        vec2<f32>(-0.1913, 0.4619), vec2<f32>(-0.4619, 0.1913),
        vec2<f32>(-0.4619, -0.1913), vec2<f32>(-0.1913, -0.4619),
        vec2<f32>(0.1913, -0.4619), vec2<f32>(0.4619, -0.1913));
    var sum = center;
    var weight = 1.0;
    for (var i = 0; i < 16; i = i + 1) {
        let sample_pixel = clamp_output(pixel + vec2<i32>(round(offsets[i] * radius)));
        // Reject foreground intrusion and background leakage at both sides
        // of a silhouette; the centre is always retained.
        if abs(view_distance(sample_pixel) - center_distance) <= p.focus.w {
            sum += hdr_at(sample_pixel);
            weight += 1.0;
        }
    }
    return sum / weight;
}
