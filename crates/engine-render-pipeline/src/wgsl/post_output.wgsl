// engine.tonemap: HDR (exposed and graded by engine.post) to the target.
// Tonemap, then vignette and film grain in display-linear space, then an
// sRGB encode when the target format is not an sRGB view.
#import engine::fullscreen
#import engine::tonemap

@group(0) @binding(0) var scene: texture_2d<f32>;
// 0: origin.xy size.xy (target pixels)
// 1: operator, encode_srgb, vignette_on, grain_on
// 2: vignette intensity, smoothness, center.xy
// 3: vignette color.xyz, grain frame
// 4: grain intensity, grain response
@group(0) @binding(1) var<uniform> p: array<vec4<f32>, 5>;

@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    return engine_fullscreen_position(index);
}

fn pcg(v: u32) -> u32 {
    let state = v * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

@fragment fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = position.xy - p[0].xy;
    let hdr = textureLoad(scene, vec2<i32>(pixel), 0);
    var c = engine_tonemap(u32(p[1].x), hdr.rgb);
    if (p[1].z > 0.5) {
        let size = p[0].zw;
        let aspect = size.x / size.y;
        let uv = pixel / size;
        let d = (uv - p[2].zw) * vec2<f32>(aspect, 1.0);
        let reach = length(vec2<f32>(aspect * 0.5, 0.5));
        let dn = length(d) / reach;
        let inner = 1.0 - p[2].y;
        let v = smoothstep(inner, 1.0, dn);
        c = mix(c, p[3].xyz, p[2].x * v);
    }
    if (p[1].w > 0.5) {
        let px = vec2<u32>(pixel);
        let h = pcg(px.x + pcg(px.y + pcg(u32(p[3].w))));
        let n = f32(h) * (1.0 / 4294967296.0) * 2.0 - 1.0;
        let luma = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
        let amplitude = p[4].x * mix(1.0, 1.0 - luma, p[4].y);
        c = c + vec3<f32>(n * amplitude);
    }
    if (p[1].y > 0.5) {
        c = engine_srgb_encode(c);
    }
    return vec4<f32>(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0)), clamp(hdr.a, 0.0, 1.0));
}
