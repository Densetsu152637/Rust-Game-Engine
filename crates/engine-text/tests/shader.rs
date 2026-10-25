use engine_text::GLYPH_SHADER_WGSL;

#[test]
fn glyph_shader_parses_and_validates_with_naga() {
    let module = naga::front::wgsl::parse_str(GLYPH_SHADER_WGSL).expect("WGSL parses");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .expect("WGSL validates");
    let stages: Vec<_> = module
        .entry_points
        .iter()
        .map(|e| (e.name.as_str(), e.stage))
        .collect();
    assert!(stages.contains(&("vs_main", naga::ShaderStage::Vertex)));
    assert!(stages.contains(&("fs_main", naga::ShaderStage::Fragment)));
    // Vertex inputs match runtime::Vertex { position: vec3, uv: vec2, color: vec4 }.
    let vs = module
        .entry_points
        .iter()
        .find(|e| e.name == "vs_main")
        .unwrap();
    let naga::TypeInner::Struct { members, .. } = &module.types[vs.function.arguments[0].ty].inner
    else {
        panic!("vertex input is a struct");
    };
    let widths: Vec<_> = members
        .iter()
        .map(|m| match module.types[m.ty].inner {
            naga::TypeInner::Vector { size, .. } => size as u32,
            _ => 0,
        })
        .collect();
    assert_eq!(widths, [3, 2, 4]);
}
