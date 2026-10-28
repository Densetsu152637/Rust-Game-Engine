// Certified built-in opaque geometry into isolated current-frame camera depth.
#import engine::mesh

@vertex fn vs_main(vertex: EngineVertex, @builtin(instance_index) instance: u32)
    -> @builtin(position) vec4<f32> {
    let world = engine_world_from_local(instance) * vec4<f32>(vertex.position, 1.0);
    return engine_view.clip_from_world * world;
}
