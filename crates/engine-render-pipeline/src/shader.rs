//! WGSL module composition (`#import module::name`) and the material shader
//! contract checks.
//!
//! A line of the form `#import a::b` (optionally ending in `;`) is replaced by
//! the named module's source, each module at most once per composed shader;
//! modules may import others. Engine modules: `engine::view` (group 0 view
//! data, light list, shadows), `engine::mesh` (engine vertex input, group 3
//! instance data), `engine::material` (material bindings, groups 1 and 2) and
//! `engine::fullscreen`. Features register their own modules with
//! [`ShaderComposer::register_module`] to keep shared code in separate files.

use anyhow::{Context, Result, bail, ensure};
use std::collections::{BTreeMap, BTreeSet};

pub const VIEW_WGSL: &str = include_str!("wgsl/view.wgsl");
pub const MESH_WGSL: &str = include_str!("wgsl/mesh.wgsl");
pub const MATERIAL_WGSL: &str = include_str!("wgsl/material.wgsl");
pub const SPRITE_MATERIAL_WGSL: &str = include_str!("wgsl/material_sprite.wgsl");
pub const FULLSCREEN_WGSL: &str = include_str!("wgsl/fullscreen.wgsl");
/// The built-in metallic-roughness material (selected when a material has no
/// custom shader). Also a reference implementation of the contract.
pub const LIT_WGSL: &str = include_str!("wgsl/lit.wgsl");
pub(crate) const MOTION_WGSL: &str = include_str!("wgsl/motion.wgsl");
pub(crate) const SHADOW_WGSL: &str = include_str!("wgsl/shadow.wgsl");
pub(crate) const NORMALS_WGSL: &str = include_str!("wgsl/normals.wgsl");
pub(crate) const OUTPUT_WGSL: &str = include_str!("wgsl/output.wgsl");
pub(crate) const UPSCALE_WGSL: &str = include_str!("wgsl/upscale.wgsl");
pub(crate) const DEPTH_EXPORT_WGSL: &str = include_str!("wgsl/depth_export.wgsl");

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
        modules.insert("engine::fullscreen".to_owned(), FULLSCREEN_WGSL.to_owned());
        let material = match target {
            MaterialTarget::Mesh => MATERIAL_WGSL,
            MaterialTarget::Sprite => SPRITE_MATERIAL_WGSL,
        };
        modules.insert("engine::material".to_owned(), material.to_owned());
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
            (1, 0),
            (1, 1),
            (1, 2),
            (1, 3),
            (2, 0),
            (2, 2),
            (3, 0),
        ],
        MaterialTarget::Sprite => &[(0, 0), (1, 0), (1, 1), (1, 2), (1, 3), (2, 0), (2, 2)],
    }
}

/// Compose and check a material shader against the material contract of
/// `target`: valid WGSL, `vs_main` (vertex) and `fs_main` (fragment) entry
/// points, every resource binding one that `engine::material` provides and
/// declared once. Returns the composed source.
pub fn compose_material(source: &str, target: MaterialTarget) -> Result<String> {
    let composer = ShaderComposer::for_target(target);
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
    let allowed = material_bindings(target);
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
