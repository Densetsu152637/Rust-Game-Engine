//! WGSL validation (owned by `engine-render`) and checks of the shipped shader.

/// Parse and validate a standalone native WGSL shader without opening a device.
/// Implemented by `engine_render::validate_wgsl`; its error type is
/// [`engine_render::RenderError`], which implements `std::error::Error` and
/// displays the shader diagnostics verbatim.
pub use engine_render::validate_wgsl;

#[cfg(test)]
mod textured_shader_tests {
    use super::*;

    #[test]
    fn shipped_shader_is_valid_and_has_the_expected_entry_points() {
        let source = include_str!("textured.wgsl");
        validate_wgsl(source).unwrap();
        let module = naga::front::wgsl::parse_str(source).unwrap();
        let mut names: Vec<_> = module
            .entry_points
            .iter()
            .map(|e| (e.name.as_str(), e.stage))
            .collect();
        names.sort_by_key(|(name, _)| *name);
        assert_eq!(
            names,
            [
                ("fs_main", naga::ShaderStage::Fragment),
                ("vs_main", naga::ShaderStage::Vertex)
            ]
        );
    }

    #[test]
    fn re_exported_validation_reports_errors_as_displayable_std_errors() {
        let error = validate_wgsl("@vertex fn broken(").unwrap_err();
        let boxed: Box<dyn std::error::Error> = Box::new(error);
        assert!(boxed.to_string().contains("WGSL parse failed"), "{boxed}");
        assert!(validate_wgsl("").is_err());
    }

    #[test]
    fn shipped_shader_resource_bindings_match_the_renderer_layouts() {
        let module = naga::front::wgsl::parse_str(include_str!("textured.wgsl")).unwrap();
        let mut bindings: Vec<_> = module
            .global_variables
            .iter()
            .filter_map(|(_, v)| v.binding.map(|b| (b.group, b.binding, v.space)))
            .collect();
        bindings.sort_by_key(|(group, binding, _)| (*group, *binding));
        assert_eq!(bindings.len(), 3);
        assert_eq!((bindings[0].0, bindings[0].1), (0, 0));
        assert!(matches!(bindings[0].2, naga::AddressSpace::Uniform));
        assert_eq!((bindings[1].0, bindings[1].1), (1, 0));
        assert_eq!((bindings[2].0, bindings[2].1), (1, 1));
        assert!(matches!(bindings[1].2, naga::AddressSpace::Handle));
        assert!(matches!(bindings[2].2, naga::AddressSpace::Handle));
    }

    #[test]
    fn shipped_shader_bindings_follow_the_engine_render_conventions() {
        use engine_render::{
            SAMPLER_SLOT, ShaderInterface, ShaderResource, ShaderSource, TEXTURE_SLOT, UniformSlot,
        };
        let interface = ShaderInterface::reflect(&ShaderResource::new(
            "textured",
            ShaderSource::wgsl(include_str!("textured.wgsl")),
        ))
        .unwrap();
        let uniforms: Vec<_> = interface
            .uniform_bindings()
            .map(|(binding, size)| (binding.slot, size))
            .collect();
        assert_eq!(uniforms, [(UniformSlot::new(0, 0), 64)]);
        assert!(interface.uses_texture() && interface.uses_sampler());
        let slots: Vec<_> = interface.bindings.iter().map(|b| b.slot).collect();
        assert!(slots.contains(&TEXTURE_SLOT) && slots.contains(&SAMPLER_SLOT));
    }

    #[test]
    fn shipped_shader_camera_uniform_is_one_4x4_float_matrix() {
        let module = naga::front::wgsl::parse_str(include_str!("textured.wgsl")).unwrap();
        let (_, camera) = module
            .global_variables
            .iter()
            .find(|(_, v)| matches!(v.space, naga::AddressSpace::Uniform))
            .unwrap();
        let naga::TypeInner::Struct { members, span } = &module.types[camera.ty].inner else {
            panic!("camera uniform must be a struct");
        };
        assert_eq!(members.len(), 1);
        assert_eq!(*span as usize, std::mem::size_of::<[f32; 16]>());
        assert!(matches!(
            module.types[members[0].ty].inner,
            naga::TypeInner::Matrix {
                columns: naga::VectorSize::Quad,
                rows: naga::VectorSize::Quad,
                ..
            }
        ));
    }

    #[test]
    fn shipped_shader_fragment_writes_a_single_vec4_colour_target() {
        let module = naga::front::wgsl::parse_str(include_str!("textured.wgsl")).unwrap();
        let fragment = module
            .entry_points
            .iter()
            .find(|e| e.stage == naga::ShaderStage::Fragment)
            .unwrap();
        let result = fragment.function.result.as_ref().unwrap();
        assert!(matches!(
            result.binding,
            Some(naga::Binding::Location { location: 0, .. })
        ));
        assert!(matches!(
            module.types[result.ty].inner,
            naga::TypeInner::Vector {
                size: naga::VectorSize::Quad,
                ..
            }
        ));
    }
}
