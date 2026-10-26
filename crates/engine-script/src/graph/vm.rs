//! The `script_graph` virtual machine: a non-recursive register VM.
//!
//! Guest calls push explicit [`Frame`]s onto a vector, so the guest call depth is
//! data (limited by the [`Budget`]) and never native stack. Every op costs one
//! fuel (host calls cost their signature's `cost`); a trap discards the run.
use super::program::{BinOp, EntryKey, Op, Program, Reg, UnOp};
use crate::{
    codes,
    host::{ScriptHost, Trigger},
    instance::{InstanceDebug, MAX_DEBUG_VARIABLES, ScriptInstance},
    trap::{Budget, Fault, Site, Trap},
    value::{EntityRef, MAX_STRING_BYTES, Value},
};
use std::{cmp::Ordering, sync::Arc};

/// Largest wait a script may request, in seconds.
pub const MAX_WAIT_SECONDS: f64 = 3600.0;

struct Frame {
    pc: usize,
    regs: Vec<Value>,
    func: Option<u32>,
    /// Caller registers receiving this frame's results.
    rets: Box<[Reg]>,
}

struct Pending {
    remaining: f64,
    pc: usize,
    regs: Vec<Value>,
}

enum Step {
    Next,
    Done,
}

/// One running `script_graph`: variables, once-flags and pending waits.
pub struct GraphInstance {
    program: Arc<Program>,
    vars: Vec<Value>,
    flags: Vec<bool>,
    pending: Vec<Pending>,
    state_bytes: usize,
}

fn pending_size(regs: &[Value]) -> usize {
    64 + regs.iter().map(Value::byte_size).sum::<usize>()
}

fn internal(message: &str) -> Trap {
    Trap::host(codes::HOST_FAILED, format!("internal: {message}"))
}

fn as_int(value: &Value) -> Result<i64, Trap> {
    match value {
        Value::Int(i) => Ok(*i),
        _ => Err(internal("expected an int")),
    }
}
fn as_float(value: &Value) -> Result<f64, Trap> {
    match value {
        Value::Float(f) => Ok(*f),
        _ => Err(internal("expected a float")),
    }
}
fn as_bool(value: &Value) -> Result<bool, Trap> {
    match value {
        Value::Bool(b) => Ok(*b),
        _ => Err(internal("expected a bool")),
    }
}
fn as_vec3(value: &Value) -> Result<[f32; 3], Trap> {
    match value {
        Value::Vec3(v) => Ok(*v),
        _ => Err(internal("expected a vec3")),
    }
}

fn finite(value: f64) -> Result<f64, Trap> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(Trap::NonFinite)
    }
}

fn finite_vec3(v: [f32; 3]) -> Result<Value, Trap> {
    if v.iter().all(|c| c.is_finite()) {
        Ok(Value::Vec3(v))
    } else {
        Err(Trap::NonFinite)
    }
}

fn overflow() -> Trap {
    Trap::host(codes::HOST_FAILED, "integer overflow")
}

fn compare(a: &Value, b: &Value) -> Result<Ordering, Trap> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Ok(x.cmp(y)),
        (Value::Float(x), Value::Float(y)) => {
            x.partial_cmp(y).ok_or_else(|| internal("unordered floats"))
        }
        (Value::Str(x), Value::Str(y)) => Ok(x.as_bytes().cmp(y.as_bytes())),
        _ => Err(internal("values cannot be ordered")),
    }
}

fn binary(op: BinOp, a: &Value, b: &Value) -> Result<Value, Trap> {
    match op {
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Min | BinOp::Max => {
            match (a, b) {
                (Value::Int(x), Value::Int(y)) => {
                    let (x, y) = (*x, *y);
                    Ok(Value::Int(match op {
                        BinOp::Add => x.checked_add(y).ok_or_else(overflow)?,
                        BinOp::Sub => x.checked_sub(y).ok_or_else(overflow)?,
                        BinOp::Mul => x.checked_mul(y).ok_or_else(overflow)?,
                        BinOp::Div => {
                            if y == 0 {
                                return Err(Trap::host(codes::HOST_FAILED, "division by zero"));
                            }
                            x.checked_div(y).ok_or_else(overflow)?
                        }
                        BinOp::Min => x.min(y),
                        _ => x.max(y),
                    }))
                }
                (Value::Float(x), Value::Float(y)) => {
                    let (x, y) = (*x, *y);
                    Ok(Value::Float(finite(match op {
                        BinOp::Add => x + y,
                        BinOp::Sub => x - y,
                        BinOp::Mul => x * y,
                        BinOp::Div => x / y,
                        BinOp::Min => x.min(y),
                        _ => x.max(y),
                    })?))
                }
                _ => Err(internal("arithmetic operand types")),
            }
        }
        BinOp::Eq => Ok(Value::Bool(a == b)),
        BinOp::Ne => Ok(Value::Bool(a != b)),
        BinOp::Lt => Ok(Value::Bool(compare(a, b)? == Ordering::Less)),
        BinOp::Le => Ok(Value::Bool(compare(a, b)? != Ordering::Greater)),
        BinOp::Gt => Ok(Value::Bool(compare(a, b)? == Ordering::Greater)),
        BinOp::Ge => Ok(Value::Bool(compare(a, b)? != Ordering::Less)),
        BinOp::And => Ok(Value::Bool(as_bool(a)? && as_bool(b)?)),
        BinOp::Or => Ok(Value::Bool(as_bool(a)? || as_bool(b)?)),
        BinOp::Concat => match (a, b) {
            (Value::Str(x), Value::Str(y)) => {
                if x.len() + y.len() > MAX_STRING_BYTES {
                    return Err(Trap::Memory);
                }
                let mut text = String::with_capacity(x.len() + y.len());
                text.push_str(x);
                text.push_str(y);
                Ok(Value::str(&text))
            }
            _ => Err(internal("concat operand types")),
        },
        BinOp::V3Add | BinOp::V3Sub => {
            let (x, y) = (as_vec3(a)?, as_vec3(b)?);
            let sign = if op == BinOp::V3Add { 1.0 } else { -1.0 };
            finite_vec3([x[0] + sign * y[0], x[1] + sign * y[1], x[2] + sign * y[2]])
        }
        BinOp::V3Dot => {
            let (x, y) = (as_vec3(a)?, as_vec3(b)?);
            let dot: f64 = (0..3).map(|i| f64::from(x[i]) * f64::from(y[i])).sum();
            Ok(Value::Float(finite(dot)?))
        }
        BinOp::V3Scale => {
            let (x, s) = (as_vec3(a)?, as_float(b)? as f32);
            finite_vec3([x[0] * s, x[1] * s, x[2] * s])
        }
    }
}

fn unary(op: UnOp, a: &Value) -> Result<Value, Trap> {
    match op {
        UnOp::Abs => match a {
            Value::Int(i) => Ok(Value::Int(i.checked_abs().ok_or_else(overflow)?)),
            Value::Float(f) => Ok(Value::Float(f.abs())),
            _ => Err(internal("abs operand type")),
        },
        UnOp::Neg => match a {
            Value::Int(i) => Ok(Value::Int(i.checked_neg().ok_or_else(overflow)?)),
            Value::Float(f) => Ok(Value::Float(-f)),
            _ => Err(internal("neg operand type")),
        },
        UnOp::ToFloat => Ok(Value::Float(as_int(a)? as f64)),
        UnOp::Not => Ok(Value::Bool(!as_bool(a)?)),
        UnOp::V3Length => {
            let v = as_vec3(a)?;
            let sum: f64 = v.iter().map(|c| f64::from(*c) * f64::from(*c)).sum();
            Ok(Value::Float(finite(sum.sqrt())?))
        }
    }
}

fn check_finite(value: &Value) -> Result<(), Trap> {
    match value {
        Value::Float(f) if !f.is_finite() => Err(Trap::NonFinite),
        Value::Vec3(v) if v.iter().any(|c| !c.is_finite()) => Err(Trap::NonFinite),
        _ => Ok(()),
    }
}

impl GraphInstance {
    pub(crate) fn new(program: Arc<Program>, vars: Vec<Value>) -> Self {
        let state_bytes = vars.iter().map(Value::byte_size).sum::<usize>() + program.flags;
        let flags = vec![false; program.flags];
        Self {
            program,
            vars,
            flags,
            pending: Vec::new(),
            state_bytes,
        }
    }

    /// The current value of a variable, by name.
    pub fn variable(&self, name: &str) -> Option<&Value> {
        let index = self.program.vars.iter().position(|v| v.name == name)?;
        self.vars.get(index)
    }

    fn run_regions(
        &mut self,
        program: &Arc<Program>,
        key: &EntryKey,
        trigger: &Trigger<'_>,
        host: &mut dyn ScriptHost,
        budget: &mut Budget,
    ) -> Result<(), Fault> {
        let Some(regions) = program.entries.get(key) else {
            return Ok(());
        };
        for region in regions {
            let mut regs = vec![Value::Unit; region.n_regs];
            match trigger {
                Trigger::Update { dt } => {
                    if let Some(r) = region.dt {
                        regs[r as usize] = Value::Float(f64::from(*dt));
                    }
                }
                Trigger::Event {
                    payload, source, ..
                } => {
                    if let Some(r) = region.payload {
                        regs[r as usize] = Value::json((*payload).clone());
                    }
                    if let Some(r) = region.source {
                        regs[r as usize] = Value::Entity(source.unwrap_or(EntityRef(0)));
                    }
                }
                _ => {}
            }
            self.run(
                program,
                Frame {
                    pc: region.pc,
                    regs,
                    func: None,
                    rets: Box::new([]),
                },
                host,
                budget,
            )?;
        }
        Ok(())
    }

    fn resume_due(
        &mut self,
        program: &Arc<Program>,
        dt: f32,
        host: &mut dyn ScriptHost,
        budget: &mut Budget,
    ) -> Result<(), Fault> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let mut due = Vec::new();
        let mut keep = Vec::new();
        for mut p in std::mem::take(&mut self.pending) {
            p.remaining -= f64::from(dt);
            if p.remaining <= 0.0 {
                self.state_bytes = self.state_bytes.saturating_sub(pending_size(&p.regs));
                due.push(p);
            } else {
                keep.push(p);
            }
        }
        self.pending = keep;
        for p in due {
            self.run(
                program,
                Frame {
                    pc: p.pc,
                    regs: p.regs,
                    func: None,
                    rets: Box::new([]),
                },
                host,
                budget,
            )?;
        }
        Ok(())
    }

    fn run(
        &mut self,
        program: &Arc<Program>,
        base: Frame,
        host: &mut dyn ScriptHost,
        budget: &mut Budget,
    ) -> Result<(), Fault> {
        let mut frames = vec![base];
        loop {
            let Some(frame) = frames.last_mut() else {
                return Ok(());
            };
            let pc = frame.pc;
            let site = || program.site_name(pc).map(|s| Site::Node(s.to_owned()));
            let Some(op) = program.code.get(pc) else {
                return Err(Fault {
                    trap: internal("program counter out of range"),
                    site: None,
                });
            };
            frame.pc += 1;
            let cost = match op {
                Op::Host { cost, .. } => u64::from(*cost),
                _ => 1,
            };
            if let Err(trap) = budget.charge(cost) {
                return Err(Fault { trap, site: site() });
            }
            match self.step(program, &mut frames, op, host, budget) {
                Ok(Step::Next) => {}
                Ok(Step::Done) => return Ok(()),
                Err(trap) => {
                    return Err(Fault { trap, site: site() });
                }
            }
        }
    }

    fn set_var(&mut self, var: u16, value: Value, budget: &Budget) -> Result<(), Trap> {
        let slot = self
            .vars
            .get_mut(var as usize)
            .ok_or_else(|| internal("variable index"))?;
        let (old, new) = (slot.byte_size(), value.byte_size());
        if self.state_bytes - old + new > budget.max_state_bytes {
            return Err(Trap::Memory);
        }
        self.state_bytes = self.state_bytes - old + new;
        *slot = value;
        Ok(())
    }

    fn step(
        &mut self,
        program: &Program,
        frames: &mut Vec<Frame>,
        op: &Op,
        host: &mut dyn ScriptHost,
        budget: &mut Budget,
    ) -> Result<Step, Trap> {
        let depth = frames.len();
        let frame = frames.last_mut().ok_or_else(|| internal("no frame"))?;
        let regs = &mut frame.regs;
        let reg = |regs: &[Value], r: Reg| -> Result<Value, Trap> {
            regs.get(r as usize)
                .cloned()
                .ok_or_else(|| internal("register out of range"))
        };
        let put = |regs: &mut [Value], r: Reg, v: Value| -> Result<(), Trap> {
            *regs
                .get_mut(r as usize)
                .ok_or_else(|| internal("register out of range"))? = v;
            Ok(())
        };
        match op {
            Op::Const { dst, k } => {
                let value = program
                    .consts
                    .get(*k as usize)
                    .cloned()
                    .ok_or_else(|| internal("constant index"))?;
                put(regs, *dst, value)?;
            }
            Op::Move { dst, src } => {
                let value = reg(regs, *src)?;
                put(regs, *dst, value)?;
            }
            Op::GetVar { dst, var } => {
                let value = self
                    .vars
                    .get(*var as usize)
                    .cloned()
                    .ok_or_else(|| internal("variable index"))?;
                put(regs, *dst, value)?;
            }
            Op::SetVar { var, src } => {
                let value = reg(regs, *src)?;
                self.set_var(*var, value, budget)?;
            }
            Op::SelfEntity { dst } => put(regs, *dst, Value::Entity(host.subject()))?,
            Op::Bin { dst, op, a, b } => {
                let value = binary(*op, &reg(regs, *a)?, &reg(regs, *b)?)?;
                put(regs, *dst, value)?;
            }
            Op::Un { dst, op, a } => {
                let value = unary(*op, &reg(regs, *a)?)?;
                put(regs, *dst, value)?;
            }
            Op::Vec3Make { dst, x, y, z } => {
                let v = [
                    as_float(&reg(regs, *x)?)? as f32,
                    as_float(&reg(regs, *y)?)? as f32,
                    as_float(&reg(regs, *z)?)? as f32,
                ];
                put(regs, *dst, finite_vec3(v)?)?;
            }
            Op::Vec3Break { dst, src } => {
                let v = as_vec3(&reg(regs, *src)?)?;
                for (i, c) in v.iter().enumerate() {
                    put(regs, *dst + i as Reg, Value::Float(f64::from(*c)))?;
                }
            }
            Op::ToJson { dst, src } => {
                let json = reg(regs, *src)?.to_json();
                put(regs, *dst, Value::json(json))?;
            }
            Op::JsonTo { dst, src, ty } => {
                let Value::Json(json) = reg(regs, *src)? else {
                    return Err(internal("expected json"));
                };
                let value = Value::from_json(*ty, &json).ok_or_else(|| {
                    Trap::host(
                        codes::HOST_ARGUMENT,
                        format!("the json value is not a {}", ty.name()),
                    )
                })?;
                put(regs, *dst, value)?;
            }
            Op::Jump(to) => frame.pc = *to as usize,
            Op::JumpIfNot { cond, to } => {
                if !as_bool(&reg(regs, *cond)?)? {
                    frame.pc = *to as usize;
                }
            }
            Op::Inc { reg: r } => {
                let next = as_int(&reg(regs, *r)?)?
                    .checked_add(1)
                    .ok_or_else(overflow)?;
                put(regs, *r, Value::Int(next))?;
            }
            Op::LoopLimit { reg: r, max } => {
                if as_int(&reg(regs, *r)?)? > i64::from(*max) {
                    return Err(Trap::Fuel);
                }
            }
            Op::DoOnce { flag, to } => {
                let seen = self
                    .flags
                    .get_mut(*flag as usize)
                    .ok_or_else(|| internal("flag index"))?;
                if *seen {
                    frame.pc = *to as usize;
                } else {
                    *seen = true;
                }
            }
            Op::Gate {
                flag,
                latch,
                open,
                dst,
            } => {
                let now = as_bool(&reg(regs, *open)?)?;
                let slot = self
                    .flags
                    .get_mut(*flag as usize)
                    .ok_or_else(|| internal("flag index"))?;
                let result = if *latch {
                    if now {
                        *slot = true;
                    }
                    *slot
                } else {
                    now
                };
                put(regs, *dst, Value::Bool(result))?;
            }
            Op::Host {
                function,
                args,
                dst,
                ..
            } => {
                let mut values = Vec::with_capacity(args.len());
                for a in args.iter() {
                    values.push(reg(regs, *a)?);
                }
                let result = host.call(*function, &values)?;
                check_finite(&result)?;
                if let Some(dst) = dst {
                    put(regs, *dst, result)?;
                }
            }
            Op::CallFn { func, args, rets } => {
                if depth >= usize::from(budget.max_call_depth) {
                    return Err(Trap::CallDepth);
                }
                let info = program
                    .functions
                    .get(*func as usize)
                    .ok_or_else(|| internal("function index"))?;
                let mut callee = vec![Value::Unit; info.n_regs];
                for ((param, _), arg) in info.params.iter().zip(args.iter()) {
                    callee[*param as usize] = reg(regs, *arg)?;
                }
                frames.push(Frame {
                    pc: info.pc,
                    regs: callee,
                    func: Some(*func),
                    rets: rets.clone(),
                });
            }
            Op::Return { srcs } => {
                let mut values = Vec::with_capacity(srcs.len());
                for s in srcs.iter() {
                    values.push(reg(regs, *s)?);
                }
                return Ok(Self::pop_frame(frames, values));
            }
            Op::End => {
                let defaults = match frame.func {
                    Some(f) => program
                        .functions
                        .get(f as usize)
                        .map(|info| info.results.iter().map(|t| t.default_value()).collect())
                        .unwrap_or_default(),
                    None => Vec::new(),
                };
                return Ok(Self::pop_frame(frames, defaults));
            }
            Op::Emit {
                event,
                payload,
                target,
            } => {
                if budget.emits_left == 0 {
                    return Err(Trap::EventLimit);
                }
                budget.emits_left -= 1;
                let target = match target {
                    Some(r) => match reg(regs, *r)? {
                        Value::Entity(e) if e.0 != 0 => Some(e),
                        Value::Entity(_) => None,
                        _ => return Err(internal("emit target")),
                    },
                    None => None,
                };
                let payload = program
                    .consts
                    .get(*payload as usize)
                    .cloned()
                    .ok_or_else(|| internal("payload constant"))?;
                host.emit(event, payload, target)?;
            }
            Op::Log { src } => {
                let Value::Str(text) = reg(regs, *src)? else {
                    return Err(internal("log message"));
                };
                host.log(&text);
            }
            Op::Wait { secs } => {
                let seconds = as_float(&reg(regs, *secs)?)?;
                if !seconds.is_finite() {
                    return Err(Trap::NonFinite);
                }
                if !(0.0..=MAX_WAIT_SECONDS).contains(&seconds) {
                    return Err(Trap::host(
                        codes::HOST_ARGUMENT,
                        format!("wait seconds must be within 0..={MAX_WAIT_SECONDS}"),
                    ));
                }
                if depth != 1 {
                    return Err(internal("wait inside a function"));
                }
                if self.pending.len() >= usize::from(budget.max_pending) {
                    return Err(Trap::Memory);
                }
                let saved = std::mem::take(regs);
                let size = pending_size(&saved);
                if self.state_bytes + size > budget.max_state_bytes {
                    return Err(Trap::Memory);
                }
                self.state_bytes += size;
                self.pending.push(Pending {
                    remaining: seconds,
                    pc: frame.pc,
                    regs: saved,
                });
                frames.clear();
                return Ok(Step::Done);
            }
        }
        Ok(Step::Next)
    }

    fn pop_frame(frames: &mut Vec<Frame>, values: Vec<Value>) -> Step {
        let Some(callee) = frames.pop() else {
            return Step::Done;
        };
        match frames.last_mut() {
            None => Step::Done,
            Some(caller) => {
                for (r, v) in callee.rets.iter().zip(values) {
                    if let Some(slot) = caller.regs.get_mut(*r as usize) {
                        *slot = v;
                    }
                }
                Step::Next
            }
        }
    }
}

impl ScriptInstance for GraphInstance {
    fn handle(
        &mut self,
        trigger: &Trigger<'_>,
        host: &mut dyn ScriptHost,
        budget: &mut Budget,
    ) -> Result<(), Fault> {
        let program = Arc::clone(&self.program);
        match trigger {
            Trigger::Start => self.run_regions(&program, &EntryKey::Start, trigger, host, budget),
            Trigger::Update { dt } => {
                self.resume_due(&program, *dt, host, budget)?;
                self.run_regions(&program, &EntryKey::Update, trigger, host, budget)
            }
            Trigger::Event { name, .. } => {
                let key = EntryKey::Event((*name).to_owned());
                self.run_regions(&program, &key, trigger, host, budget)
            }
            Trigger::Stop => {
                let result = self.run_regions(&program, &EntryKey::Stop, trigger, host, budget);
                for p in self.pending.drain(..) {
                    self.state_bytes = self.state_bytes.saturating_sub(pending_size(&p.regs));
                }
                result
            }
            Trigger::Timer { .. } => Ok(()),
        }
    }

    fn subscriptions(&self) -> &[String] {
        &self.program.subscriptions
    }

    fn debug(&self) -> InstanceDebug {
        let mut variables: Vec<(String, String)> = self
            .program
            .vars
            .iter()
            .zip(&self.vars)
            .map(|(decl, value)| {
                let mut text = format!("{value:?}");
                text.truncate(64);
                (decl.name.clone(), text)
            })
            .collect();
        variables.sort();
        variables.truncate(MAX_DEBUG_VARIABLES);
        InstanceDebug {
            state_bytes: self.state_bytes,
            pending: self.pending.len(),
            variables,
        }
    }
}
