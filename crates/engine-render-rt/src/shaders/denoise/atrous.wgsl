// One edge-aware a-trous wavelet iteration: 5 x 5 B3-spline taps spaced
// pass_info.x pixels apart, weighted by depth (plane distance), normal and
// luminance (relative to the 3 x 3 blurred standard deviation) edge stops.
// The variance is filtered with the squared weights.

@group(0) @binding(3) var da_input: texture_2d<f32>;
@group(0) @binding(4) var da_moments: texture_2d<f32>;
@group(0) @binding(5) var da_output: texture_storage_2d<rgba32float, write>;

fn da_kernel(i: i32) -> f32 {
    if i == 0 {
        return 0.375;
    }
    if i == 1 {
        return 0.25;
    }
    return 0.0625;
}

fn da_valid(q: vec2<i32>) -> bool {
    return dn_in_bounds(q) && textureLoad(da_moments, q, 0).w >= 0.5;
}

@compute @workgroup_size(8, 8)
fn atrous(@builtin(global_invocation_id) id: vec3<u32>) {
    let pixel = vec2<i32>(id.xy);
    if !dn_in_bounds(pixel) {
        return;
    }
    let center = textureLoad(da_input, pixel, 0);
    if !da_valid(pixel) {
        textureStore(da_output, pixel, center);
        return;
    }
    var blurred = 0.0;
    var blur_w = 0.0;
    for (var dy = -1; dy <= 1; dy += 1) {
        for (var dx = -1; dx <= 1; dx += 1) {
            let q = pixel + vec2<i32>(dx, dy);
            if !da_valid(q) {
                continue;
            }
            let k = select(0.25, 0.5, dx == 0) * select(0.25, 0.5, dy == 0);
            blurred += k * textureLoad(da_input, q, 0).w;
            blur_w += k;
        }
    }
    let sigma = sqrt(max(blurred / blur_w, 0.0));
    let s = dn_surface(pixel);
    let lp = dn_luminance(center.xyz);
    let step = i32(dn.pass_info.x);
    let k0 = da_kernel(0) * da_kernel(0);
    var sum_c = center.xyz * k0;
    var sum_v = k0 * k0 * center.w;
    var sum_w = k0;
    for (var dy = -2; dy <= 2; dy += 1) {
        for (var dx = -2; dx <= 2; dx += 1) {
            if dx == 0 && dy == 0 {
                continue;
            }
            let q = pixel + vec2<i32>(dx, dy) * step;
            if !da_valid(q) {
                continue;
            }
            let sq = dn_surface(q);
            let cq = textureLoad(da_input, q, 0);
            let w_l = exp(-abs(lp - dn_luminance(cq.xyz)) / (dn.edge.z * sigma + 1e-6));
            let w = da_kernel(abs(dx)) * da_kernel(abs(dy)) * dn_depth_weight(s, sq) * dn_normal_weight(s, sq) * w_l;
            sum_c += w * cq.xyz;
            sum_v += w * w * cq.w;
            sum_w += w;
        }
    }
    textureStore(da_output, pixel, vec4<f32>(sum_c / sum_w, sum_v / (sum_w * sum_w)));
}
