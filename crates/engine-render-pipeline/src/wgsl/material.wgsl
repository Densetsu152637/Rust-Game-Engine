// engine::material - everything a material shader may use. Import it and
// write `vs_main`/`fs_main`; never declare bindings yourself.
#import engine::mesh
#import engine::lighting

struct EngineMaterial {
    tint: vec4<f32>,
    // x unlit, y metallic, z roughness, w receive shadows.
    options: vec4<f32>,
}

@group(1) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;
@group(1) @binding(2) var metallic_roughness_map: texture_2d<f32>;
@group(1) @binding(3) var normal_map: texture_2d<f32>;
@group(2) @binding(0) var<uniform> material: EngineMaterial;
// x normal map enabled, y normal scale, z metallic-roughness map enabled.
@group(2) @binding(2) var<uniform> maps: vec4<f32>;
