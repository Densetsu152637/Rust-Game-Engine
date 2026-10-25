// Output: the filtered signal in the signal's layout (scalar in x; rgb with
// alpha 1, or the input hit distance for DN_RGB_HIT). Pixels excluded from
// filtering (no geometry, or a negative hit-distance flag) pass the raw
// signal through unchanged.

@group(0) @binding(3) var dr_filtered: texture_2d<f32>;
@group(0) @binding(4) var dr_moments: texture_2d<f32>;
@group(0) @binding(5) var dr_signal: texture_2d<f32>;
@group(0) @binding(6) var dr_output: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(8, 8)
fn resolve(@builtin(global_invocation_id) id: vec3<u32>) {
    let pixel = vec2<i32>(id.xy);
    if !dn_in_bounds(pixel) {
        return;
    }
    let raw = textureLoad(dr_signal, pixel, 0);
    if textureLoad(dr_moments, pixel, 0).w < 0.5 {
        textureStore(dr_output, pixel, raw);
        return;
    }
    let f = textureLoad(dr_filtered, pixel, 0).xyz;
    var out = vec4<f32>(f, 1.0);
    if dn.size.z == DN_SCALAR {
        out = vec4<f32>(f.x, 0.0, 0.0, 1.0);
    } else if dn.size.z == DN_RGB_HIT {
        out = vec4<f32>(f, raw.w);
    }
    textureStore(dr_output, pixel, out);
}
