#import engine::material_bindings
fn engine_material_indirect_lighting(surface: EngineSurface, v: vec3<f32>, frag: vec2<f32>) -> vec3<f32> {
    return engine_indirect_lighting(surface, v, frag);
}
