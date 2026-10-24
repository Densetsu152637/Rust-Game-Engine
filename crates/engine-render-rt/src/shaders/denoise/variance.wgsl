// Variance estimate: pixels with a history shorter than pass_info.y frames
// replace the temporal variance by the variance of luminance over an
// edge-aware 7 x 7 neighbourhood, scaled up by pass_info.y / history length.

@group(0) @binding(3) var dv_input: texture_2d<f32>;
@group(0) @binding(4) var dv_moments: texture_2d<f32>;
@group(0) @binding(5) var dv_output: texture_storage_2d<rgba32float, write>;

@compute @workgroup_size(8, 8)
fn variance(@builtin(global_invocation_id) id: vec3<u32>) {
    let pixel = vec2<i32>(id.xy);
    if !dn_in_bounds(pixel) {
        return;
    }
    let center = textureLoad(dv_input, pixel, 0);
    let m = textureLoad(dv_moments, pixel, 0);
    let min_history = f32(dn.pass_info.y);
    if m.w < 0.5 || m.z >= min_history {
        textureStore(dv_output, pixel, center);
        return;
    }
    let s = dn_surface(pixel);
    var sum_w = 0.0;
    var m1 = 0.0;
    var m2 = 0.0;
    for (var dy = -3; dy <= 3; dy += 1) {
        for (var dx = -3; dx <= 3; dx += 1) {
            let q = pixel + vec2<i32>(dx, dy);
            if !dn_in_bounds(q) || textureLoad(dv_moments, q, 0).w < 0.5 {
                continue;
            }
            let sq = dn_surface(q);
            let w = dn_depth_weight(s, sq) * dn_normal_weight(s, sq);
            let lq = dn_luminance(textureLoad(dv_input, q, 0).xyz);
            m1 += w * lq;
            m2 += w * lq * lq;
            sum_w += w;
        }
    }
    m1 /= sum_w;
    m2 /= sum_w;
    let v = max(0.0, m2 - m1 * m1) * min_history / max(m.z, 1.0);
    textureStore(dv_output, pixel, vec4<f32>(center.xyz, v));
}
