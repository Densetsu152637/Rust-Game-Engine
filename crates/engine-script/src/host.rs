//! The host seam: function signatures, the registry of native functions,
//! capabilities and the [`ScriptHost`] a script instance runs against.
use crate::{
    codes,
    trap::{ScriptError, Trap},
    value::{EntityRef, Value, ValueType, validate_value},
};
use engine_graph::valid_name;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{BTreeMap, BTreeSet};

/// Most parameters one host function may take.
pub const MAX_FN_PARAMS: usize = 16;
/// Highest fuel cost one host call may declare.
pub const MAX_FN_COST: u32 = 10_000;
/// Longest dotted function name.
pub const MAX_FN_NAME: usize = 64;

/// What a host function may touch. A script component must grant the matching
/// capability before the host lets a call with these effects run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Effects(u8);

impl Effects {
    pub const NONE: Effects = Effects(0);
    /// Reads world state (components, positions).
    pub const READ_WORLD: Effects = Effects(1);
    /// Writes the calling entity's state.
    pub const WRITE_SELF: Effects = Effects(2);
    /// Emits events.
    pub const EMIT: Effects = Effects(4);
    /// Spawns entities (reserved: no runtime spawn path ships yet).
    pub const SPAWN: Effects = Effects(8);

    const NAMES: [(&'static str, Effects); 4] = [
        ("read_world", Self::READ_WORLD),
        ("write_self", Self::WRITE_SELF),
        ("emit", Self::EMIT),
        ("spawn", Self::SPAWN),
    ];

    pub const fn union(self, other: Effects) -> Effects {
        Effects(self.0 | other.0)
    }
    pub const fn contains(self, other: Effects) -> bool {
        self.0 & other.0 == other.0
    }
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
    /// Effect names in a stable order.
    pub fn names(self) -> Vec<&'static str> {
        Self::NAMES
            .iter()
            .filter(|(_, e)| self.contains(*e))
            .map(|(n, _)| *n)
            .collect()
    }
    pub fn parse(names: &[String]) -> Result<Effects, String> {
        let mut out = Effects::NONE;
        for name in names {
            let (_, effect) = Self::NAMES
                .iter()
                .find(|(n, _)| n == name)
                .ok_or_else(|| format!("unknown effect {name:?}"))?;
            out = out.union(*effect);
        }
        Ok(out)
    }
}

impl Serialize for Effects {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.names().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Effects {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let names = Vec::<String>::deserialize(deserializer)?;
        Effects::parse(&names).map_err(serde::de::Error::custom)
    }
}

/// One named, typed parameter of a host function.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FnParam {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: ValueType,
}

impl FnParam {
    pub fn new(name: &str, ty: ValueType) -> Self {
        Self {
            name: name.to_owned(),
            ty,
        }
    }
}

/// The declared signature of a host function.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FnSig {
    pub params: Vec<FnParam>,
    pub returns: Option<ValueType>,
    /// A pure function has no effects, is deterministic in its arguments and returns
    /// a value; graphs call it as a data node with no flow ports.
    pub pure: bool,
    /// Fuel one call costs; at least 1.
    pub cost: u32,
    pub effects: Effects,
}

impl FnSig {
    /// An impure function with cost 1 and no effects.
    pub fn new(params: &[(&str, ValueType)], returns: Option<ValueType>) -> Self {
        Self {
            params: params.iter().map(|(n, t)| FnParam::new(n, *t)).collect(),
            returns,
            pure: false,
            cost: 1,
            effects: Effects::NONE,
        }
    }
    pub fn pure(mut self) -> Self {
        self.pure = true;
        self
    }
    pub fn cost(mut self, cost: u32) -> Self {
        self.cost = cost;
        self
    }
    pub fn effects(mut self, effects: Effects) -> Self {
        self.effects = effects;
        self
    }

    /// Checks the signature's own consistency.
    ///
    /// # Errors
    /// A human-readable reason.
    pub fn validate(&self) -> Result<(), String> {
        if self.params.len() > MAX_FN_PARAMS {
            return Err(format!("more than {MAX_FN_PARAMS} parameters"));
        }
        let mut seen = BTreeSet::new();
        for param in &self.params {
            if !valid_name(&param.name) || !seen.insert(param.name.as_str()) {
                return Err(format!(
                    "parameter name {:?} is invalid or repeated",
                    param.name
                ));
            }
        }
        if self.cost == 0 || self.cost > MAX_FN_COST {
            return Err(format!("cost must be 1..={MAX_FN_COST}"));
        }
        if self.pure && (!self.effects.is_empty() || self.returns.is_none()) {
            return Err("a pure function has no effects and returns a value".to_owned());
        }
        Ok(())
    }
}

/// Whether `name` is a valid dotted host function name: two to four segments of
/// `[a-z][a-z0-9_]*`, at most [`MAX_FN_NAME`] bytes.
pub fn valid_function_name(name: &str) -> bool {
    let segments: Vec<&str> = name.split('.').collect();
    name.len() <= MAX_FN_NAME
        && (2..=4).contains(&segments.len())
        && segments.iter().all(|s| {
            let mut chars = s.chars();
            chars.next().is_some_and(|c| c.is_ascii_lowercase())
                && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
}

/// Namespace reserved for the engine's built-in host functions.
pub const RESERVED_PREFIX: &str = "engine.";

/// Index of a registered function, valid for the registry that issued it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FnId(pub u32);

/// Whom a script may emit events to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmitScope {
    /// Only events targeted at the script's own entity.
    #[default]
    SelfOnly,
    /// Targeted at any entity, or broadcast.
    Any,
}

/// What a script instance has been granted. Everything is denied by default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// Component names the script may read.
    pub read: BTreeSet<String>,
    /// Component names the script may write (on its own entity).
    pub write: BTreeSet<String>,
    /// Entities the script may spawn (reserved; must stay 0).
    pub spawn: u16,
    pub emit: EmitScope,
}

/// Context handed to a native function.
pub struct NativeCx<'a, W: ?Sized> {
    pub world: &'a mut W,
    /// The entity whose script is calling.
    pub subject: EntityRef,
    pub caps: &'a Capabilities,
}

/// A native host function: trusted Rust that gets `&mut W` and applies its
/// effects immediately. Arguments are already validated against the signature.
pub type NativeFn<W> = fn(&mut NativeCx<'_, W>, &[Value]) -> Result<Value, ScriptError>;

struct Entry<W: ?Sized> {
    name: String,
    sig: FnSig,
    function: NativeFn<W>,
}

/// Resolves host function names to ids and signatures at compile time.
pub trait Signatures {
    fn lookup(&self, name: &str) -> Option<(FnId, &FnSig)>;
}

/// Registered native functions, by dotted name. Ids follow registration order.
pub struct HostRegistry<W: ?Sized> {
    entries: Vec<Entry<W>>,
    by_name: BTreeMap<String, FnId>,
}

impl<W: ?Sized> Default for HostRegistry<W> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            by_name: BTreeMap::new(),
        }
    }
}

impl<W: ?Sized> HostRegistry<W> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a game function. `engine.*` is reserved for the engine.
    ///
    /// # Errors
    /// `script.function.invalid` for a bad name or signature (or a reserved
    /// namespace), `script.function.duplicate` for a repeated name.
    pub fn register(
        &mut self,
        name: &str,
        sig: FnSig,
        function: NativeFn<W>,
    ) -> Result<FnId, ScriptError> {
        if name.starts_with(RESERVED_PREFIX) {
            return Err(ScriptError::new(
                codes::FUNCTION_INVALID,
                format!("{name}: the `engine.` namespace is reserved"),
            ));
        }
        self.insert(name, sig, function)
    }

    /// Registers an engine built-in (the reserved namespace). For engine crates.
    ///
    /// # Errors
    /// As [`register`](Self::register), except that `engine.*` is required.
    pub fn register_builtin(
        &mut self,
        name: &str,
        sig: FnSig,
        function: NativeFn<W>,
    ) -> Result<FnId, ScriptError> {
        if !name.starts_with(RESERVED_PREFIX) {
            return Err(ScriptError::new(
                codes::FUNCTION_INVALID,
                format!("{name}: built-ins live in the `engine.` namespace"),
            ));
        }
        self.insert(name, sig, function)
    }

    fn insert(
        &mut self,
        name: &str,
        sig: FnSig,
        function: NativeFn<W>,
    ) -> Result<FnId, ScriptError> {
        if !valid_function_name(name) {
            return Err(ScriptError::new(
                codes::FUNCTION_INVALID,
                format!("{name:?} is not a dotted lower-case function name"),
            ));
        }
        sig.validate().map_err(|reason| {
            ScriptError::new(codes::FUNCTION_INVALID, format!("{name}: {reason}"))
        })?;
        if self.by_name.contains_key(name) {
            return Err(ScriptError::new(
                codes::FUNCTION_DUPLICATE,
                format!("{name} is already registered"),
            ));
        }
        let id = FnId(self.entries.len() as u32);
        self.entries.push(Entry {
            name: name.to_owned(),
            sig,
            function,
        });
        self.by_name.insert(name.to_owned(), id);
        Ok(id)
    }

    pub fn resolve(&self, name: &str) -> Option<(FnId, &FnSig)> {
        let id = *self.by_name.get(name)?;
        Some((id, &self.entries[id.0 as usize].sig))
    }
    pub fn sig(&self, id: FnId) -> Option<&FnSig> {
        self.entries.get(id.0 as usize).map(|e| &e.sig)
    }
    pub fn name(&self, id: FnId) -> Option<&str> {
        self.entries.get(id.0 as usize).map(|e| e.name.as_str())
    }
    /// Registered names in sorted order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.by_name.keys().map(String::as_str)
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Validates `args` against the signature, then runs the native function and
    /// validates its result. The native function is never invoked when an
    /// argument is invalid.
    ///
    /// # Errors
    /// `script.host.argument` for any argument or result that violates the
    /// signature; whatever the native function returned otherwise.
    pub fn invoke(
        &self,
        id: FnId,
        cx: &mut NativeCx<'_, W>,
        args: &[Value],
    ) -> Result<Value, ScriptError> {
        let entry = self.entries.get(id.0 as usize).ok_or_else(|| {
            ScriptError::new(codes::HOST_ARGUMENT, format!("no host function {}", id.0))
        })?;
        check_args(&entry.name, &entry.sig, args)?;
        let result = (entry.function)(cx, args)?;
        match entry.sig.returns {
            None if matches!(result, Value::Unit) => Ok(result),
            None => Err(ScriptError::new(
                codes::HOST_FAILED,
                format!("{} returned a value but declares none", entry.name),
            )),
            Some(ty) => match validate_value(ty, &result) {
                Ok(()) => Ok(result),
                Err(reason) => Err(ScriptError::new(
                    codes::HOST_FAILED,
                    format!("{} returned an invalid value: {reason}", entry.name),
                )),
            },
        }
    }
}

impl<W: ?Sized> Signatures for HostRegistry<W> {
    fn lookup(&self, name: &str) -> Option<(FnId, &FnSig)> {
        self.resolve(name)
    }
}

/// Checks `args` against `sig`: count, exact types, finite numbers, string and
/// JSON bounds.
///
/// # Errors
/// `script.host.argument`.
pub fn check_args(name: &str, sig: &FnSig, args: &[Value]) -> Result<(), ScriptError> {
    if args.len() != sig.params.len() {
        return Err(ScriptError::new(
            codes::HOST_ARGUMENT,
            format!(
                "{name} takes {} arguments, got {}",
                sig.params.len(),
                args.len()
            ),
        ));
    }
    for (param, value) in sig.params.iter().zip(args) {
        validate_value(param.ty, value).map_err(|reason| {
            ScriptError::new(
                codes::HOST_ARGUMENT,
                format!("{name} argument {}: {reason}", param.name),
            )
        })?;
    }
    Ok(())
}

/// A name-to-signature table with no implementations, for validation tools that
/// must not run game code. Ids follow insertion order.
#[derive(Clone, Debug, Default)]
pub struct SigTable {
    sigs: Vec<(String, FnSig)>,
    by_name: BTreeMap<String, FnId>,
}

impl SigTable {
    pub fn new() -> Self {
        Self::default()
    }
    /// Adds a signature; the first declaration of a name wins and a repeat is an error.
    ///
    /// # Errors
    /// `script.function.invalid` or `script.function.duplicate`.
    pub fn insert(&mut self, name: &str, sig: FnSig) -> Result<FnId, ScriptError> {
        if !valid_function_name(name) {
            return Err(ScriptError::new(
                codes::FUNCTION_INVALID,
                format!("{name:?} is not a dotted lower-case function name"),
            ));
        }
        sig.validate().map_err(|reason| {
            ScriptError::new(codes::FUNCTION_INVALID, format!("{name}: {reason}"))
        })?;
        if self.by_name.contains_key(name) {
            return Err(ScriptError::new(
                codes::FUNCTION_DUPLICATE,
                format!("{name} is declared twice"),
            ));
        }
        let id = FnId(self.sigs.len() as u32);
        self.sigs.push((name.to_owned(), sig));
        self.by_name.insert(name.to_owned(), id);
        Ok(id)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&str, &FnSig)> {
        self.by_name
            .iter()
            .map(|(n, id)| (n.as_str(), &self.sigs[id.0 as usize].1))
    }
    pub fn len(&self) -> usize {
        self.sigs.len()
    }
    pub fn is_empty(&self) -> bool {
        self.sigs.is_empty()
    }
}

impl Signatures for SigTable {
    fn lookup(&self, name: &str) -> Option<(FnId, &FnSig)> {
        let id = *self.by_name.get(name)?;
        Some((id, &self.sigs[id.0 as usize].1))
    }
}

/// What a running script calls into. Implemented by the engine (and by recorders
/// in tests). The host owns staging: world effects of built-ins are only applied
/// when the run returns `Ok`.
pub trait ScriptHost {
    /// Calls a host function. `args` match the signature's types.
    ///
    /// # Errors
    /// A [`Trap`]: the run stops and its staged effects are discarded.
    fn call(&mut self, function: FnId, args: &[Value]) -> Result<Value, Trap>;
    /// Emits a game event, delivered on the next tick.
    ///
    /// # Errors
    /// [`Trap::EventLimit`] or [`Trap::Capability`].
    fn emit(&mut self, name: &str, payload: Value, target: Option<EntityRef>) -> Result<(), Trap>;
    /// Appends to the bounded runtime log (never stdout).
    fn log(&mut self, message: &str);
    /// The entity this instance belongs to.
    fn subject(&self) -> EntityRef;
}

/// Why an instance runs.
#[derive(Clone, Copy, Debug)]
pub enum Trigger<'a> {
    Start,
    Update {
        dt: f32,
    },
    Event {
        name: &'a str,
        payload: &'a serde_json::Value,
        source: Option<EntityRef>,
    },
    Timer {
        id: u32,
    },
    Stop,
}
