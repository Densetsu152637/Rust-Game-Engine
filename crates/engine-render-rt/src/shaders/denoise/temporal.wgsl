// Temporal accumulation: bilinear reprojection through the motion vectors
// (current-to-previous, in pixels), per-tap rejection on view-distance and
// normal disocclusion, exponential moving averages of the signal and of the
// first two luminance moments with alpha = 1 / history length. The output
// variance is the moment variance divided by the history length (the
// variance of the accumulated mean, which the a-trous luminance stop uses).

@group(0) @binding(3) var tp_signal: texture_2d<f32>;
@group(0) @binding(4) var tp_motion: texture_2d<f32>;
@group(0) @binding(5) var tp_prev_history: texture_2d<f32>;
@group(0) @binding(6) var tp_prev_moments: texture_2d<f32>;
@group(0) @binding(7) var tp_prev_geometry: texture_2d<f32>;
// rgb: integrated signal, a: variance of the integrated signal.
@group(0) @binding(8) var tp_integrated: texture_storage_2d<rgba32float, write>;
// x, y: luminance moments; z: history length; w: 1 = valid pixel.
@group(0) @binding(9) var tp_moments: texture_storage_2d<rgba32float, write>;
// xyz: normal; w: distance from the camera (0 = no geometry).
@group(0) @binding(10) var tp_geometry: texture_storage_2d<rgba32float, write>;

@compute @workgroup_size(8, 8)
fn temporal(@builtin(global_invocation_id) id: vec3<u32>) {
    let pixel = vec2<i32>(id.xy);
    if !dn_in_bounds(pixel) {
        return;
    }
    let s = dn_surface(pixel);
    let raw = textureLoad(tp_signal, pixel, 0);
    if !s.valid || (dn.size.z == DN_RGB_HIT && raw.w < 0.0) {
        textureStore(tp_integrated, pixel, vec4<f32>(dn_signal(raw), 0.0));
        textureStore(tp_moments, pixel, vec4<f32>(0.0));
        textureStore(tp_geometry, pixel, vec4<f32>(0.0));
        return;
    }
    let c = dn_signal(raw);
    let l = dn_luminance(c);
    var history = vec3<f32>(0.0);
    var moments = vec2<f32>(0.0);
    var history_sum = 0.0;
    var weight = 0.0;
    if dn.size.w == 0u {
        // Texel-centre coordinates of the previous frame: pixel + motion.
        let previous = vec2<f32>(pixel) + textureLoad(tp_motion, pixel, 0).xy;
        let base = floor(previous);
        let f = previous - base;
        for (var t = 0; t < 4; t += 1) {
            let o = vec2<i32>(t & 1, t >> 1u);
            let q = vec2<i32>(base) + o;
            let w = select(1.0 - f.x, f.x, o.x == 1) * select(1.0 - f.y, f.y, o.y == 1);
            if w <= 0.0 || !dn_in_bounds(q) {
                continue;
            }
            let g = textureLoad(tp_prev_geometry, q, 0);
            if g.w <= 0.0 || abs(g.w - s.depth) > dn.temporal.x * s.depth || dot(g.xyz, s.normal) < dn.temporal.y {
                continue;
            }
            let m = textureLoad(tp_prev_moments, q, 0);
            history += w * textureLoad(tp_prev_history, q, 0).xyz;
            moments += w * m.xy;
            history_sum += w * m.z;
            weight += w;
        }
    }
    var new_length = 1.0;
    if weight > 1e-3 {
        history /= weight;
        moments /= weight;
        new_length = min(history_sum / weight + 1.0, dn.temporal.z);
    }
    let alpha = 1.0 / new_length;
    let color = mix(history, c, alpha);
    let m = mix(moments, vec2<f32>(l, l * l), alpha);
    // Variance of the accumulated estimate: the moment variance of the
    // samples over the number of frames averaged.
    textureStore(tp_integrated, pixel, vec4<f32>(color, max(0.0, m.y - m.x * m.x) / new_length));
    textureStore(tp_moments, pixel, vec4<f32>(m, new_length, 1.0));
    textureStore(tp_geometry, pixel, vec4<f32>(s.normal, s.depth));
}
