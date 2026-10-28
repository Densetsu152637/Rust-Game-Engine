//! The compiled form of a `script_graph`: an immutable [`Program`] of VM ops.
use crate::{
    codes,
    host::FnId,
    trap::ScriptError,
    value::{Value, ValueType},
};
use std::{collections::BTreeMap, sync::Arc};

/// A register index within one frame.
pub(crate) type Reg = u16;

/// Most ops one program may hold.
pub const MAX_OPS: usize = 65_536;
/// Most registers one frame may use.
pub const MAX_REGS: usize = 4096;

/// Binary and ternary-free value operations; operand types are fixed by the
/// compiler, the VM matches on the runtime value variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Concat,
    V3Add,
    V3Sub,
    V3Dot,
    V3Scale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnOp {
    Abs,
    Neg,
    ToFloat,
    Not,
    V3Length,
}

/// One VM instruction. Jump targets are op indices after compilation.
#[derive(Clone, Debug)]
pub(crate) enum Op {
    Const {
        dst: Reg,
        k: u32,
    },
    Move {
        dst: Reg,
        src: Reg,
    },
    GetVar {
        dst: Reg,
        var: u16,
    },
    SetVar {
        var: u16,
        src: Reg,
    },
    SelfEntity {
        dst: Reg,
    },
    Bin {
        dst: Reg,
        op: BinOp,
        a: Reg,
        b: Reg,
    },
    Un {
        dst: Reg,
        op: UnOp,
        a: Reg,
    },
    Vec3Make {
        dst: Reg,
        x: Reg,
        y: Reg,
        z: Reg,
    },
    /// Writes x, y, z to `dst`, `dst + 1`, `dst + 2`.
    Vec3Break {
        dst: Reg,
        src: Reg,
    },
    ToJson {
        dst: Reg,
        src: Reg,
    },
    JsonTo {
        dst: Reg,
        src: Reg,
        ty: ValueType,
    },
    Jump(u32),
    JumpIfNot {
        cond: Reg,
        to: u32,
    },
    Inc {
        reg: Reg,
    },
    /// Traps when the loop counter `reg` exceeds `max`.
    LoopLimit {
        reg: Reg,
        max: u32,
    },
    /// Jumps to `to` when the once-flag is already set, else sets it.
    DoOnce {
        flag: u16,
        to: u32,
    },
    /// `dst = open`, or `true` once a latched gate has opened.
    Gate {
        flag: u16,
        latch: bool,
        open: Reg,
        dst: Reg,
    },
    Host {
        function: FnId,
        args: Box<[Reg]>,
        dst: Option<Reg>,
        cost: u32,
    },
    CallFn {
        func: u32,
        args: Box<[Reg]>,
        rets: Box<[Reg]>,
    },
    Return {
        srcs: Box<[Reg]>,
    },
    End,
    Emit {
        event: Arc<str>,
        payload: u32,
        target: Option<Reg>,
    },
    Log {
        src: Reg,
    },
    Wait {
        secs: Reg,
    },
}

/// What starts an entry region.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryKey {
    Start,
    Update,
    Event(String),
    Stop,
}

/// Compiled entry: where it starts and how big its frame is.
#[derive(Clone, Debug)]
pub(crate) struct Region {
    pub pc: usize,
    pub n_regs: usize,
    pub dt: Option<Reg>,
    pub payload: Option<Reg>,
    pub source: Option<Reg>,
}

/// A compiled function.
#[derive(Clone, Debug)]
pub(crate) struct FuncInfo {
    pub pc: usize,
    pub n_regs: usize,
    pub params: Vec<(Reg, ValueType)>,
    pub results: Vec<ValueType>,
}

#[derive(Clone, Debug)]
pub(crate) struct VarDecl {
    pub name: String,
    pub ty: ValueType,
    pub default: Value,
}

/// An immutable, shareable compiled script graph.
#[derive(Debug)]
pub struct Program {
    pub(crate) code: Vec<Op>,
    pub(crate) sites: Vec<u32>,
    pub(crate) node_names: Vec<String>,
    pub(crate) consts: Vec<Value>,
    pub(crate) vars: Vec<VarDecl>,
    pub(crate) entries: BTreeMap<EntryKey, Vec<Region>>,
    pub(crate) functions: Vec<FuncInfo>,
    pub(crate) flags: usize,
    pub(crate) subscriptions: Vec<String>,
    pub(crate) host_functions: Vec<String>,
    pub(crate) resolved: bool,
}

impl Program {
    /// Event names the script handles (`on_event` nodes), sorted and unique.
    pub fn subscriptions(&self) -> &[String] {
        &self.subscriptions
    }
    /// Host function names the script calls, sorted and unique.
    pub fn host_functions(&self) -> &[String] {
        &self.host_functions
    }
    /// Names of the declared variables, in declaration order.
    pub fn variable_names(&self) -> impl Iterator<Item = &str> {
        self.vars.iter().map(|v| v.name.as_str())
    }
    /// Number of VM ops (a size measure for tests and telemetry).
    pub fn op_count(&self) -> usize {
        self.code.len()
    }
    /// Whether host calls were resolved against signatures (a program compiled for
    /// validation only has none and cannot be instantiated).
    pub fn is_runnable(&self) -> bool {
        self.resolved
    }

    pub(crate) fn site_name(&self, pc: usize) -> Option<&str> {
        self.sites
            .get(pc)
            .and_then(|i| self.node_names.get(*i as usize))
            .map(String::as_str)
    }

    /// Creates a fresh instance. `overrides` replace variable defaults by name.
    ///
    /// # Errors
    /// `script.variable.invalid` for an unknown variable or a mistyped value, or
    /// when the program was compiled without signatures.
    pub fn instantiate(
        self: &Arc<Self>,
        overrides: &BTreeMap<String, serde_json::Value>,
    ) -> Result<super::vm::GraphInstance, ScriptError> {
        if !self.resolved {
            return Err(ScriptError::new(
                codes::FUNCTION_UNREGISTERED,
                "the program was compiled for validation only",
            ));
        }
        let mut values: Vec<Value> = self.vars.iter().map(|v| v.default.clone()).collect();
        for (name, raw) in overrides {
            let Some(index) = self.vars.iter().position(|v| &v.name == name) else {
                return Err(ScriptError::new(
                    codes::VARIABLE_INVALID,
                    format!("the graph has no variable {name:?}"),
                ));
            };
            let ty = self.vars[index].ty;
            values[index] = Value::from_json(ty, raw).ok_or_else(|| {
                ScriptError::new(
                    codes::VARIABLE_INVALID,
                    format!("variable {name:?} expects a {}", ty.name()),
                )
            })?;
        }
        Ok(super::vm::GraphInstance::new(Arc::clone(self), values))
    }
}
