// engine.atmosphere: sky, planet and sun disk where scene depth is at the far
// plane, added to the background. Composed after atmosphere.wgsl.
@group(1) @binding(0) var<uniform> atmosphere: Atmosphere;

@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    // Depth 1.0: the LessEqual test keeps only pixels still at the far plane.
    return vec4<f32>(uv * 2.0 - 1.0, 1.0, 1.0);
}

@fragment fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    var sun = vec3<f32>(0.0, 1.0, 0.0);
    var irradiance = vec3<f32>(0.0);
    if !atm_sun_irradiance(&sun, &irradiance) {
        return vec4<f32>(0.0);
    }
    let radiance = atm_sky(atm_view_direction(frag.xy), sun, irradiance);
    return vec4<f32>(radiance, 0.0);
}
