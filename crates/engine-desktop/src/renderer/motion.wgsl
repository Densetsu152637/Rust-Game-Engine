// Engine-owned depth/motion pass for temporal upscaling. Rasterizes with the
// jittered matrix used by the color pass; motion uses unjittered clip positions.
struct Motion {
    jittered: mat4x4<f32>,
    current: mat4x4<f32>,
    previous: mat4x4<f32>,
    // xy: render size in pixels; zw unused.
    size: vec4<f32>,
}
@group(0) @binding(0) var<uniform> motion: Motion;
struct Output {
    @builtin(position) position: vec4<f32>,
    @location(0) current: vec4<f32>,
    @location(1) previous: vec4<f32>,
}
@vertex fn vs_main(@location(0) position: vec3<f32>) -> Output {
    var out: Output;
    let p = vec4<f32>(position, 1.0);
    out.position = motion.jittered * p;
    out.current = motion.current * p;
    out.previous = motion.previous * p;
    return out;
}
// xy: current-to-previous motion in render pixels (y down); z: clip depth 0..1.
@fragment fn fs_main(input: Output) -> @location(0) vec4<f32> {
    var delta = vec2<f32>(0.0);
    if input.current.w > 0.000001 && input.previous.w > 0.000001 {
        let current = input.current.xy / input.current.w;
        let previous = input.previous.xy / input.previous.w;
        delta = (previous - current) * vec2<f32>(0.5, -0.5) * motion.size.xy;
    }
    return vec4<f32>(delta, input.position.z, 1.0);
}
