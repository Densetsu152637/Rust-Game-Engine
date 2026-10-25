// physics.debug_lines: world-space segments as screen-space quads of constant pixel width.
#import engine::view

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex fn vs(
    @builtin(vertex_index) index: u32,
    @location(0) a: vec4<f32>,
    @location(1) b: vec4<f32>,
    @location(2) color: vec4<f32>,
) -> Out {
    var ends = array<f32, 6>(0.0, 0.0, 1.0, 0.0, 1.0, 1.0);
    var sides = array<f32, 6>(-1.0, 1.0, 1.0, -1.0, 1.0, -1.0);
    var out: Out;
    out.color = color;
    out.position = vec4<f32>(2.0, 2.0, 2.0, 1.0);
    var ca = engine_view.clip_from_world * vec4<f32>(a.xyz, 1.0);
    var cb = engine_view.clip_from_world * vec4<f32>(b.xyz, 1.0);
    // Clip the segment to the near plane (WebGPU clip z >= 0) so lines crossing the camera
    // plane stay finite.
    if ca.z < 0.0 && cb.z < 0.0 {
        return out;
    }
    if ca.z < 0.0 {
        ca = mix(ca, cb, ca.z / (ca.z - cb.z));
    } else if cb.z < 0.0 {
        cb = mix(cb, ca, cb.z / (cb.z - ca.z));
    }
    let size = engine_view.render_size.xy;
    let sa = ca.xy / ca.w * 0.5 * size;
    let sb = cb.xy / cb.w * 0.5 * size;
    var direction = sb - sa;
    if length(direction) < 1e-4 {
        direction = vec2<f32>(1.0, 0.0);
    }
    direction = normalize(direction);
    let normal = vec2<f32>(-direction.y, direction.x);
    let clip = select(ca, cb, ends[index] > 0.5);
    let offset_px = normal * sides[index] * a.w * 0.5;
    let ndc = clip.xy / clip.w + offset_px * 2.0 / size;
    out.position = vec4<f32>(ndc * clip.w, clip.z, clip.w);
    return out;
}

@fragment fn fs(in: Out) -> @location(0) vec4<f32> {
    return in.color;
}
