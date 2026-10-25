// The default output operator: linear HDR clamped to 0..1 into the target
// (an sRGB target encodes it), matching the pre-HDR appearance.
#import engine::fullscreen

@group(0) @binding(0) var scene: texture_2d<f32>;
// xy: viewport origin in target pixels.
@group(0) @binding(1) var<uniform> origin: vec4<f32>;

@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    return engine_fullscreen_position(index);
}

@fragment fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let color = textureLoad(scene, vec2<i32>(position.xy - origin.xy), 0);
    return clamp(color, vec4<f32>(0.0), vec4<f32>(1.0));
}
