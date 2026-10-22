use anyhow::{Result, ensure};

/// Parse and validate a standalone native WGSL shader without opening a device.
/// All native capabilities are admitted here; device feature/limit requirements
/// are checked separately when the actual pipeline is created. Diagnostics retain
/// Naga source spans. Sources must contain an entry point and fit within 16 MiB.
pub fn validate_wgsl(source: &str) -> Result<()> {
    ensure!(
        source.len() <= 16 * 1024 * 1024,
        "WGSL source exceeds 16 MiB"
    );
    let module = naga::front::wgsl::parse_str(source)
        .map_err(|error| anyhow::anyhow!("WGSL parse failed:\n{}", error.emit_to_string(source)))?;
    ensure!(
        !module.entry_points.is_empty(),
        "WGSL shader has no entry point"
    );
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .map_err(|error| {
        anyhow::anyhow!("WGSL validation failed:\n{}", error.emit_to_string(source))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_real_shader_and_reports_parse_and_semantic_errors_without_gpu() {
        validate_wgsl(include_str!("textured.wgsl")).unwrap();
        assert!(
            validate_wgsl("@vertex fn broken(")
                .unwrap_err()
                .to_string()
                .contains("WGSL parse failed")
        );
        assert!(
            validate_wgsl("@vertex fn main() -> @builtin(position) f32 { return 1.0; }")
                .unwrap_err()
                .to_string()
                .contains("WGSL validation failed")
        );
        assert!(validate_wgsl("").is_err());
    }
}

#[cfg(test)]
mod wgsl_contract_tests {
    use super::*;

    const VERTEX_ONLY: &str =
        "@vertex fn vs() -> @builtin(position) vec4<f32> { return vec4<f32>(0.0, 0.0, 0.0, 1.0); }";

    macro_rules! accepts {
        ($($name:ident: $source:expr;)*) => {$(
            #[test]
            fn $name() {
                validate_wgsl($source).unwrap();
            }
        )*};
    }

    accepts! {
        accepts_minimal_vertex_shader: VERTEX_ONLY;
        accepts_fragment_only_shader: "@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }";
        accepts_compute_shader_with_workgroup_size: "@compute @workgroup_size(8, 8, 1) fn main() {}";
        accepts_compute_shader_with_storage_buffer: "@group(0) @binding(0) var<storage, read_write> data: array<u32>;\n@compute @workgroup_size(64) fn main(@builtin(global_invocation_id) id: vec3<u32>) { data[id.x] = id.x * 2u; }";
        accepts_uniform_buffer_and_texture_bindings: "struct U { m: mat4x4<f32> }\n@group(0) @binding(0) var<uniform> u: U;\n@group(1) @binding(0) var t: texture_2d<f32>;\n@group(1) @binding(1) var s: sampler;\n@fragment fn fs(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> { return textureSample(t, s, uv) * u.m[0]; }";
        accepts_multiple_entry_points_in_one_module: "@vertex fn vs() -> @builtin(position) vec4<f32> { return vec4<f32>(0.0); }\n@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }";
        accepts_comments_and_unicode_in_comments: "// shader \u{3b1}\u{3b2}\u{3b3}\n/* block */ @vertex fn vs() -> @builtin(position) vec4<f32> { return vec4<f32>(0.0); }";
        accepts_helper_functions_and_constants: "const SCALE: f32 = 2.0;\nfn twice(x: f32) -> f32 { return x * SCALE; }\n@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(twice(0.25)); }";
        accepts_loops_and_branches: "@fragment fn fs() -> @location(0) vec4<f32> { var acc = 0.0; for (var i = 0; i < 4; i++) { if (i == 2) { continue; } acc += 0.25; } return vec4<f32>(acc); }";
    }

    macro_rules! rejects {
        ($($name:ident: $source:expr => $message:expr;)*) => {$(
            #[test]
            fn $name() {
                let error = validate_wgsl($source).unwrap_err().to_string();
                assert!(error.contains($message), "expected {:?} in:\n{error}", $message);
            }
        )*};
    }

    rejects! {
        rejects_empty_source: "" => "no entry point";
        rejects_whitespace_only_source: "  \n\t  \n" => "no entry point";
        rejects_comment_only_source: "// nothing here" => "no entry point";
        rejects_module_without_entry_point: "fn helper() -> f32 { return 1.0; }" => "no entry point";
        rejects_unterminated_parameter_list: "@vertex fn broken(" => "WGSL parse failed";
        rejects_missing_semicolon: "@fragment fn fs() -> @location(0) vec4<f32> { let x = 1.0 return vec4<f32>(x); }" => "WGSL parse failed";
        rejects_unbalanced_braces: "@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(1.0);" => "WGSL parse failed";
        rejects_unknown_type: "@fragment fn fs() -> @location(0) vec4<nope> { return vec4<f32>(1.0); }" => "WGSL parse failed";
        rejects_undefined_identifier: "@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(missing); }" => "missing";
        rejects_unknown_attribute: "@bogus fn f() {}\n@vertex fn vs() -> @builtin(position) vec4<f32> { return vec4<f32>(0.0); }" => "WGSL parse failed";
        rejects_duplicate_function_names: "fn f() {}\nfn f() {}\n@compute @workgroup_size(1) fn main() { f(); }" => "WGSL parse failed";
        rejects_compute_entry_without_workgroup_size: "@compute fn main() {}" => "WGSL parse failed";
        rejects_float_integer_mismatch: "@fragment fn fs() -> @location(0) vec4<f32> { let x: i32 = 1.5; return vec4<f32>(0.0); }" => "WGSL parse failed";
        rejects_vertex_entry_returning_non_position: "@vertex fn main() -> @builtin(position) f32 { return 1.0; }" => "WGSL validation failed";
        rejects_fragment_output_without_binding: "@fragment fn fs() -> vec4<f32> { return vec4<f32>(1.0); }" => "WGSL validation failed";
        rejects_vertex_without_position_output: "@vertex fn vs() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }" => "WGSL validation failed";
        rejects_two_bindings_sharing_a_slot: "@group(0) @binding(0) var<uniform> a: vec4<f32>;\n@group(0) @binding(0) var<uniform> b: vec4<f32>;\n@fragment fn fs() -> @location(0) vec4<f32> { return a + b; }" => "WGSL validation failed";
        rejects_unsized_array_in_uniform_space: "@group(0) @binding(0) var<uniform> a: array<f32>;\n@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(a[0]); }" => "WGSL";
    }

    #[test]
    fn parse_errors_carry_line_and_column_spans() {
        let source = "@vertex\nfn broken() -> @builtin(position) vec4<f32> {\n    return vec4<f32>(oops);\n}\n";
        let error = validate_wgsl(source).unwrap_err().to_string();
        assert!(error.contains("oops"), "{error}");
        assert!(error.contains(":3:"), "line number missing:\n{error}");
    }

    #[test]
    fn validation_errors_name_the_offending_entry_point() {
        let source = "@vertex
fn main() -> @builtin(position) f32 {
    return 1.0;
}
";
        let error = validate_wgsl(source).unwrap_err().to_string();
        assert!(error.starts_with("WGSL validation failed"), "{error}");
        assert!(error.contains("Entry point main"), "{error}");
    }

    #[test]
    fn size_limit_is_sixteen_mebibytes_inclusive() {
        let limit = 16 * 1024 * 1024;
        let at_limit = " ".repeat(limit);
        let error = validate_wgsl(&at_limit).unwrap_err().to_string();
        assert!(
            !error.contains("16 MiB"),
            "limit itself must not trip the size check"
        );
        let over = " ".repeat(limit + 1);
        let error = validate_wgsl(&over).unwrap_err().to_string();
        assert!(error.contains("exceeds 16 MiB"), "{error}");
    }

    #[test]
    fn oversized_source_is_rejected_before_parsing() {
        let mut source = String::from(VERTEX_ONLY);
        source.push_str(&" ".repeat(16 * 1024 * 1024));
        assert!(
            validate_wgsl(&source)
                .unwrap_err()
                .to_string()
                .contains("exceeds 16 MiB")
        );
    }

    #[test]
    fn validation_is_deterministic() {
        let first = validate_wgsl("@vertex fn x(").unwrap_err().to_string();
        let second = validate_wgsl("@vertex fn x(").unwrap_err().to_string();
        assert_eq!(first, second);
    }

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
