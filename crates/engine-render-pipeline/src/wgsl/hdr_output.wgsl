// engine.hdr_output: display-linear colour (1.0 = paper white) to the signal
// of an HDR surface (scRGB or PQ). The source covers the whole target.
#import engine::fullscreen
#import engine::hdr

@group(0) @binding(0) var scene: texture_2d<f32>;
// x: paper white nits, y: peak nits, z: encoding (0 scRGB, 1 PQ)
@group(0) @binding(1) var<uniform> p: vec4<f32>;

@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    return engine_fullscreen_position(index);
}

@fragment fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let color = textureLoad(scene, vec2<i32>(position.xy), 0);
    return vec4<f32>(engine_hdr_encode(u32(p.z), color.rgb, p.x, p.y), 1.0);
}
