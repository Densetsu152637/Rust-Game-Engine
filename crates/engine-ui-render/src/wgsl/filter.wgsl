// ui.color_filter - the colour-vision filter: one 3x3 matrix on the sampled final linear
// colour (see color.rs for the matrices and their sources).
#import engine::fullscreen

@group(0) @binding(0) var scene: texture_2d<f32>;
// The matrix rows (xyz used).
@group(0) @binding(1) var<uniform> rows: array<vec4<f32>, 3>;

@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    return engine_fullscreen_position(index);
}

@fragment fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let source = textureLoad(scene, vec2<i32>(position.xy), 0);
    let c = source.rgb;
    let filtered = vec3<f32>(dot(rows[0].xyz, c), dot(rows[1].xyz, c), dot(rows[2].xyz, c));
    return vec4<f32>(max(filtered, vec3<f32>(0.0)), source.a);
}
