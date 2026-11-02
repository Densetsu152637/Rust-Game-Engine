// engine.atmosphere.aerial: opaque geometry becomes
// scene * transmittance + in-scatter along its view distance. Pixels at the
// far plane pass through (the sky pass owns them). Composed after atmosphere.wgsl.
@group(1) @binding(0) var<uniform> atmosphere: Atmosphere;
@group(1) @binding(1) var scene_color: texture_2d<f32>;
@group(1) @binding(2) var scene_depth: texture_depth_2d;

@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

@fragment fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(frag.xy);
    let color = textureLoad(scene_color, pixel, 0);
    let depth = textureLoad(scene_depth, pixel, 0);
    var sun = vec3<f32>(0.0, 1.0, 0.0);
    var irradiance = vec3<f32>(0.0);
    if depth >= 1.0 || atmosphere.layer.z <= 0.0 || !atm_sun_irradiance(&sun, &irradiance) {
        return color;
    }
    let dir = atm_view_direction(frag.xy);
    let distance = scatter_view_distance(frag.xy, depth);
    let m = atm_aerial(dir, sun, irradiance, distance * atmosphere.layer.z);
    return vec4<f32>(color.rgb * m.transmittance + m.inscatter, color.a);
}
