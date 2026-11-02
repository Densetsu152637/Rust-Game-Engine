// engine.particles billboards: one camera-facing quad per particle slot, drawn
// straight from the simulation buffer. Dead slots collapse outside the clip volume.
#import engine::view

struct Emitter {
    origin: vec4<f32>,
    axis: vec4<f32>,
    tangent: vec4<f32>,
    bitangent: vec4<f32>,
    gravity: vec4<f32>,
    life_dt: vec4<f32>,
    counts: vec4<u32>,
    base: vec4<u32>,
    start_color: vec4<f32>,
    end_color: vec4<f32>,
    // x start size (half width in world units), y end size.
    size: vec4<f32>,
}

struct Particle {
    position: vec3<f32>,
    age: f32,
    velocity: vec3<f32>,
    lifetime: f32,
}

@group(1) @binding(0) var<uniform> emitter: Emitter;
@group(1) @binding(1) var<storage, read> particles: array<Particle>;

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) corner: vec2<f32>,
    @location(1) color: vec4<f32>,
}

@vertex
fn vs(@builtin(vertex_index) index: u32, @builtin(instance_index) instance: u32) -> Out {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0),
    );
    var out: Out;
    let p = particles[instance];
    if p.lifetime <= 0.0 {
        out.position = vec4<f32>(2.0, 2.0, 2.0, 1.0);
        out.corner = vec2<f32>(0.0);
        out.color = vec4<f32>(0.0);
        return out;
    }
    let t = clamp(p.age / p.lifetime, 0.0, 1.0);
    let size = mix(emitter.size.x, emitter.size.y, t);
    let corner = corners[index];
    let right = engine_view.world_from_view[0].xyz;
    let up = engine_view.world_from_view[1].xyz;
    let world = p.position + (right * corner.x + up * corner.y) * size;
    out.position = engine_clip_from_world() * vec4<f32>(world, 1.0);
    out.corner = corner;
    out.color = mix(emitter.start_color, emitter.end_color, t);
    return out;
}

// Premultiplied output: the alpha pipeline blends One / OneMinusSrcAlpha, the
// additive pipeline One / One on colour.
@fragment
fn fs(in: Out) -> @location(0) vec4<f32> {
    if dot(in.corner, in.corner) > 1.0 {
        discard;
    }
    return vec4<f32>(in.color.rgb * in.color.a, in.color.a);
}
