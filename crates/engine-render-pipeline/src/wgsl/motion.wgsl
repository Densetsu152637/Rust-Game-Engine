// Built-in motion prepass: rasterize with the jittered matrix, output
// current-to-previous motion in render pixels (y down) from unjittered
// positions. Custom vertex displacement is not replayed.
#import engine::mesh

struct Output {
    @builtin(position) position: vec4<f32>,
    @location(0) current: vec4<f32>,
    @location(1) previous: vec4<f32>,
}

@vertex fn vs_main(vertex: EngineVertex, @builtin(instance_index) instance: u32) -> Output {
    var out: Output;
    let p = vec4<f32>(vertex.position, 1.0);
    let world = engine_world_from_local(instance) * p;
    out.position = engine_view.clip_from_world * world;
    out.current = engine_view.unjittered_clip_from_world * world;
    out.previous = engine_view.previous_clip_from_world * (engine_previous_world_from_local(instance) * p);
    return out;
}

@fragment fn fs_main(input: Output) -> @location(0) vec4<f32> {
    var delta = vec2<f32>(0.0);
    if input.current.w > 0.000001 && input.previous.w > 0.000001 {
        let current = input.current.xy / input.current.w;
        let previous = input.previous.xy / input.previous.w;
        delta = (previous - current) * vec2<f32>(0.5, -0.5) * engine_view.render_size.xy;
    }
    return vec4<f32>(delta, 0.0, 1.0);
}
