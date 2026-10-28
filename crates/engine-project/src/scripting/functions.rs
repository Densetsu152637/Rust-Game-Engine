//! Host function declarations: the `scriptFunctions` project file, the built-in
//! `engine.*` signatures and registration of game implementations.
use super::runtime::ScriptState;
use crate::{Error, Project, Result};
use bevy_ecs::prelude::*;
use engine_core::EngineApp;
use engine_script::{
    Effects, FnParam, FnSig, NativeFn, ScriptError, SigTable, ValueType, codes, valid_function_name,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Largest accepted `scriptFunctions` file.
pub const MAX_FUNCTIONS_BYTES: u64 = 1024 * 1024;
/// Most functions a project may declare.
pub const MAX_FUNCTIONS: usize = 256;

fn one() -> u32 {
    1
}

/// One declared host function (an entry of the `scriptFunctions` array).
///
/// Signatures are data so tools validate graphs without running game code.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptFunctionDecl {
    /// Dotted lower-case name such as `game.damage`; `engine.*` is reserved.
    pub name: String,
    #[serde(default)]
    pub params: Vec<FnParam>,
    #[serde(default)]
    pub returns: Option<ValueType>,
    #[serde(default)]
    pub pure: bool,
    /// Fuel one call costs (at least 1).
    #[serde(default = "one")]
    pub cost: u32,
    #[serde(default)]
    pub effects: Effects,
}

impl ScriptFunctionDecl {
    /// The signature this declaration describes.
    pub fn sig(&self) -> FnSig {
        FnSig {
            params: self.params.clone(),
            returns: self.returns,
            pure: self.pure,
            cost: self.cost,
            effects: self.effects,
        }
    }
}

/// Parses and validates the bytes of a `scriptFunctions` file.
///
/// # Errors
/// `script.function.invalid` or `script.function.duplicate`.
pub fn parse_declarations(bytes: &[u8]) -> Result<Vec<ScriptFunctionDecl>> {
    let invalid = |message: String| Error::new(codes::FUNCTION_INVALID, message);
    let declarations: Vec<ScriptFunctionDecl> = serde_json::from_slice(bytes)
        .map_err(|e| invalid(format!("scriptFunctions is not a valid array: {e}")))?;
    if declarations.len() > MAX_FUNCTIONS {
        return Err(invalid(format!(
            "more than {MAX_FUNCTIONS} script functions"
        )));
    }
    let mut seen = BTreeSet::new();
    for declaration in &declarations {
        if declaration
            .name
            .starts_with(engine_script::host::RESERVED_PREFIX)
        {
            return Err(invalid(format!(
                "{}: the `engine.` namespace is reserved",
                declaration.name
            )));
        }
        if !valid_function_name(&declaration.name) {
            return Err(invalid(format!(
                "{:?} is not a dotted lower-case function name",
                declaration.name
            )));
        }
        declaration
            .sig()
            .validate()
            .map_err(|reason| invalid(format!("{}: {reason}", declaration.name)))?;
        if !seen.insert(declaration.name.as_str()) {
            return Err(Error::new(
                codes::FUNCTION_DUPLICATE,
                format!("{} is declared twice", declaration.name),
            ));
        }
    }
    Ok(declarations)
}

/// Names and signatures of the engine's built-in host functions.
pub fn builtin_signatures() -> Vec<(&'static str, FnSig)> {
    use ValueType::{Entity, Float, Int, Json, Str, Vec3};
    vec![
        ("engine.self", FnSig::new(&[], Some(Entity)).pure()),
        (
            "engine.find_entity",
            FnSig::new(&[("id", Str)], Some(Entity)).effects(Effects::READ_WORLD),
        ),
        (
            "engine.position",
            FnSig::new(&[("entity", Entity)], Some(Vec3)).effects(Effects::READ_WORLD),
        ),
        (
            "engine.set_position",
            FnSig::new(&[("entity", Entity), ("position", Vec3)], None)
                .effects(Effects::WRITE_SELF),
        ),
        (
            "engine.component_get",
            FnSig::new(&[("name", Str), ("field", Str)], Some(Json)).effects(Effects::READ_WORLD),
        ),
        (
            "engine.component_set",
            FnSig::new(&[("name", Str), ("field", Str), ("value", Json)], None)
                .effects(Effects::WRITE_SELF),
        ),
        (
            "engine.emit",
            FnSig::new(
                &[("name", Str), ("payload", Json), ("target", Entity)],
                None,
            )
            .effects(Effects::EMIT),
        ),
        ("engine.log", FnSig::new(&[("message", Str)], None)),
        (
            "engine.random_int",
            FnSig::new(&[("min", Int), ("max", Int)], Some(Int)),
        ),
        ("engine.random_float", FnSig::new(&[], Some(Float))),
        ("engine.time", FnSig::new(&[], Some(Float))),
    ]
}

/// Every signature a graph of `project` may call: the built-ins plus the project's
/// declarations. Never runs game code.
pub fn signature_table(project: &Project) -> SigTable {
    let mut table = SigTable::new();
    for (name, sig) in builtin_signatures() {
        table
            .insert(name, sig)
            .expect("built-in signatures are valid");
    }
    for declaration in &project.script_functions {
        // Declarations were validated when the project loaded.
        let _ = table.insert(&declaration.name, declaration.sig());
    }
    table
}

/// Registers the native implementation of a declared game function. Call from the
/// game's `configure` before the runtime starts.
///
/// `Runtime::start` fails with `script.function.unregistered` for a declared
/// function without an implementation and with `script.function.signature` when
/// the registered signature differs from the declaration.
///
/// # Errors
/// `script.function.invalid` (bad name, signature or reserved namespace),
/// `script.function.duplicate`, or an `app` that is not a project runtime.
pub fn register_script_fn(
    app: &mut EngineApp,
    name: &str,
    sig: FnSig,
    function: NativeFn<World>,
) -> Result<()> {
    let mut state = app
        .world_mut()
        .get_resource_mut::<ScriptState>()
        .ok_or_else(|| {
            Error::new(
                "script.unavailable",
                "register_script_fn needs a project runtime",
            )
        })?;
    state
        .registry
        .register(name, sig, function)
        .map(|_| ())
        .map_err(|e: ScriptError| Error::new(e.code, e.message))
}
