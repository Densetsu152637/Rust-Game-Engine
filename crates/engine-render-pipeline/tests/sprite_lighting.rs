use engine_render::shader::BindingKind;
use engine_render::{ShaderInterface, ShaderResource, ShaderSource, UniformSlot};
use engine_render_pipeline::sprite_lighting::*;
use engine_render_pipeline::{
    LIT_WGSL, Light, LightShape, MaterialParams, MaterialTarget, compose_material,
};
use glam::Vec3;

fn named(id: &str) -> SpriteLight {
    SpriteLight::new(id.into(), Light::directional(Vec3::NEG_Z, Vec3::ONE, 1.0)).unwrap()
}
#[test]
fn light_boundary_rejects_unsupported_and_nonfinite_inputs() {
    let light = Light::point(Vec3::Z, Vec3::ONE, 1.0, 10.0);
    assert!(SpriteLight::new(String::new(), light).is_err());
    assert!(SpriteLight::new("x".repeat(65), light).is_err());
    for intensity in [f32::NAN, f32::INFINITY, -1.0, 1_000_001.0] {
        assert!(SpriteLight::new("bad".into(), Light { intensity, ..light }).is_err());
    }
    for position in [
        Vec3::splat(f32::NAN),
        Vec3::splat(f32::INFINITY),
        Vec3::splat(1_000_001.0),
    ] {
        assert!(SpriteLight::new("bad".into(), Light { position, ..light }).is_err());
    }
    for color in [Vec3::NEG_ONE, Vec3::splat(1.01), Vec3::splat(f32::NAN)] {
        assert!(SpriteLight::new("bad".into(), Light { color, ..light }).is_err());
    }
    for range in [0.0, -1.0, f32::NAN, f32::INFINITY, 1_000_001.0] {
        assert!(SpriteLight::new("bad".into(), Light { range, ..light }).is_err());
    }
    assert!(
        SpriteLight::new("bad".into(), Light::directional(Vec3::ZERO, Vec3::ONE, 1.0)).is_err()
    );
    assert!(SpriteLight::new("bad".into(), light.with_shadow(Default::default())).is_err());
    assert!(
        SpriteLight::new(
            "bad".into(),
            Light {
                shape: LightShape::Spot {
                    inner_angle: 0.1,
                    outer_angle: 0.2
                },
                ..light
            }
        )
        .is_err()
    );
    let directional = Light::directional(Vec3::NEG_Z, Vec3::ONE, 1.0);
    assert!(
        SpriteLight::new(
            "bad".into(),
            Light {
                range: f32::NAN,
                ..directional
            }
        )
        .is_err()
    );
}

#[test]
fn stable_lights_do_not_reorder_draw_offsets_and_budgets_fail_atomically() {
    let lighting = SpriteLighting::new(Vec3::splat(0.1), vec![named("z"), named("a")]).unwrap();
    assert_eq!(
        lighting
            .lights()
            .iter()
            .map(SpriteLight::id)
            .collect::<Vec<_>>(),
        ["a", "z"]
    );
    let offsets = [Vec3::X, Vec3::NEG_X, Vec3::ZERO];
    let uniforms = lighting
        .prepare_draws(&wgpu::Limits::default(), &offsets)
        .unwrap();
    assert_eq!(
        uniforms
            .iter()
            .map(SpriteLightingUniform::offset)
            .collect::<Vec<_>>(),
        offsets
    );
    assert!(uniforms.iter().all(|u| u.light_count() == 2));
    assert_eq!(SPRITE_LIGHTING_UNIFORM_BYTES, 5168);
    let at_limit = lighting
        .prepare_draws(
            &wgpu::Limits::default(),
            &vec![Vec3::ZERO; MAX_LIT_SPRITE_DRAWS],
        )
        .unwrap();
    assert_eq!(at_limit.len(), MAX_LIT_SPRITE_DRAWS);
    let all_lights = SpriteLighting::new(
        Vec3::ZERO,
        (0..MAX_SPRITE_LIGHTS)
            .map(|i| named(&format!("{i}")))
            .collect(),
    )
    .unwrap();
    assert_eq!(
        all_lights
            .prepare_draws(&wgpu::Limits::default(), &[Vec3::ZERO])
            .unwrap()[0]
            .light_count() as usize,
        MAX_SPRITE_LIGHTS
    );
    assert!(SpriteLighting::new(Vec3::ZERO, vec![named("a"), named("a")]).is_err());
    let many = (0..MAX_SPRITE_LIGHTS + 1)
        .map(|i| named(&format!("{i}")))
        .collect();
    assert!(SpriteLighting::new(Vec3::ZERO, many).is_err());
    for ambient in [
        Vec3::NEG_ONE,
        Vec3::splat(f32::NAN),
        Vec3::splat(1_000_001.0),
    ] {
        assert!(SpriteLighting::new(ambient, vec![]).is_err());
    }
    assert!(
        lighting
            .prepare_draws(
                &wgpu::Limits::default(),
                &vec![Vec3::ZERO; MAX_LIT_SPRITE_DRAWS + 1]
            )
            .is_err()
    );
    assert!(
        lighting
            .prepare_draws(
                &wgpu::Limits::default(),
                &[Vec3::ZERO, Vec3::splat(f32::NAN)]
            )
            .is_err()
    );
    assert_eq!(
        lighting
            .prepare_draws(&wgpu::Limits::default(), &[])
            .unwrap()
            .len(),
        0
    );
    // A failed preparation never mutates the immutable accepted light snapshot.
    assert_eq!(
        lighting
            .prepare_draws(&wgpu::Limits::default(), &offsets)
            .unwrap()[0]
            .light_count(),
        2
    );
}

#[test]
fn sprite_lit_rejects_device_uniform_and_dynamic_binding_shortfalls() {
    check_limits(&wgpu::Limits::default()).unwrap();
    let small = wgpu::Limits {
        max_uniform_buffer_binding_size: 5167,
        ..Default::default()
    };
    assert!(check_limits(&small).is_err());
    let dynamic = wgpu::Limits {
        max_dynamic_uniform_buffers_per_pipeline_layout: 3,
        ..Default::default()
    };
    assert!(check_limits(&dynamic).is_err());
    let stages = wgpu::Limits {
        max_uniform_buffers_per_shader_stage: 2,
        ..Default::default()
    };
    assert!(check_limits(&stages).is_err());
    let lighting = SpriteLighting::new(Vec3::ZERO, vec![]).unwrap();
    let buffer = wgpu::Limits {
        max_buffer_size: 8192,
        ..Default::default()
    };
    assert!(lighting.prepare_draws(&buffer, &[Vec3::ZERO; 2]).is_err());
    let offsets = wgpu::Limits {
        min_uniform_buffer_offset_alignment: 1 << 31,
        max_buffer_size: u64::MAX,
        ..Default::default()
    };
    assert!(lighting.prepare_draws(&offsets, &[Vec3::ZERO; 2]).is_err());
}

#[test]
fn lit_material_reflects_exact_optional_binding_and_legacy_stays_unchanged() {
    let composed = compose_material(SPRITE_LIT_WGSL, MaterialTarget::SpriteLit).unwrap();
    let interface = ShaderInterface::reflect(&ShaderResource::new(
        "SpriteLit",
        ShaderSource::wgsl(composed),
    ))
    .unwrap();
    let binding = interface
        .bindings
        .iter()
        .find(|b| b.slot == UniformSlot::new(2, 1))
        .unwrap();
    assert_eq!(binding.kind, BindingKind::UniformBuffer { size: 5168 });
    assert!(interface.bindings.iter().all(|b| b.slot.group <= 2));
    let restricted = engine_render::DeviceLimits {
        max_uniform_buffer_binding_size: 5167,
        ..Default::default()
    };
    assert!(
        interface.check_limits(&restricted).is_err(),
        "neutral device must reject oversized lighting before shader creation"
    );
    for target in [MaterialTarget::Mesh, MaterialTarget::Sprite] {
        let source = compose_material(LIT_WGSL, target).unwrap();
        let module = engine_render_pipeline::validate_wgsl(&source).unwrap();
        assert!(!module.global_variables.iter().any(|(_, value)| {
            value
                .binding
                .as_ref()
                .is_some_and(|binding| binding.group == 2 && binding.binding == 1)
        }));
    }
    assert!(compose_material(SPRITE_LIT_WGSL, MaterialTarget::Sprite).is_err());
}

#[test]
fn sprite_surface_uses_existing_map_flags_and_material_validation() {
    let material = SpriteMaterialUniforms::new(
        MaterialParams::default(),
        SpriteTextureMaps {
            normal: true,
            metallic_roughness: true,
        },
    )
    .unwrap();
    assert_eq!(material.maps(), &[1.0, 1.0, 1.0, 0.0]);
    assert_eq!(
        material.material()[1][3],
        0.0,
        "SpriteLit never requests shadows"
    );
    let bad = MaterialParams {
        roughness: f32::NAN,
        ..Default::default()
    };
    assert!(SpriteMaterialUniforms::new(bad, SpriteTextureMaps::default()).is_err());
}
