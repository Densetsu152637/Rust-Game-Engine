// physics.fluid: sphere impostors. Each particle is a camera-facing quad; the fragment shader
// reconstructs the sphere (normal and depth) so spheres intersect scene geometry and each
// other correctly, and shades it with the view's light list.
#import engine::view
#import engine::lighting

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) corner: vec2<f32>,
    @location(1) center: vec4<f32>,
    @location(2) color: vec4<f32>,
}

struct FragmentOut {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
}

@vertex fn vs(
    @builtin(vertex_index) index: u32,
    @location(0) center: vec4<f32>,
    @location(1) color: vec4<f32>,
) -> Out {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0),
    );
    let corner = corners[index];
    let right = engine_view.world_from_view[0].xyz;
    let up = engine_view.world_from_view[1].xyz;
    let world = center.xyz + (right * corner.x + up * corner.y) * center.w;
    var out: Out;
    out.position = engine_clip_from_world() * vec4<f32>(world, 1.0);
    out.corner = corner;
    out.center = center;
    out.color = color;
    return out;
}

@fragment fn fs(in: Out) -> FragmentOut {
    let r2 = dot(in.corner, in.corner);
    if r2 > 1.0 {
        discard;
    }
    let right = engine_view.world_from_view[0].xyz;
    let up = engine_view.world_from_view[1].xyz;
    let toward_camera = normalize(engine_camera_position() - in.center.xyz);
    let n = normalize(right * in.corner.x + up * in.corner.y + toward_camera * sqrt(1.0 - r2));
    let world = in.center.xyz + n * in.center.w;
    let clip = engine_clip_from_world() * vec4<f32>(world, 1.0);
    let v = normalize(engine_camera_position() - world);
    var surface: EngineSurface;
    surface.albedo = in.color.rgb;
    surface.metallic = 0.0;
    surface.roughness = 0.35;
    surface.normal = n;
    let direct = engine_direct_lighting(surface, world, v, in.position.xy, true);
    let ambient = engine_indirect_lighting(surface, v, in.position.xy);
    var out: FragmentOut;
    out.color = vec4<f32>(direct + ambient, in.color.a);
    out.depth = clip.z / clip.w;
    return out;
}
