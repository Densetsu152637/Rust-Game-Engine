// engine.post bloom: threshold prefilter, dual-filter downsample, tent
// upsample (added into the coarser-to-finer chain with a blend constant).
#import engine::fullscreen

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
// 0: source texel size.xy, threshold, knee (absolute)
// 1: exposure mode, manual scale, destination size.xy
@group(0) @binding(2) var<uniform> p: array<vec4<f32>, 2>;
@group(0) @binding(3) var<storage, read> auto_exposure: array<vec4<f32>, 1>;

@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    return engine_fullscreen_position(index);
}

fn uv_of(position: vec4<f32>, target_size: vec2<f32>) -> vec2<f32> {
    return position.xy / target_size;
}

fn tap(uv: vec2<f32>, offset: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(src, samp, uv + offset * p[0].xy, 0.0).rgb;
}

fn threshold_tap(uv: vec2<f32>, offset: vec2<f32>, scale: f32) -> vec3<f32> {
    let c = max(tap(uv, offset), vec3<f32>(0.0)) * scale;
    let threshold = p[0].z;
    let knee = max(p[0].w, 1e-5);
    let brightness = max(c.r, max(c.g, c.b));
    var soft = clamp(brightness - threshold + knee, 0.0, 2.0 * knee);
    soft = soft * soft / (4.0 * knee + 1e-5);
    let contribution = max(soft, brightness - threshold) / max(brightness, 1e-5);
    return c * contribution;
}

@fragment fn fs_prefilter(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = uv_of(position, p[1].zw);
    var scale = p[1].y;
    if (p[1].x > 1.5) {
        scale = auto_exposure[0].x;
    }
    let sum = threshold_tap(uv, vec2<f32>(-1.0, -1.0), scale)
        + threshold_tap(uv, vec2<f32>(1.0, -1.0), scale)
        + threshold_tap(uv, vec2<f32>(-1.0, 1.0), scale)
        + threshold_tap(uv, vec2<f32>(1.0, 1.0), scale);
    return vec4<f32>(sum * 0.25, 1.0);
}

@fragment fn fs_down(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = uv_of(position, p[1].zw);
    let sum = tap(uv, vec2<f32>(0.0)) * 4.0
        + tap(uv, vec2<f32>(-1.0, -1.0))
        + tap(uv, vec2<f32>(1.0, -1.0))
        + tap(uv, vec2<f32>(-1.0, 1.0))
        + tap(uv, vec2<f32>(1.0, 1.0));
    return vec4<f32>(sum * 0.125, 1.0);
}

@fragment fn fs_up(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = uv_of(position, p[1].zw);
    let sum = tap(uv, vec2<f32>(-2.0, 0.0))
        + tap(uv, vec2<f32>(2.0, 0.0))
        + tap(uv, vec2<f32>(0.0, -2.0))
        + tap(uv, vec2<f32>(0.0, 2.0))
        + 2.0 * (tap(uv, vec2<f32>(-1.0, -1.0))
            + tap(uv, vec2<f32>(1.0, -1.0))
            + tap(uv, vec2<f32>(-1.0, 1.0))
            + tap(uv, vec2<f32>(1.0, 1.0)));
    return vec4<f32>(sum * (1.0 / 12.0), 0.0);
}
