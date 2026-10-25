// engine::tonemap - display transforms. Mirrors src/post/tonemap.rs.
// Operator codes: 0 clamp, 1 ACES (Narkowicz), 2 AgX, 3 AgX punchy,
// 4 Khronos PBR Neutral. Input is scene-linear sRGB; output is
// display-linear 0..1 (the target encodes it, or engine_srgb_encode does).

fn engine_rows(r0: vec3<f32>, r1: vec3<f32>, r2: vec3<f32>, v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(dot(r0, v), dot(r1, v), dot(r2, v));
}

fn engine_aces(x: vec3<f32>) -> vec3<f32> {
    let c = max(x, vec3<f32>(0.0));
    return clamp((c * (2.51 * c + 0.03)) / (c * (2.43 * c + 0.59) + 0.14), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn engine_agx_contrast(x: vec3<f32>) -> vec3<f32> {
    let x2 = x * x;
    let x4 = x2 * x2;
    return 15.5 * x4 * x2 - 40.14 * x4 * x + 31.96 * x4 - 6.868 * x2 * x + 0.4298 * x2 + 0.1191 * x - 0.00232;
}

fn engine_agx(color: vec3<f32>, punchy: bool) -> vec3<f32> {
    var c = engine_rows(
        vec3<f32>(0.856627153, 0.095121240, 0.048251606),
        vec3<f32>(0.137318972, 0.761241990, 0.101439036),
        vec3<f32>(0.111898213, 0.076799418, 0.811302368),
        engine_rows(
            vec3<f32>(0.6274, 0.3293, 0.0433),
            vec3<f32>(0.0691, 0.9195, 0.0114),
            vec3<f32>(0.0164, 0.0880, 0.8956),
            color));
    let min_ev = -12.47393;
    let max_ev = 4.026069;
    c = clamp((log2(max(c, vec3<f32>(1e-10))) - min_ev) / (max_ev - min_ev), vec3<f32>(0.0), vec3<f32>(1.0));
    c = engine_agx_contrast(c);
    if (punchy) {
        c = pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.35));
        let luma = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
        c = vec3<f32>(luma) + 1.4 * (c - vec3<f32>(luma));
    }
    c = engine_rows(
        vec3<f32>(1.1271006, -0.11000664, -0.01649394),
        vec3<f32>(-0.14132976, 1.1578237, -0.01649394),
        vec3<f32>(-0.14132976, -0.11060664, 1.2519364),
        c);
    c = pow(max(c, vec3<f32>(0.0)), vec3<f32>(2.2));
    c = engine_rows(
        vec3<f32>(1.6605, -0.5876, -0.0728),
        vec3<f32>(-0.1246, 1.1329, -0.0083),
        vec3<f32>(-0.0182, -0.1006, 1.1187),
        c);
    return clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
}

fn engine_neutral(color: vec3<f32>) -> vec3<f32> {
    let start = 0.76;
    let desat = 0.15;
    let x = min(color.r, min(color.g, color.b));
    var offset = 0.04;
    if (x < 0.08) {
        offset = x - 6.25 * x * x;
    }
    var c = color - vec3<f32>(offset);
    let peak = max(c.r, max(c.g, c.b));
    if (peak < start) {
        return c;
    }
    let d = 1.0 - start;
    let new_peak = 1.0 - d * d / (peak + d - start);
    c = c * (new_peak / peak);
    let g = 1.0 - 1.0 / (desat * (peak - new_peak) + 1.0);
    return mix(c, vec3<f32>(new_peak), g);
}

fn engine_tonemap(op: u32, color: vec3<f32>) -> vec3<f32> {
    switch op {
        case 1u: { return engine_aces(color); }
        case 2u: { return engine_agx(color, false); }
        case 3u: { return engine_agx(color, true); }
        case 4u: { return clamp(engine_neutral(color), vec3<f32>(0.0), vec3<f32>(1.0)); }
        default: { return clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)); }
    }
}

fn engine_srgb_encode(linear: vec3<f32>) -> vec3<f32> {
    let l = clamp(linear, vec3<f32>(0.0), vec3<f32>(1.0));
    let low = l * 12.92;
    let high = 1.055 * pow(l, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(high, low, l <= vec3<f32>(0.0031308));
}
