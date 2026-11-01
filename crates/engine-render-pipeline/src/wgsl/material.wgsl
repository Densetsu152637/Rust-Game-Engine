#import engine::material_bindings
fn engine_material_indirect_lighting(surface: EngineSurface, v: vec3<f32>, frag: vec2<f32>) -> vec3<f32> {
    return engine_indirect_lighting(surface, v, frag);
}

// The specular term engine_material_indirect_lighting adds to a surface.
// The legacy ambient path has none; the surface-material pass (engine.ssr)
// writes it so screen-space reflections can replace it.
fn engine_material_indirect_specular(surface: EngineSurface, v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(0.0);
}
