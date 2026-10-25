// engine.post composite: exposure, bloom, HDR colour grading.
#import engine::fullscreen

@group(0) @binding(0) var scene: texture_2d<f32>;
@group(0) @binding(1) var bloom: texture_2d<f32>;
@group(0) @binding(2) var lut: texture_3d<f32>;
@group(0) @binding(3) var samp: sampler;
// 0: exposure mode, manual scale, bloom_on, bloom intensity
// 1: bloom tint.xyz, grading_on
// 2: white balance.xyz, exposure offset scale
// 3: contrast, saturation, hue shift (radians), lut_on
// 4: lift.xyz, lut intensity
// 5: 1/gamma.xyz, lut size
// 6: gain.xyz
// 7: lut domain min.xyz
// 8: lut domain max.xyz
@group(0) @binding(4) var<uniform> p: array<vec4<f32>, 9>;
// x: exposure scale, y: current EV, z: target EV, w: initialised.
@group(0) @binding(5) var<storage, read> auto_exposure: array<vec4<f32>, 1>;

@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    return engine_fullscreen_position(index);
}

fn grade(input: vec3<f32>) -> vec3<f32> {
    var c = max(input, vec3<f32>(0.0));
    c = c * p[2].xyz * p[2].w;
    c = 0.18 * pow(max(c, vec3<f32>(1e-6)) / 0.18, vec3<f32>(p[3].x));
    let luma = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    c = vec3<f32>(luma) + (c - vec3<f32>(luma)) * p[3].y;
    let angle = p[3].z;
    if (angle != 0.0) {
        let k = vec3<f32>(0.57735027);
        c = c * cos(angle) + cross(k, c) * sin(angle) + k * dot(k, c) * (1.0 - cos(angle));
    }
    c = max(c * p[6].xyz + p[4].xyz, vec3<f32>(0.0));
    c = pow(c, p[5].xyz);
    if (p[3].w > 0.5) {
        let v = c / (vec3<f32>(1.0) + c);
        let t = clamp((v - p[7].xyz) / (p[8].xyz - p[7].xyz), vec3<f32>(0.0), vec3<f32>(1.0));
        let n = p[5].w;
        let uvw = t * ((n - 1.0) / n) + vec3<f32>(0.5 / n);
        let mapped = clamp(textureSampleLevel(lut, samp, uvw, 0.0).rgb, vec3<f32>(0.0), vec3<f32>(0.9995));
        c = mix(c, mapped / (vec3<f32>(1.0) - mapped), p[4].w);
    }
    return c;
}

@fragment fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let src = textureLoad(scene, vec2<i32>(position.xy), 0);
    var scale = p[0].y;
    if (p[0].x > 1.5) {
        scale = auto_exposure[0].x;
    }
    var c = src.rgb * scale;
    if (p[0].z > 0.5) {
        let dims = vec2<f32>(textureDimensions(scene));
        let glow = textureSampleLevel(bloom, samp, position.xy / dims, 0.0).rgb;
        c = c + glow * p[0].w * p[1].xyz;
    }
    if (p[1].w > 0.5) {
        c = grade(c);
    }
    return vec4<f32>(c, src.a);
}
