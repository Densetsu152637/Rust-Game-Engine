@group(0) @binding(0) var hdr: texture_2d<f32>;
struct Params { geometry: vec4<f32> }
@group(0) @binding(1) var<uniform> p: Params;

@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

// Blending happens in a display-compressed domain. Each colour is scaled by
// one scalar weight derived from its brightest channel: values with a peak up
// to 1.0 are untouched (identity), a larger peak m maps to 2 - 1/m. Scaling the
// whole colour keeps its hue and channel ratios exactly, the map is monotonic
// and reversible, and a very bright side of an edge cannot dominate every
// partially covered pixel after the later tonemap. The peak channel is used
// rather than luminance because the display operator is applied per channel:
// luminance barely compresses a saturated colour (an 8.0 red has luminance 1.7)
// and left its brightest channel far above the display range.
fn compress(color: vec3<f32>) -> vec3<f32> {
    let positive = max(color, vec3<f32>(0.0));
    let peak = max(positive.r, max(positive.g, positive.b));
    if peak <= 1.0 { return positive; }
    return positive * ((2.0 - 1.0 / peak) / peak);
}

fn expand(color: vec3<f32>) -> vec3<f32> {
    let peak = max(color.r, max(color.g, color.b));
    if peak <= 1.0 { return color; }
    let limited = min(peak, 1.99999);
    return color * (1.0 / ((2.0 - limited) * peak));
}

// Coordinates are output pixel centres. The actual source may still be render
// sized when engine.upscale is disabled. Every fetch, including the copy path,
// uses the source texture's real dimensions. Texels are compressed before the
// bilinear mix so interpolated samples live in the same domain as the blend;
// alpha is returned untouched.
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
    let mixed = mix(
        mix(vec4<f32>(compress(a.rgb), a.a), vec4<f32>(compress(b.rgb), b.a), fraction.x),
        mix(vec4<f32>(compress(c.rgb), c.a), vec4<f32>(compress(d.rgb), d.a), fraction.x),
        fraction.y);
    return mixed;
}

// Contrast is measured on the tone-bounded luma of the original (expanded)
// colour so thresholds keep their meaning for HDR values.
fn luma(color: vec4<f32>) -> f32 {
    let positive = expand(max(color.rgb, vec3<f32>(0.0)));
    let bounded = positive / (vec3<f32>(1.0) + positive);
    return dot(bounded, vec3<f32>(0.2126, 0.7152, 0.0722));
}

@fragment fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = floor(position.xy);
    let center = color_at(pixel);
    let original = vec4<f32>(expand(center.rgb), center.a);
    if p.geometry.w == 0.0 { return original; }

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
        return original;
    }

    let gx = ne + 2.0 * e + se - nw - 2.0 * w - sw;
    let gy = sw + 2.0 * s + se - nw - 2.0 * n - ne;
    let length = sqrt(gx * gx + gy * gy);
    if length <= 0.00001 { return original; }
    let tangent = vec2<f32>(-gy, gx) / length;
    let along = (color_at(pixel - tangent * 0.5).rgb
        + color_at(pixel + tangent * 0.5).rgb
        + color_at(pixel - tangent).rgb
        + color_at(pixel + tangent).rgb) * 0.25;
    // Strength guard. The compressed domain keeps the display-space result at
    // least as good as the unfiltered pixel while the expand map can still
    // represent the mix. Its range ends at a compressed peak of 2 (a source
    // peak of 1e5), where 2 - 1/m is no longer distinguishable from 2, so the
    // blend fades to zero between compressed peaks 1.999 (source 1e3) and 1.9999
    // (source 1e4) and such pixels keep their original colour.
    let peak = max(max(center.r, max(center.g, center.b)), max(along.r, max(along.g, along.b)));
    let strength = p.geometry.w * clamp((1.9999 - peak) / 0.0009, 0.0, 1.0);
    return vec4<f32>(expand(mix(center.rgb, along, strength)), center.a);
}
