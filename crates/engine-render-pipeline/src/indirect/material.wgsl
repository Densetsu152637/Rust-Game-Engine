#import engine::material_bindings

struct EngineIndirectMaterial {
    diffuse0: array<vec4<f32>, 9>,
    diffuse1: array<vec4<f32>, 9>,
    environment_from_world0: mat4x4<f32>,
    environment_from_world1: mat4x4<f32>,
    weights_lods: vec4<f32>, // weights 0/1, maximum mips 0/1
    scales: vec4<f32>, // intensity 0/1, environment present 0/1
}
@group(1) @binding(4) var engine_environment0: texture_cube<f32>;
@group(1) @binding(5) var engine_environment1: texture_cube<f32>;
@group(1) @binding(6) var engine_brdf_lut: texture_2d<f32>;
@group(1) @binding(7) var engine_environment_sampler: sampler;
@group(2) @binding(1) var<uniform> engine_indirect_material: EngineIndirectMaterial;

fn engine_diffuse_polynomial(c: array<vec4<f32>, 9>, n: vec3<f32>) -> vec3<f32> {
    return max(c[0].rgb + c[1].rgb*n.x + c[2].rgb*n.y + c[3].rgb*n.z + c[4].rgb*n.x*n.y
        + c[5].rgb*n.y*n.z + c[6].rgb*(3.0*n.z*n.z-1.0) + c[7].rgb*n.x*n.z + c[8].rgb*(n.x*n.x-n.y*n.y), vec3<f32>(0.0));
}

// Materials opt in by calling this helper. Legacy engine_indirect_lighting
// remains ambient/screen-only, and the sprite implementation remains neutral.
fn engine_material_indirect_lighting(surface: EngineSurface, v: vec3<f32>, frag: vec2<f32>) -> vec3<f32> {
    let p = engine_indirect_material;
    let n0 = (p.environment_from_world0 * vec4<f32>(surface.normal, 0.0)).xyz;
    let n1 = (p.environment_from_world1 * vec4<f32>(surface.normal, 0.0)).xyz;
    let nv = clamp(dot(surface.normal, v), 0.0, 1.0);
    let f0 = mix(vec3<f32>(0.04), surface.albedo, surface.metallic);
    let fresnel = f0 + (max(vec3<f32>(1.0-surface.roughness), f0)-f0)*pow(1.0-nv,5.0);
    let incident0 = mix(p.diffuse0[0].rgb,engine_diffuse_polynomial(p.diffuse0,n0),p.scales.z)*p.scales.x;
    let incident1 = engine_diffuse_polynomial(p.diffuse1,n1)*p.scales.y;
    // Missing global data preserves legacy ambient, including its weighting.
    let diffuse = surface.albedo*(1.0-surface.metallic) * (
        incident0*p.weights_lods.x*mix(vec3<f32>(1.0),vec3<f32>(1.0)-fresnel,p.scales.z)
        + incident1*p.weights_lods.y*(vec3<f32>(1.0)-fresnel));
    let reflected = reflect(-v,surface.normal);
    let r0 = (p.environment_from_world0*vec4<f32>(reflected,0.0)).xyz;
    let r1 = (p.environment_from_world1*vec4<f32>(reflected,0.0)).xyz;
    let radiance = textureSampleLevel(engine_environment0,engine_environment_sampler,r0,surface.roughness*p.weights_lods.z).rgb*p.scales.x*p.weights_lods.x
        + textureSampleLevel(engine_environment1,engine_environment_sampler,r1,surface.roughness*p.weights_lods.w).rgb*p.scales.y*p.weights_lods.y;
    let brdf = textureSampleLevel(engine_brdf_lut,engine_environment_sampler,vec2<f32>(nv,surface.roughness),0.0).rg;
    let specular = radiance*(f0*brdf.x+vec3<f32>(brdf.y));
    return engine_resolve_indirect_lighting(surface,v,frag,diffuse,specular);
}
