//! Traps, faults, error sites and execution budgets.
use crate::codes;
use std::fmt;

/// Where in a script a problem happened: a graph node id or a text position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Site {
    Node(String),
    Pos { line: u32, col: u32 },
}

impl fmt::Display for Site {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Node(id) => write!(f, "node:{id}"),
            Self::Pos { line, col } => write!(f, "{line}:{col}"),
        }
    }
}

/// A coded script error with an optional site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptError {
    pub code: &'static str,
    pub message: String,
    pub site: Option<Site>,
}

impl ScriptError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            site: None,
        }
    }
    pub fn at(mut self, site: Site) -> Self {
        self.site = Some(site);
        self
    }
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)?;
        if let Some(site) = &self.site {
            write!(f, " (at {site})")?;
        }
        Ok(())
    }
}

impl std::error::Error for ScriptError {}

/// Why one script run stopped abnormally. A trap discards the run's staged writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trap {
    Fuel,
    Memory,
    CallDepth,
    /// A host function failed or rejected its arguments (`script.host.*`).
    Host(ScriptError),
    /// The script used a capability it was not granted.
    Capability(String),
    NonFinite,
    EventLimit,
}

impl Trap {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Fuel => codes::FUEL_EXHAUSTED,
            Self::Memory => codes::MEMORY_LIMIT,
            Self::CallDepth => codes::CALL_DEPTH,
            Self::Host(error) => error.code,
            Self::Capability(_) => codes::CAPABILITY_DENIED,
            Self::NonFinite => codes::VALUE_NONFINITE,
            Self::EventLimit => codes::EVENT_LIMIT,
        }
    }
    pub fn message(&self) -> String {
        match self {
            Self::Fuel => "the fuel budget is exhausted".to_owned(),
            Self::Memory => "a memory limit was exceeded".to_owned(),
            Self::CallDepth => "the call depth limit was exceeded".to_owned(),
            Self::Host(error) => error.message.clone(),
            Self::Capability(what) => format!("capability denied: {what}"),
            Self::NonFinite => "a non-finite number reached a boundary".to_owned(),
            Self::EventLimit => "the per-tick emit limit was exceeded".to_owned(),
        }
    }
    /// A host error with the given code.
    pub fn host(code: &'static str, message: impl Into<String>) -> Self {
        Self::Host(ScriptError::new(code, message))
    }
}

impl From<ScriptError> for Trap {
    fn from(error: ScriptError) -> Self {
        Self::Host(error)
    }
}

/// A trap plus the site where the instance trapped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fault {
    pub trap: Trap,
    pub site: Option<Site>,
}

impl Fault {
    pub fn code(&self) -> &'static str {
        self.trap.code()
    }
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.trap.code(), self.trap.message())?;
        if let Some(site) = &self.site {
            write!(f, " (at {site})")?;
        }
        Ok(())
    }
}

impl std::error::Error for Fault {}

/// Default fuel per instance and tick.
pub const DEFAULT_FUEL: u64 = 10_000;
/// Authorable fuel range.
pub const FUEL_RANGE: (u64, u64) = (100, 1_000_000);
/// Deepest guest call stack.
pub const MAX_CALL_DEPTH: u16 = 16;
/// Instance state limit.
pub const MAX_STATE_BYTES: usize = 64 * 1024;
/// Pending waits per instance.
pub const MAX_PENDING: u16 = 64;
/// Emits per instance per tick.
pub const MAX_EMITS_PER_TICK: u16 = 16;

/// Per-instance, per-tick execution budget. The caller builds one per instance and
/// tick and passes it to every `handle` call of that tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Budget {
    pub fuel: u64,
    pub max_call_depth: u16,
    pub max_state_bytes: usize,
    /// Most pending latent waits (or timers) an instance may hold.
    pub max_pending: u16,
    /// Emits the instance may still perform this tick.
    pub emits_left: u16,
}

impl Default for Budget {
    fn default() -> Self {
        Self::with_fuel(DEFAULT_FUEL)
    }
}

impl Budget {
    /// The default limits with `fuel` clamped to the authorable range.
    pub fn with_fuel(fuel: u64) -> Self {
        Self {
            fuel: fuel.clamp(FUEL_RANGE.0, FUEL_RANGE.1),
            max_call_depth: MAX_CALL_DEPTH,
            max_state_bytes: MAX_STATE_BYTES,
            max_pending: MAX_PENDING,
            emits_left: MAX_EMITS_PER_TICK,
        }
    }
    /// Spends `amount` fuel.
    ///
    /// # Errors
    /// [`Trap::Fuel`] when less than `amount` remains; the budget is then zero.
    pub fn charge(&mut self, amount: u64) -> Result<(), Trap> {
        match self.fuel.checked_sub(amount) {
            Some(rest) => {
                self.fuel = rest;
                Ok(())
            }
            None => {
                self.fuel = 0;
                Err(Trap::Fuel)
            }
        }
    }
}
