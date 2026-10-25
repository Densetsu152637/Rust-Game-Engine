// engine.post auto-exposure: a log2 luminance histogram of the HDR colour,
// then a one-thread reduction that averages the percentile range and adapts
// the exposure over time.

@group(0) @binding(0) var scene: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> histogram: array<atomic<u32>, 64>;
// 0: width, height, min_ev, max_ev
// 1: low percentile, high percentile, compensation, reset (0/1)
// 2: speed_up, speed_down, delta_time
@group(0) @binding(2) var<uniform> p: array<vec4<f32>, 3>;
// x: exposure scale, y: current EV, z: target EV, w: initialised.
@group(0) @binding(3) var<storage, read_write> state: array<vec4<f32>, 1>;

var<workgroup> local_bins: array<atomic<u32>, 64>;

@compute @workgroup_size(16, 16)
fn cs_histogram(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_index) li: u32,
) {
    if (li < 64u) {
        atomicStore(&local_bins[li], 0u);
    }
    workgroupBarrier();
    if (f32(gid.x) < p[0].x && f32(gid.y) < p[0].y) {
        let c = textureLoad(scene, vec2<i32>(gid.xy), 0).rgb;
        let lum = dot(max(c, vec3<f32>(0.0)), vec3<f32>(0.2126, 0.7152, 0.0722));
        var bin = 0u;
        if (lum > 1e-5) {
            let t = clamp((log2(lum) - p[0].z) / (p[0].w - p[0].z), 0.0, 1.0);
            bin = 1u + u32(t * 62.999);
        }
        atomicAdd(&local_bins[bin], 1u);
    }
    workgroupBarrier();
    if (li < 64u) {
        atomicAdd(&histogram[li], atomicLoad(&local_bins[li]));
    }
}

@compute @workgroup_size(1)
fn cs_reduce() {
    var counts: array<f32, 64>;
    var total = 0.0;
    for (var i = 0u; i < 64u; i = i + 1u) {
        let n = f32(atomicExchange(&histogram[i], 0u));
        counts[i] = n;
        if (i > 0u) {
            total = total + n;
        }
    }
    let previous = state[0];
    var target_ev = previous.z;
    if (total > 0.0) {
        let lo = total * p[1].x;
        let hi = total * p[1].y;
        var cumulative = 0.0;
        var weight_sum = 0.0;
        var log_sum = 0.0;
        for (var i = 1u; i < 64u; i = i + 1u) {
            let n = counts[i];
            let w = clamp(min(cumulative + n, hi) - max(cumulative, lo), 0.0, n);
            let centre = p[0].z + ((f32(i - 1u) + 0.5) / 63.0) * (p[0].w - p[0].z);
            weight_sum = weight_sum + w;
            log_sum = log_sum + w * centre;
            cumulative = cumulative + n;
        }
        if (weight_sum > 0.0) {
            let average_log = log_sum / weight_sum;
            target_ev = log2(0.18) - average_log + p[1].z;
        }
    }
    var current = previous.y;
    if (previous.w < 0.5 || p[1].w > 0.5) {
        current = target_ev;
    } else {
        var speed = p[2].y;
        if (target_ev > current) {
            speed = p[2].x;
        }
        current = current + (target_ev - current) * (1.0 - exp(-speed * p[2].z));
    }
    state[0] = vec4<f32>(exp2(current), current, target_ev, 1.0);
}
