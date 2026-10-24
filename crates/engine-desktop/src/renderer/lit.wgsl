struct Camera { mvp: mat4x4<f32> }
struct Object { model: mat4x4<f32> }
struct Material { tint: vec4<f32>, options: vec4<f32> }
struct Lighting {
    light_vp: mat4x4<f32>,
    eye: vec4<f32>,
    direction: vec4<f32>,
    radiance: vec4<f32>,
    shadow: vec4<f32>,
}
@group(0) @binding(0) var<uniform> camera: Camera;
@group(0) @binding(1) var<uniform> object: Object;
@group(0) @binding(2) var<uniform> normal_matrix: mat4x4<f32>;
@group(1) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;
@group(1) @binding(2) var metallic_roughness_map: texture_2d<f32>;
@group(1) @binding(3) var normal_map: texture_2d<f32>;
@group(2) @binding(0) var<uniform> material: Material;
@group(2) @binding(1) var<uniform> lighting: Lighting;
@group(2) @binding(2) var<uniform> maps: vec4<f32>;
@group(3) @binding(0) var shadow_map: texture_depth_2d;
struct Output {
    @builtin(position) position: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) normal: vec3<f32>,
}
@vertex fn vs_main(@location(0) position: vec3<f32>, @location(1) uv: vec2<f32>, @location(2) color: vec4<f32>, @location(3) normal: vec3<f32>) -> Output {
    var out: Output;
    out.position = camera.mvp * vec4<f32>(position, 1.0);
    out.world = (object.model * vec4<f32>(position, 1.0)).xyz;
    out.uv = uv;
    out.color = color;
    out.normal = (normal_matrix * vec4<f32>(normal, 0.0)).xyz;
    return out;
}
fn visibility(world: vec3<f32>) -> f32 {
    let p = lighting.light_vp * vec4<f32>(world, 1.0);
    let ndc = p.xyz / p.w;
    let uv = ndc.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    // Receiver-plane depth correction keeps neighboring PCF taps from shadowing
    // the same sloped triangle. Compute derivatives before divergent bounds checks.
    let ux = dpdx(uv);
    let uy = dpdy(uv);
    let zx = dpdx(ndc.z);
    let zy = dpdy(ndc.z);
    let det = ux.x * uy.y - ux.y * uy.x;
    let safe_det = select(1.0, det, abs(det) > 0.00000001);
    let gradient = select(vec2<f32>(0.0), vec2<f32>(uy.y * zx - ux.y * zy, ux.x * zy - uy.x * zx) / safe_det, abs(det) > 0.00000001);
    if lighting.shadow.x < 0.5 || material.options.w < 0.5 { return 1.0; }
    if any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) || ndc.z < 0.0 || ndc.z > 1.0 { return 1.0; }
    let size = vec2<i32>(textureDimensions(shadow_map));
    let texel = vec2<i32>(uv * vec2<f32>(size));
    var sum = 0.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let coord = clamp(texel + vec2<i32>(x, y), vec2<i32>(0), size - vec2<i32>(1));
            let tap_uv = (vec2<f32>(coord) + vec2<f32>(0.5)) / vec2<f32>(size);
            let receiver_depth = ndc.z + dot(gradient, tap_uv - uv);
            sum += select(0.0, 1.0, receiver_depth - lighting.shadow.y <= textureLoad(shadow_map, coord, 0));
        }
    }
    return sum / 9.0;
}
@fragment fn fs_main(input: Output, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let color = textureSample(image, image_sampler, input.uv) * input.color * material.tint;
    let derivative_normal = cross(dpdx(input.world), dpdy(input.world));
    var n = derivative_normal / max(length(derivative_normal), 0.000001);
    let v = normalize(lighting.eye.xyz - input.world);
    // Derivative normals face the viewed side; authored normals follow triangle winding.
    n = select(-n, n, dot(n, v) >= 0.0);
    if length(input.normal) > 0.000001 { n = normalize(input.normal) * select(-1.0, 1.0, front); }
    let dx = dpdx(input.world);
    let dy = dpdy(input.world);
    let tx = dpdx(input.uv);
    let ty = dpdy(input.uv);
    let determinant = tx.x * ty.y - tx.y * ty.x;
    let sampled_normal = textureSample(normal_map, image_sampler, input.uv).xyz * 2.0 - vec3<f32>(1.0);
    if maps.x > 0.5 && abs(determinant) > 0.000001 {
        let tangent = (dx * ty.y - dy * tx.y) / determinant;
        let bitangent = (dy * tx.x - dx * ty.x) / determinant;
        let projected = tangent - n * dot(n, tangent);
        if length(projected) > 0.000001 {
            let t = normalize(projected);
            let b = cross(n, t) * select(-1.0, 1.0, dot(cross(n, t), bitangent) >= 0.0);
            let mapped = mat3x3<f32>(t, b, n) * vec3<f32>(sampled_normal.xy * maps.y, sampled_normal.z);
            if length(mapped) > 0.000001 { n = normalize(mapped); }
        }
    }
    let l = normalize(lighting.direction.xyz);
    let half_vector = l + v;
    let h = half_vector / max(length(half_vector), 0.000001);
    let nl = max(dot(n, l), 0.0);
    let nv = max(dot(n, v), 0.0001);
    let nh = max(dot(n, h), 0.0);
    let vh = max(dot(v, h), 0.0);
    let mr = textureSample(metallic_roughness_map, image_sampler, input.uv);
    let metallic = material.options.y * select(1.0, mr.b, maps.z > 0.5);
    let roughness = max(material.options.z * select(1.0, mr.g, maps.z > 0.5), 0.045);
    let alpha = roughness * roughness;
    let a2 = alpha * alpha;
    let denom = nh * nh * (a2 - 1.0) + 1.0;
    let distribution = a2 / max(3.14159265 * denom * denom, 0.0000001);
    let gv = 2.0 * nv / (nv + sqrt(a2 + (1.0 - a2) * nv * nv));
    let gl = 2.0 * nl / max(nl + sqrt(a2 + (1.0 - a2) * nl * nl), 0.000001);
    let f0 = mix(vec3<f32>(0.04), color.rgb, metallic);
    let fresnel = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - vh, 5.0);
    let specular = distribution * gv * gl * fresnel / max(4.0 * nv * nl, 0.000001);
    let diffuse = (vec3<f32>(1.0) - fresnel) * (1.0 - metallic) * color.rgb / 3.14159265;
    let direct = (diffuse + specular) * lighting.radiance.rgb * nl * visibility(input.world);
    let ambient = color.rgb * (1.0 - metallic) * lighting.radiance.w;
    let shaded = direct + ambient;
    let result = select(shaded, color.rgb, material.options.x > 0.5);
    return vec4<f32>(result, color.a);
}
