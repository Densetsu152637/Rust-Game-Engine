// Depth-free diagnostic of the compacted camera draws: every surviving
// instance covers its true screen footprint, colours it with the low 24 bits
// of its draw-id hash and adds one count to alpha, so a culled instance leaves
// no pixels and overlap is visible whatever the draw order.
#import engine::mesh

struct Output {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) tag: u32,
}

@vertex fn vs_main(vertex: EngineVertex, @builtin(instance_index) instance: u32) -> Output {
    var out: Output;
    let world = engine_world_from_local(instance) * vec4<f32>(vertex.position, 1.0);
    out.position = engine_view.clip_from_world * world;
    out.tag = engine_instances[instance].params.x;
    return out;
}

@fragment fn fs_main(input: Output) -> @location(0) vec4<f32> {
    return vec4<f32>(
        f32(input.tag & 255u),
        f32((input.tag >> 8u) & 255u),
        f32((input.tag >> 16u) & 255u),
        1.0,
    ) / 255.0;
}
