// The default upscale: bilinear resample of render-size HDR to the output size.
#import engine::fullscreen

@group(0) @binding(0) var scene: texture_2d<f32>;
@group(0) @binding(1) var scene_sampler: sampler;
// xy: 1 / output size.
@group(0) @binding(2) var<uniform> scale: vec4<f32>;

@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    return engine_fullscreen_position(index);
}

@fragment fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    return textureSampleLevel(scene, scene_sampler, position.xy * scale.xy, 0.0);
}
