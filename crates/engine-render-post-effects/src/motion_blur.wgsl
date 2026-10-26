@group(0) @binding(0) var hdr: texture_2d<f32>;
@group(0) @binding(1) var depth: texture_depth_2d;
@group(0) @binding(2) var motion: texture_2d<f32>;
struct Params {
    inverse_projection: mat4x4<f32>,
    geometry: vec4<f32>,
    blur: vec4<f32>,
}
@group(0) @binding(3) var<uniform> p: Params;

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

fn render_pixel(pixel: vec2<i32>) -> vec2<i32> {
    let uv = (vec2<f32>(clamp_output(pixel)) + vec2<f32>(0.5)) / p.geometry.xy;
    return min(vec2<i32>(uv * p.geometry.zw), vec2<i32>(p.geometry.zw) - vec2<i32>(1));
}

fn view_distance(pixel: vec2<i32>) -> f32 {
    let uv = (vec2<f32>(clamp_output(pixel)) + vec2<f32>(0.5)) / p.geometry.xy;
    let clip = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), textureLoad(depth, render_pixel(pixel), 0), 1.0);
    let view = p.inverse_projection * clip;
    return abs(view.z / view.w);
}

@fragment fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(position.xy);
    let center = hdr_at(pixel);
    if p.blur.w > 0.5 {
        return center;
    }
    // Prepass vectors are previous-current in render pixels, x right/y down.
    // Scale each axis separately; post colour is at output resolution.
    let vector = textureLoad(motion, render_pixel(pixel), 0).xy * (p.geometry.xy / p.geometry.zw) * p.blur.x;
    let span = length(vector);
    if span < 0.75 {
        return center;
    }
    let path = vector * min(1.0, p.blur.y / span);
    let center_distance = view_distance(pixel);
    var sum = center;
    var weight = 1.0;
    // Nine taps including centre, symmetrically spanning the shutter path.
    for (var i = -4; i <= 4; i = i + 1) {
        if i == 0 { continue; }
        let sample_pixel = clamp_output(pixel + vec2<i32>(round(path * f32(i) / 4.0)));
        if abs(view_distance(sample_pixel) - center_distance) <= p.blur.z {
            sum += hdr_at(sample_pixel);
            weight += 1.0;
        }
    }
    return sum / weight;
}
