// Built-in shadow depth pass for one shadow view (atlas layer).
#import engine::mesh

struct ShadowPass { index: vec4<u32> }
@group(1) @binding(0) var<uniform> shadow_pass: ShadowPass;

@vertex fn vs_main(vertex: EngineVertex, @builtin(instance_index) instance: u32) -> @builtin(position) vec4<f32> {
    let world = engine_world_from_local(instance) * vec4<f32>(vertex.position, 1.0);
    return engine_shadows[shadow_pass.index.x].clip_from_world * world;
}
