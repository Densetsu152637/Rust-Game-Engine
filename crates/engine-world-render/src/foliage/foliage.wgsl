// world.foliage: instanced meshes and camera-facing impostors with wind sway. The sway
// function repeats engine_foliage::WindParams::sway_offset exactly (same terms, same
// constants), and the bend factor repeats engine_foliage::bend_factor.
#import engine::lighting

struct TypeData {
    // Linear RGBA multiplier of the albedo.
    tint: vec4<f32>,
    // x: mesh base height (local y), y: 1 / mesh height extent, z: wind weight.
    wind: vec4<f32>,
    // xy: impostor width and height in metres.
    impostor: vec4<f32>,
}

struct WindData {
    // x, y: direction (x, z); z: strength; w: sway frequency.
    a: vec4<f32>,
    // x: gust frequency; y: gust strength; z: flutter; w: 1 / wavelength.
    b: vec4<f32>,
}

@group(1) @binding(0) var<uniform> ty: TypeData;
@group(1) @binding(1) var foliage_albedo: texture_2d<f32>;
@group(1) @binding(2) var foliage_sampler: sampler;
@group(2) @binding(0) var<uniform> wind: WindData;
@group(3) @binding(0) var<uniform> shadow_index: vec4<u32>;

const TAU: f32 = 6.283185307179586;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
}

fn foliage_rotate(q: vec4<f32>, v: vec3<f32>) -> vec3<f32> {
    let t = 2.0 * cross(q.xyz, v);
    return v + q.w * t + cross(q.xyz, t);
}

// Horizontal sway of a vertex; `p` is the instance's world position, `bend` already includes
// the type's wind weight.
fn foliage_wind_offset(time: f32, p: vec3<f32>, phase: f32, bend: f32) -> vec3<f32> {
    let dir = wind.a.xy;
    let perp = vec2<f32>(-dir.y, dir.x);
    let along = dot(dir, p.xz) * wind.b.w;
    let gust = sin(TAU * (wind.b.x * time - 0.25 * along));
    let amp = wind.a.z * (1.0 + wind.b.y * gust);
    let w1 = sin(TAU * (wind.a.w * time + phase + along));
    let w2 = sin(TAU * (2.0 * wind.a.w * time + 2.0 * phase + along));
    let lean = bend * amp * (0.6 + 0.4 * w1);
    let side = bend * amp * 0.5 * wind.b.z * w2;
    return vec3<f32>(lean * dir.x + side * perp.x, 0.0, lean * dir.y + side * perp.y);
}

fn foliage_bend(height_fraction: f32) -> f32 {
    let h = clamp(height_fraction, 0.0, 1.0);
    return h * h * ty.wind.z;
}

// Mesh instances. Vertex slot 0: the engine vertex. Slot 1 (per instance): rotation
// quaternion (x y z w), position (xyz) and wind phase (w), scale (xyz).
fn mesh_output(
    position: vec3<f32>, uv: vec2<f32>, color: vec4<f32>, normal: vec3<f32>,
    rotation: vec4<f32>, position_phase: vec4<f32>, scale: vec4<f32>,
) -> VsOut {
    let origin = position_phase.xyz;
    var world = origin + foliage_rotate(rotation, position * scale.xyz);
    let bend = foliage_bend((position.y - ty.wind.x) * ty.wind.y);
    world += foliage_wind_offset(engine_view.time.x, origin, position_phase.w, bend);
    var out: VsOut;
    out.position = engine_view.clip_from_world * vec4<f32>(world, 1.0);
    out.world = world;
    out.normal = foliage_rotate(rotation, normal);
    out.uv = uv;
    out.color = color;
    return out;
}

// Impostors: a unit quad (x in -1..1, y in 0..1) turned about the vertical axis to face the
// camera, `impostor.xy` metres at scale 1.
fn billboard_output(
    position: vec3<f32>, uv: vec2<f32>, color: vec4<f32>, normal: vec3<f32>,
    rotation: vec4<f32>, position_phase: vec4<f32>, scale: vec4<f32>,
) -> VsOut {
    let origin = position_phase.xyz;
    let to_camera = engine_camera_position() - origin;
    var facing = vec2<f32>(to_camera.x, to_camera.z);
    if dot(facing, facing) < 1e-8 {
        facing = vec2<f32>(0.0, 1.0);
    }
    facing = normalize(facing);
    let right = vec3<f32>(facing.y, 0.0, -facing.x);
    var world = origin
        + right * (position.x * 0.5 * ty.impostor.x * scale.x)
        + vec3<f32>(0.0, position.y * ty.impostor.y * scale.y, 0.0);
    world += foliage_wind_offset(engine_view.time.x, origin, position_phase.w, foliage_bend(position.y));
    var out: VsOut;
    out.position = engine_view.clip_from_world * vec4<f32>(world, 1.0);
    out.world = world;
    // Cards are lit from above like the ground they stand on.
    out.normal = vec3<f32>(0.0, 1.0, 0.0);
    out.uv = vec2<f32>((position.x + 1.0) * 0.5, 1.0 - position.y);
    out.color = vec4<f32>(1.0);
    return out;
}

@vertex fn vs_mesh(
    @location(0) position: vec3<f32>, @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>, @location(3) normal: vec3<f32>,
    @location(4) rotation: vec4<f32>, @location(5) position_phase: vec4<f32>,
    @location(6) scale: vec4<f32>,
) -> VsOut {
    return mesh_output(position, uv, color, normal, rotation, position_phase, scale);
}

@vertex fn vs_billboard(
    @location(0) position: vec3<f32>, @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>, @location(3) normal: vec3<f32>,
    @location(4) rotation: vec4<f32>, @location(5) position_phase: vec4<f32>,
    @location(6) scale: vec4<f32>,
) -> VsOut {
    return billboard_output(position, uv, color, normal, rotation, position_phase, scale);
}

@vertex fn vs_shadow_mesh(
    @location(0) position: vec3<f32>, @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>, @location(3) normal: vec3<f32>,
    @location(4) rotation: vec4<f32>, @location(5) position_phase: vec4<f32>,
    @location(6) scale: vec4<f32>,
) -> VsOut {
    var out = mesh_output(position, uv, color, normal, rotation, position_phase, scale);
    out.position = engine_shadows[shadow_index.x].clip_from_world * vec4<f32>(out.world, 1.0);
    return out;
}

@vertex fn vs_shadow_billboard(
    @location(0) position: vec3<f32>, @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>, @location(3) normal: vec3<f32>,
    @location(4) rotation: vec4<f32>, @location(5) position_phase: vec4<f32>,
    @location(6) scale: vec4<f32>,
) -> VsOut {
    var out = billboard_output(position, uv, color, normal, rotation, position_phase, scale);
    out.position = engine_shadows[shadow_index.x].clip_from_world * vec4<f32>(out.world, 1.0);
    return out;
}

@fragment fn fs_shadow(input: VsOut) {
    if textureSample(foliage_albedo, foliage_sampler, input.uv).a < 0.5 { discard; }
}

@fragment fn fs_main(input: VsOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let texel = textureSample(foliage_albedo, foliage_sampler, input.uv);
    if texel.a < 0.5 {
        discard;
    }
    var surface: EngineSurface;
    surface.albedo = texel.rgb * input.color.rgb * ty.tint.rgb;
    surface.metallic = 0.0;
    surface.roughness = 0.8;
    var n = normalize(input.normal);
    if !front {
        n = -n;
    }
    surface.normal = n;
    let v = normalize(engine_camera_position() - input.world);
    let direct = engine_direct_lighting(surface, input.world, v, input.position.xy, true);
    let ambient = engine_indirect_lighting(surface, v, input.position.xy);
    return vec4<f32>(direct + ambient, 1.0);
}
