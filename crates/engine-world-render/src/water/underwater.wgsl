// world.water.underwater: when the view's camera is below a water surface, every pixel is
// fogged by the distance to what it shows: mix(fog, scene * exp(-absorption d), exp(-density d)),
// matching engine_water::Underwater::fog_factor.
#import engine::view
#import engine::fullscreen

struct Fog {
    // rgb fog colour, w density per metre.
    color_density: vec4<f32>,
    // rgb absorption per metre.
    absorption: vec4<f32>,
}

@group(1) @binding(0) var<uniform> fog: Fog;
@group(1) @binding(1) var scene_color: texture_2d<f32>;
@group(1) @binding(2) var scene_depth: texture_depth_2d;

@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    return engine_fullscreen_position(index);
}

@fragment fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let texel = vec2<i32>(position.xy);
    let color = textureLoad(scene_color, texel, 0).rgb;
    let depth = textureLoad(scene_depth, texel, 0);
    let size = vec2<f32>(textureDimensions(scene_depth));
    let uv = position.xy / size;
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let p = engine_view.world_from_clip * vec4<f32>(ndc, depth, 1.0);
    let distance_to = select(distance(engine_camera_position(), p.xyz / p.w), 10000.0, depth >= 1.0);
    let clear = exp(-fog.color_density.w * distance_to);
    let absorbed = color * exp(-fog.absorption.xyz * distance_to);
    return vec4<f32>(mix(fog.color_density.rgb, absorbed, clear), 1.0);
}
