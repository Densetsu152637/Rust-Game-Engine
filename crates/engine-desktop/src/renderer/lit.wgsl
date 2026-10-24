struct Camera { mvp: mat4x4<f32> }
struct Object { model: mat4x4<f32> }
struct Material { tint: vec4<f32>, options: vec4<f32> }
@group(0) @binding(0) var<uniform> camera: Camera;
@group(0) @binding(1) var<uniform> object: Object;
@group(1) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;
@group(2) @binding(0) var<uniform> material: Material;
struct Output {
    @builtin(position) position: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
}
@vertex fn vs_main(@location(0) position: vec3<f32>, @location(1) uv: vec2<f32>, @location(2) color: vec4<f32>) -> Output {
    var out: Output;
    out.position = camera.mvp * vec4<f32>(position, 1.0);
    out.world = (object.model * vec4<f32>(position, 1.0)).xyz;
    out.uv = uv;
    out.color = color;
    return out;
}
@fragment fn fs_main(input: Output, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let derivative_normal = cross(dpdx(input.world), dpdy(input.world));
    let normal = derivative_normal / max(length(derivative_normal), 0.000001);
    // Triangle normals derive from transformed geometry, including nonuniform scale.
    let lighting = 0.2 + 0.8 * abs(dot(normal, normalize(vec3<f32>(0.4, 0.7, 1.0))));
    let intensity = select(lighting, 1.0, material.options.x > 0.5);
    let color = textureSample(image, image_sampler, input.uv) * input.color * material.tint;
    return vec4<f32>(color.rgb * intensity, color.a);
}
