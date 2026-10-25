// Cloth, soft-body and fragment surfaces: world-space EngineVertex data (position, colour,
// normal) rewritten every frame, two-sided, lit by the view's light list or normal-shaded.
#import engine::view
#import engine::lighting

struct Vertex {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) normal: vec3<f32>,
}

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) normal: vec3<f32>,
}

// x: perceptual roughness. Pipeline-creation constant substitution keeps the layout to the
// single view group, so the value is baked into the source.
const ROUGHNESS: f32 = __ROUGHNESS__;

@vertex fn vs(vertex: Vertex) -> Out {
    var out: Out;
    out.world = vertex.position;
    out.position = engine_clip_from_world() * vec4<f32>(vertex.position, 1.0);
    out.color = vertex.color;
    out.normal = vertex.normal;
    return out;
}

fn surface_normal(in: Out, front: bool) -> vec3<f32> {
    // Derivatives stay in uniform control flow: always computed, then selected.
    let derivative = cross(dpdx(in.world), dpdy(in.world));
    let n = derivative / max(length(derivative), 0.000001);
    let v = normalize(engine_camera_position() - in.world);
    let derived = select(-n, n, dot(n, v) >= 0.0);
    let authored = normalize(in.normal) * select(-1.0, 1.0, front);
    return select(derived, authored, length(in.normal) > 0.000001);
}

@fragment fn fs_lit(in: Out, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let n = surface_normal(in, front);
    let v = normalize(engine_camera_position() - in.world);
    var surface: EngineSurface;
    surface.albedo = in.color.rgb;
    surface.metallic = 0.0;
    surface.roughness = ROUGHNESS;
    surface.normal = n;
    let direct = engine_direct_lighting(surface, in.world, v, in.position.xy, true);
    let ambient = engine_indirect_lighting(surface, v, in.position.xy);
    return vec4<f32>(direct + ambient, in.color.a);
}

@fragment fn fs_normal(in: Out, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let n = surface_normal(in, front);
    let hemisphere = 0.5 + 0.5 * dot(n, normalize(vec3<f32>(0.3, 1.0, 0.2)));
    return vec4<f32>(in.color.rgb * (0.3 + 0.7 * hemisphere), in.color.a);
}
