@group(0) @binding(0) var hdr: texture_2d<f32>;
struct Params { geometry: vec4<f32> }
@group(0) @binding(1) var<uniform> p: Params;

@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

// Coordinates are output pixel centres. The actual source may still be render
// sized when engine.upscale is disabled. Every fetch, including the copy path,
// uses the source texture's real dimensions.
fn color_at(output_pixel: vec2<f32>) -> vec4<f32> {
    let source_size = vec2<i32>(textureDimensions(hdr));
    let last = source_size - vec2<i32>(1);
    let output_pos = clamp(output_pixel, vec2<f32>(0.0), p.geometry.xy - vec2<f32>(1.0));
    let source_pos = (output_pos + vec2<f32>(0.5)) * vec2<f32>(source_size) / p.geometry.xy - vec2<f32>(0.5);
    let low = vec2<i32>(floor(source_pos));
    let fraction = fract(source_pos);
    let a = textureLoad(hdr, clamp(low, vec2<i32>(0), last), 0);
    let b = textureLoad(hdr, clamp(low + vec2<i32>(1, 0), vec2<i32>(0), last), 0);
    let c = textureLoad(hdr, clamp(low + vec2<i32>(0, 1), vec2<i32>(0), last), 0);
    let d = textureLoad(hdr, clamp(low + vec2<i32>(1, 1), vec2<i32>(0), last), 0);
    return mix(mix(a, b, fraction.x), mix(c, d, fraction.x), fraction.y);
}

fn luma(color: vec4<f32>) -> f32 {
    let positive = max(color.rgb, vec3<f32>(0.0));
    let bounded = positive / (vec3<f32>(1.0) + positive);
    return dot(bounded, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// Blending happens in a display-compressed domain. Values up to 1.0 are
// untouched (identity), values above 1.0 follow 2 - 1/x so that a very bright
// side of an edge cannot dominate every partially covered pixel after the
// later tonemap. This is a monotonic, reversible per-channel map; it does not
// change which pixels are filtered, only how the filtered samples are mixed.
fn compress(color: vec3<f32>) -> vec3<f32> {
    let bright = vec3<f32>(2.0) - vec3<f32>(1.0) / max(color, vec3<f32>(1.0));
    return select(bright, color, color <= vec3<f32>(1.0));
}

fn expand(color: vec3<f32>) -> vec3<f32> {
    let limited = min(color, vec3<f32>(1.99999));
    let bright = vec3<f32>(1.0) / (vec3<f32>(2.0) - limited);
    return select(bright, color, color <= vec3<f32>(1.0));
}

@fragment fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = floor(position.xy);
    let center = color_at(pixel);
    if p.geometry.w == 0.0 { return center; }

    let nw = luma(color_at(pixel + vec2<f32>(-1.0, -1.0)));
    let n  = luma(color_at(pixel + vec2<f32>( 0.0, -1.0)));
    let ne = luma(color_at(pixel + vec2<f32>( 1.0, -1.0)));
    let w  = luma(color_at(pixel + vec2<f32>(-1.0,  0.0)));
    let m  = luma(center);
    let e  = luma(color_at(pixel + vec2<f32>( 1.0,  0.0)));
    let sw = luma(color_at(pixel + vec2<f32>(-1.0,  1.0)));
    let s  = luma(color_at(pixel + vec2<f32>( 0.0,  1.0)));
    let se = luma(color_at(pixel + vec2<f32>( 1.0,  1.0)));
    let minimum = min(m, min(min(nw, n), min(ne, min(w, min(e, min(sw, min(s, se)))))));
    let maximum = max(m, max(max(nw, n), max(ne, max(w, max(e, max(sw, max(s, se)))))));
    let contrast = maximum - minimum;
    if contrast <= p.geometry.z * max(maximum, 0.08) || contrast <= 0.00001 {
        return center;
    }

    let gx = ne + 2.0 * e + se - nw - 2.0 * w - sw;
    let gy = sw + 2.0 * s + se - nw - 2.0 * n - ne;
    let length = sqrt(gx * gx + gy * gy);
    if length <= 0.00001 { return center; }
    let tangent = vec2<f32>(-gy, gx) / length;
    let along = (compress(color_at(pixel - tangent * 0.5).rgb)
        + compress(color_at(pixel + tangent * 0.5).rgb)
        + compress(color_at(pixel - tangent).rgb)
        + compress(color_at(pixel + tangent).rgb)) * 0.25;
    return vec4<f32>(expand(mix(compress(center.rgb), along, p.geometry.w)), center.a);
}
