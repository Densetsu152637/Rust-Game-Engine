//! WGSL module composition (`#import module::name`) and the material shader
//! contract checks.
//!
//! A line of the form `#import a::b` (optionally ending in `;`) is replaced by
//! the named module's source, each module at most once per composed shader;
//! modules may import others. Engine modules: `engine::view` (group 0 view
//! data, light list, shadows, light clusters), `engine::brdf` (shading
//! math), `engine::lighting` (direct lighting of the light list),
//! `engine::mesh` (engine vertex input, group 3 instance data),
//! `engine::material` (material bindings, groups 1 and 2, plus lighting) and
//! `engine::fullscreen`. Features register their own modules with
//! [`ShaderComposer::register_module`] to keep shared code in separate files.

use anyhow::{Context, Result, bail, ensure};
use std::collections::{BTreeMap, BTreeSet};

pub const VIEW_WGSL: &str = include_str!("wgsl/view.wgsl");
pub const MESH_WGSL: &str = include_str!("wgsl/mesh.wgsl");
pub const BRDF_WGSL: &str = include_str!("wgsl/brdf.wgsl");
pub const LIGHTING_WGSL: &str = include_str!("wgsl/lighting.wgsl");
pub const MATERIAL_WGSL: &str = include_str!("wgsl/material.wgsl");
pub const SPRITE_MATERIAL_WGSL: &str = include_str!("wgsl/material_sprite.wgsl");
pub const FULLSCREEN_WGSL: &str = include_str!("wgsl/fullscreen.wgsl");
pub const TONEMAP_WGSL: &str = include_str!("wgsl/tonemap.wgsl");
/// The built-in metallic-roughness material (selected when a material has no
/// custom shader). Also a reference implementation of the contract.
pub const LIT_WGSL: &str = include_str!("wgsl/lit.wgsl");
pub(crate) const MOTION_WGSL: &str = include_str!("wgsl/motion.wgsl");
pub(crate) const SHADOW_WGSL: &str = include_str!("wgsl/shadow.wgsl");
pub(crate) const NORMALS_WGSL: &str = include_str!("wgsl/normals.wgsl");
pub(crate) const OUTPUT_WGSL: &str = include_str!("wgsl/output.wgsl");
pub(crate) const UPSCALE_WGSL: &str = include_str!("wgsl/upscale.wgsl");
pub(crate) const DEPTH_EXPORT_WGSL: &str = include_str!("wgsl/depth_export.wgsl");
pub(crate) const CLUSTERS_WGSL: &str = include_str!("wgsl/clusters.wgsl");
pub(crate) const POST_MAIN_WGSL: &str = include_str!("wgsl/post_main.wgsl");
pub(crate) const POST_BLOOM_WGSL: &str = include_str!("wgsl/post_bloom.wgsl");
pub(crate) const POST_EXPOSURE_WGSL: &str = include_str!("wgsl/post_exposure.wgsl");
pub(crate) const POST_OUTPUT_WGSL: &str = include_str!("wgsl/post_output.wgsl");
pub(crate) const SSR_WGSL: &str = include_str!("wgsl/ssr.wgsl");

/// Longest composed shader accepted, in bytes.
pub const MAX_COMPOSED_BYTES: usize = 4 * 1024 * 1024;
/// Deepest import nesting accepted.
pub const MAX_IMPORT_DEPTH: usize = 16;

/// Which implementation of `engine::material` a shader is composed against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MaterialTarget {
    /// 3D meshes of the built-in mesh path (view, instances, lights, shadows).
    Mesh,
    /// 2D sprites with a material on the sprite path (unlit, one uniform).
    Sprite,
    /// Opt-in orthographic lit sprites; separate native lighting uniform/layout.
    SpriteLit,
}

/// Named WGSL modules and the `#import` resolver.
#[derive(Clone, Debug)]
pub struct ShaderComposer {
    modules: BTreeMap<String, String>,
}

impl Default for ShaderComposer {
    fn default() -> Self {
        Self::new()
    }
}

impl ShaderComposer {
    /// The engine modules with the 3D material implementation.
    pub fn new() -> Self {
        Self::for_target(MaterialTarget::Mesh)
    }

    /// The engine modules with the `engine::material` of `target`.
    pub fn for_target(target: MaterialTarget) -> Self {
        let mut modules = BTreeMap::new();
        modules.insert("engine::view".to_owned(), VIEW_WGSL.to_owned());
        modules.insert("engine::mesh".to_owned(), MESH_WGSL.to_owned());
        modules.insert("engine::brdf".to_owned(), BRDF_WGSL.to_owned());
        modules.insert("engine::lighting".to_owned(), LIGHTING_WGSL.to_owned());
        modules.insert("engine::fullscreen".to_owned(), FULLSCREEN_WGSL.to_owned());
        modules.insert("engine::tonemap".to_owned(), TONEMAP_WGSL.to_owned());
        let material = match target {
            MaterialTarget::Mesh => MATERIAL_WGSL,
            MaterialTarget::Sprite => SPRITE_MATERIAL_WGSL,
            MaterialTarget::SpriteLit => crate::sprite_lighting::MATERIAL_WGSL,
        };
        modules.insert("engine::material".to_owned(), material.to_owned());
        modules.insert(
            "engine::material_bindings".to_owned(),
            include_str!("wgsl/material_bindings.wgsl").to_owned(),
        );
        Self { modules }
    }

    /// Add a module importable as `name` (`segment::segment`, lowercase
    /// identifiers). `engine::` names are reserved.
    pub fn register_module(&mut self, name: &str, source: impl Into<String>) -> Result<()> {
        validate_module_name(name)?;
        ensure!(
            !name.starts_with("engine::"),
            "WGSL module names starting with engine:: are reserved: {name}"
        );
        ensure!(
            !self.modules.contains_key(name),
            "duplicate WGSL module {name}"
        );
        self.modules.insert(name.to_owned(), source.into());
        Ok(())
    }

    pub fn has_module(&self, name: &str) -> bool {
        self.modules.contains_key(name)
    }

    /// Replace every `#import` of `source` (recursively) with module sources.
    pub fn compose(&self, source: &str) -> Result<String> {
        let mut output = String::with_capacity(source.len() + 4096);
        let mut included = BTreeSet::new();
        let mut stack = Vec::new();
        self.expand(source, "<shader>", &mut output, &mut included, &mut stack)?;
        ensure!(
            output.len() <= MAX_COMPOSED_BYTES,
            "composed WGSL exceeds {MAX_COMPOSED_BYTES} bytes"
        );
        Ok(output)
    }

    fn expand(
        &self,
        source: &str,
        name: &str,
        output: &mut String,
        included: &mut BTreeSet<String>,
        stack: &mut Vec<String>,
    ) -> Result<()> {
        ensure!(
            stack.len() < MAX_IMPORT_DEPTH,
            "WGSL imports nest deeper than {MAX_IMPORT_DEPTH}: {}",
            stack.join(" -> ")
        );
        stack.push(name.to_owned());
        for (line_index, line) in source.lines().enumerate() {
            let Some(import) = parse_import(line) else {
                output.push_str(line);
                output.push('\n');
                continue;
            };
            let import = import.with_context(|| format!("{name}:{}", line_index + 1))?;
            if stack.iter().any(|entry| entry == import) {
                bail!("WGSL import cycle: {} -> {import}", stack.join(" -> "));
            }
            if !included.insert(import.to_owned()) {
                continue;
            }
            let module = self.modules.get(import).with_context(|| {
                format!(
                    "{name}:{}: unknown WGSL module {import} (known: {})",
                    line_index + 1,
                    self.modules.keys().cloned().collect::<Vec<_>>().join(", ")
                )
            })?;
            self.expand(module, import, output, included, stack)?;
        }
        stack.pop();
        Ok(())
    }
}

/// `Some(Ok(name))` for an import line, `Some(Err)` for a malformed one.
fn parse_import(line: &str) -> Option<Result<&str>> {
    let rest = line.trim().strip_prefix("#import")?;
    let name = rest.trim().trim_end_matches(';').trim();
    Some(validate_module_name(name).map(|()| name))
}

fn validate_module_name(name: &str) -> Result<()> {
    let segments: Vec<&str> = name.split("::").collect();
    ensure!(
        segments.len() >= 2
            && segments.iter().all(|s| {
                !s.is_empty()
                    && s.bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            }),
        "WGSL module names are `a::b` lowercase identifier paths: {name:?}"
    );
    Ok(())
}

/// Parse and validate composed WGSL with naga.
pub fn validate_wgsl(source: &str) -> Result<naga::Module> {
    let module = naga::front::wgsl::parse_str(source)
        .map_err(|error| anyhow::anyhow!("invalid WGSL: {}", error.emit_to_string(source)))?;
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .map_err(|error| anyhow::anyhow!("invalid WGSL: {}", error.emit_to_string(source)))?;
    Ok(module)
}

/// Bindings `engine::material` provides for `target`: `(group, binding)`.
pub fn material_bindings(target: MaterialTarget) -> &'static [(u32, u32)] {
    match target {
        MaterialTarget::Mesh => &[
            (0, 0),
            (0, 1),
            (0, 2),
            (0, 3),
            (0, 4),
            (0, 5),
            (0, 6),
            (0, 7),
            (0, 8),
            (0, 9),
            (0, 10),
            (1, 0),
            (1, 1),
            (1, 2),
            (1, 3),
            (2, 0),
            (2, 2),
            (3, 0),
        ],
        MaterialTarget::Sprite => &[(0, 0), (1, 0), (1, 1), (1, 2), (1, 3), (2, 0), (2, 2)],
        MaterialTarget::SpriteLit => &[
            (0, 0),
            (1, 0),
            (1, 1),
            (1, 2),
            (1, 3),
            (2, 0),
            (2, 1),
            (2, 2),
        ],
    }
}

/// Compose and check a material shader against the material contract of
/// `target`: valid WGSL, `vs_main` (vertex) and `fs_main` (fragment) entry
/// points, every resource binding one that `engine::material` provides and
/// declared once. Returns the composed source.
pub fn compose_material(source: &str, target: MaterialTarget) -> Result<String> {
    compose_material_mode(source, target, false)
}

pub(crate) fn compose_mesh_material(source: &str, indirect: bool) -> Result<String> {
    compose_material_mode(source, MaterialTarget::Mesh, indirect)
}

/// Anchors of [`LIT_WGSL`] the lightmapped variant extends; each occurs once.
const LIGHTMAP_ANCHORS: [&str; 4] = [
    "    @location(3) normal: vec3<f32>,
}",
    "@vertex fn vs_main(vertex: EngineVertex, @builtin(instance_index) instance: u32) -> Output {",
    "    out.normal = (engine_normal_from_local(instance) * vec4<f32>(vertex.normal, 0.0)).xyz;",
    "let ambient = engine_material_indirect_lighting(surface, v, input.position.xy);",
];

/// The built-in lit material with a second UV stream (`@location(4)`, vertex
/// slot 1) and the instance's lightmap binding: the vertex stage forwards
/// `uv2 * scale + offset` and the lightmap flags, and the fragment stage calls
/// `engine_material_indirect_lighting_lightmap`. It is derived from
/// [`LIT_WGSL`] so the shading body can never drift from the default material.
/// Only the native indirect composition provides that function.
pub(crate) fn lit_lightmap_source() -> Result<String> {
    // Checkouts with CRLF line endings embed CRLF source text.
    let lit = LIT_WGSL.replace("\r\n", "\n");
    for anchor in LIGHTMAP_ANCHORS {
        ensure!(
            lit.matches(anchor).count() == 1,
            "the built-in lit shader changed; lightmap anchor missing: {anchor}"
        );
    }
    Ok(lit
        .replace(
            LIGHTMAP_ANCHORS[0],
            "    @location(3) normal: vec3<f32>,
    @location(4) lightmap_uv: vec2<f32>,
    @location(5) @interpolate(flat) lightmap_flags: u32,
}",
        )
        .replace(
            LIGHTMAP_ANCHORS[1],
            "@vertex fn vs_main(vertex: EngineVertex, @location(4) uv2: vec2<f32>, @builtin(instance_index) instance: u32) -> Output {",
        )
        .replace(
            LIGHTMAP_ANCHORS[2],
            "    out.normal = (engine_normal_from_local(instance) * vec4<f32>(vertex.normal, 0.0)).xyz;
    out.lightmap_uv = engine_lightmap_uv(instance, uv2);
    out.lightmap_flags = engine_lightmap_flags(instance);",
        )
        .replace(
            LIGHTMAP_ANCHORS[3],
            "let ambient = engine_material_indirect_lighting_lightmap(surface, v, input.position.xy, input.lightmap_uv, input.lightmap_flags);",
        ))
}

/// Opening of the fragment entry point and the lighting tail of [`LIT_WGSL`]
/// that the surface-material variant replaces.
const SURFACE_SIGNATURE: &str = "@fragment fn fs_main(input: Output, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {";
const SURFACE_TAIL_START: &str = "    let direct = engine_direct_lighting(";
const SURFACE_TAIL_END: &str = "    return vec4<f32>(result, color.a);";

/// The built-in lit material with its lighting tail replaced by the
/// surface-material outputs read by screen-space reflections: the shaded
/// (normal-mapped) world normal with perceptual roughness, F0 with coverage
/// (zero for unlit materials) and the specular term the opaque pass adds. It
/// is derived from `lit` (the plain or the lightmapped source) so the normal
/// and material logic can never drift from the shading body.
pub(crate) fn lit_surface_source(lit: &str) -> Result<String> {
    let lit = lit.replace("\r\n", "\n");
    for anchor in [SURFACE_SIGNATURE, SURFACE_TAIL_START, SURFACE_TAIL_END] {
        ensure!(
            lit.matches(anchor).count() == 1,
            "the built-in lit shader changed; surface-material anchor missing: {anchor}"
        );
    }
    let signature = lit.find(SURFACE_SIGNATURE).expect("checked above");
    let start = lit.find(SURFACE_TAIL_START).expect("checked above");
    let end = lit.find(SURFACE_TAIL_END).expect("checked above") + SURFACE_TAIL_END.len();
    ensure!(
        signature < start && start < end,
        "the built-in lit shader changed; surface-material anchors are out of order"
    );
    let mut out = String::with_capacity(lit.len() + 1024);
    out.push_str(&lit[..signature]);
    out.push_str(
        "struct EngineSurfaceMaterialOut {
    // World-space shaded normal (xyz) and perceptual roughness (w).
    @location(0) normal_roughness: vec4<f32>,
    // F0 (rgb) and coverage (a): 0 for unlit materials.
    @location(1) f0_coverage: vec4<f32>,
    // Specular term of the indirect lighting already in the opaque colour.
    @location(2) fallback: vec4<f32>,
}

@fragment fn fs_main(input: Output, @builtin(front_facing) front: bool) -> EngineSurfaceMaterialOut {",
    );
    out.push_str(&lit[signature + SURFACE_SIGNATURE.len()..start]);
    out.push_str(
        "    var out: EngineSurfaceMaterialOut;
    out.normal_roughness = vec4<f32>(n, surface.roughness);
    out.f0_coverage = vec4<f32>(mix(vec3<f32>(0.04), surface.albedo, surface.metallic), select(1.0, 0.0, material.options.x > 0.5));
    out.fallback = vec4<f32>(engine_material_indirect_specular(surface, v), 1.0);
    return out;",
    );
    out.push_str(&lit[end..]);
    Ok(out)
}

fn compose_material_mode(source: &str, target: MaterialTarget, indirect: bool) -> Result<String> {
    let mut composer = ShaderComposer::for_target(target);
    if indirect {
        composer.modules.insert(
            "engine::material".to_owned(),
            crate::indirect::MATERIAL_WGSL.to_owned(),
        );
    }
    let composed = composer.compose(source)?;
    let module = validate_wgsl(&composed)?;
    for (name, stage) in [
        ("vs_main", naga::ShaderStage::Vertex),
        ("fs_main", naga::ShaderStage::Fragment),
    ] {
        ensure!(
            module
                .entry_points
                .iter()
                .any(|e| e.name == name && e.stage == stage),
            "material shaders need a `{name}` {stage:?} entry point"
        );
    }
    let mut allowed = material_bindings(target).to_vec();
    if indirect {
        allowed.extend_from_slice(&[(1, 4), (1, 5), (1, 6), (1, 7), (1, 8), (2, 1)]);
    }
    let mut seen = BTreeSet::new();
    for (_, global) in module.global_variables.iter() {
        let Some(binding) = &global.binding else {
            continue;
        };
        let slot = (binding.group, binding.binding);
        ensure!(
            allowed.contains(&slot),
            "unsupported material binding (group {}, binding {}) `{}`: material shaders use only \
             the bindings `#import engine::material` declares",
            slot.0,
            slot.1,
            global.name.as_deref().unwrap_or("?")
        );
        ensure!(
            seen.insert(slot),
            "material binding (group {}, binding {}) is declared twice; import \
             engine::material instead of declaring engine bindings",
            slot.0,
            slot.1
        );
    }
    Ok(composed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_indirect_and_legacy_material_contracts_compose_without_extra_groups() {
        let native = compose_mesh_material(LIT_WGSL, true).unwrap();
        assert!(native.contains("engine_environment0"));
        assert!(native.contains("engine_resolve_indirect_lighting"));
        let module = validate_wgsl(&native).unwrap();
        assert!(
            module
                .global_variables
                .iter()
                .all(|(_, v)| v.binding.as_ref().is_none_or(|b| b.group <= 3))
        );
        for target in [MaterialTarget::Mesh, MaterialTarget::Sprite] {
            let legacy = compose_material(LIT_WGSL, target).unwrap();
            assert!(!legacy.contains("var engine_environment0"));
            let custom = LIT_WGSL.replace(
                "engine_material_indirect_lighting",
                "engine_indirect_lighting",
            );
            compose_material(&custom, target).unwrap();
        }
    }

    #[test]
    fn lightmapped_lit_variant_composes_only_against_native_indirect() {
        let source = lit_lightmap_source().unwrap();
        let composed = compose_mesh_material(&source, true).unwrap();
        let module = validate_wgsl(&composed).unwrap();
        let vertex = module
            .entry_points
            .iter()
            .find(|e| e.stage == naga::ShaderStage::Vertex)
            .unwrap();
        let locations: Vec<u32> = vertex
            .function
            .arguments
            .iter()
            .flat_map(|argument| match &module.types[argument.ty].inner {
                naga::TypeInner::Struct { members, .. } => members
                    .iter()
                    .filter_map(|m| match m.binding {
                        Some(naga::Binding::Location { location, .. }) => Some(location),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                _ => match argument.binding {
                    Some(naga::Binding::Location { location, .. }) => vec![location],
                    _ => Vec::new(),
                },
            })
            .collect();
        assert_eq!(
            locations,
            [0, 1, 2, 3, 4],
            "vertex inputs: engine vertex + uv2"
        );
        // The legacy material has no lightmap-aware lighting function.
        assert!(compose_mesh_material(&source, false).is_err());
        // The default lit shader keeps the four-attribute vertex ABI.
        let plain = compose_mesh_material(LIT_WGSL, true).unwrap();
        assert!(!plain.contains("@location(4) uv2"));
    }

    #[test]
    fn surface_material_variant_composes_against_both_material_paths_and_lightmaps() {
        let plain = lit_surface_source(LIT_WGSL).unwrap();
        let lightmapped = lit_surface_source(&lit_lightmap_source().unwrap()).unwrap();
        for (source, indirect) in [(&plain, false), (&plain, true), (&lightmapped, true)] {
            let composed = compose_mesh_material(source, indirect).unwrap();
            let module = validate_wgsl(&composed).unwrap();
            let fragment = module
                .entry_points
                .iter()
                .find(|e| e.stage == naga::ShaderStage::Fragment && e.name == "fs_main")
                .unwrap();
            let result = fragment.function.result.as_ref().unwrap();
            let naga::TypeInner::Struct { members, .. } = &module.types[result.ty].inner else {
                panic!("three surface outputs");
            };
            assert_eq!(members.len(), 3);
        }
        // The shading body is the lit material's: normal-map logic stays shared.
        assert!(plain.contains("let sampled_normal = textureSample(normal_map"));
        assert!(!plain.contains("engine_direct_lighting("));
        assert!(plain.contains("engine_material_indirect_specular(surface, v)"));
        assert!(!plain.contains("return vec4<f32>(result, color.a);"));
        assert!(lit_surface_source("fn fs_main() {}").is_err());
    }

    #[test]
    fn lightmap_variant_changes_only_the_extension_points_of_the_lit_shader() {
        let source = lit_lightmap_source().unwrap();
        assert!(source.contains("engine_material_indirect_lighting_lightmap"));
        // Everything outside the four anchors is byte-for-byte the default material.
        let mut stripped = source.clone();
        for removed in [
            "    @location(4) lightmap_uv: vec2<f32>,
    @location(5) @interpolate(flat) lightmap_flags: u32,
",
            "
    out.lightmap_uv = engine_lightmap_uv(instance, uv2);
    out.lightmap_flags = engine_lightmap_flags(instance);",
            "@location(4) uv2: vec2<f32>, ",
        ] {
            stripped = stripped.replace(removed, "");
        }
        let stripped = stripped.replace(
            "engine_material_indirect_lighting_lightmap(surface, v, input.position.xy, input.lightmap_uv, input.lightmap_flags)",
            "engine_material_indirect_lighting(surface, v, input.position.xy)",
        );
        assert_eq!(stripped, LIT_WGSL.replace("\r\n", "\n"));
    }

    #[test]
    fn imports_resolve_recursively_once_each() {
        let mut composer = ShaderComposer::new();
        composer
            .register_module(
                "game::common",
                "#import engine::view\nconst GAME_SCALE: f32 = 2.0;",
            )
            .unwrap();
        let composed = composer
            .compose(
                "#import engine::view\n#import game::common;\nfn f() -> f32 { return GAME_SCALE; }",
            )
            .unwrap();
        assert_eq!(composed.matches("struct EngineView").count(), 1);
        assert!(composed.contains("const GAME_SCALE"));
        validate_wgsl(&composed).unwrap();
    }

    #[test]
    fn unknown_malformed_reserved_and_cyclic_imports_fail() {
        let mut composer = ShaderComposer::new();
        let error = composer.compose("#import nope::missing").unwrap_err();
        assert!(format!("{error:#}").contains("unknown WGSL module nope::missing"));
        assert!(composer.compose("#import Bad Name").is_err());
        assert!(composer.register_module("engine::mine", "").is_err());
        assert!(composer.register_module("single", "").is_err());
        composer.register_module("a::x", "#import a::y").unwrap();
        composer.register_module("a::y", "#import a::x").unwrap();
        let error = composer.compose("#import a::x").unwrap_err();
        assert!(format!("{error:#}").contains("cycle"), "{error:#}");
        assert!(composer.register_module("a::x", "").is_err(), "duplicate");
    }

    #[test]
    fn engine_modules_and_builtin_shaders_are_valid() {
        let composer = ShaderComposer::new();
        for source in [
            LIT_WGSL,
            MOTION_WGSL,
            SHADOW_WGSL,
            NORMALS_WGSL,
            OUTPUT_WGSL,
            UPSCALE_WGSL,
            DEPTH_EXPORT_WGSL,
            CLUSTERS_WGSL,
            POST_MAIN_WGSL,
            POST_BLOOM_WGSL,
            POST_EXPOSURE_WGSL,
            POST_OUTPUT_WGSL,
            SSR_WGSL,
        ] {
            validate_wgsl(&composer.compose(source).unwrap()).unwrap();
        }
    }

    #[test]
    fn the_lit_material_satisfies_both_material_targets() {
        compose_material(LIT_WGSL, MaterialTarget::Mesh).unwrap();
        compose_material(LIT_WGSL, MaterialTarget::Sprite).unwrap();
    }

    #[test]
    fn material_contract_violations_are_reported() {
        let extra = format!("{LIT_WGSL}\n@group(3) @binding(1) var<uniform> extra: vec4<f32>;");
        let error = compose_material(&extra, MaterialTarget::Mesh).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported material binding (group 3, binding 1)"),
            "{error}"
        );
        let renamed = LIT_WGSL.replace("fn fs_main", "fn fs_other");
        assert!(
            compose_material(&renamed, MaterialTarget::Mesh)
                .unwrap_err()
                .to_string()
                .contains("fs_main")
        );
        assert!(compose_material("not wgsl", MaterialTarget::Sprite).is_err());
        // A legacy shader that declares the engine bindings itself.
        let legacy =
            "#import engine::material\n@group(2) @binding(0) var<uniform> mine: vec4<f32>;";
        assert!(compose_material(legacy, MaterialTarget::Sprite).is_err());
    }
}
