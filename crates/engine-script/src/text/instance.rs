//! Text instance execution and its safe, synchronous host bridge.

use super::{TextProgram, engine};
use crate::{
    Budget, Fault, FnId, FnSig, InstanceDebug, ScriptError, ScriptHost, ScriptInstance, Site, Trap,
    Trigger, Value, ValueType, codes, validate_value,
};
use rhai::{
    Array, CallFnOptions, Dynamic, Engine, EvalAltResult, ImmutableString, Map, Module, Position,
    Scope, Shared,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    thread,
};

/// A named host function available to a text program. `FnId` is valid for the
/// registry whose signatures constructed these bindings.
#[derive(Clone)]
pub struct HostBinding {
    pub name: String,
    pub id: FnId,
    pub sig: FnSig,
}

#[derive(Clone)]
struct Timer {
    id: u32,
    remaining: f64,
    function: String,
}

enum Request {
    Call {
        binding: HostBinding,
        args: Vec<Value>,
        reply: SyncSender<Result<Value, Trap>>,
    },
    After {
        delay: f64,
        function: String,
        reply: SyncSender<Result<(), Trap>>,
    },
    Log {
        message: String,
        reply: SyncSender<Result<(), Trap>>,
    },
}

/// One persistent text script. A scoped worker exists only during `handle`;
/// neither channels nor a borrowed host survive a call.
pub struct TextInstance {
    program: Arc<TextProgram>,
    bindings: Vec<HostBinding>,
    scope: Scope<'static>,
    state: Dynamic,
    timers: Vec<Timer>,
    next_timer: u32,
}

impl TextInstance {
    pub fn new(program: Arc<TextProgram>, bindings: Vec<HostBinding>) -> Result<Self, ScriptError> {
        for binding in &bindings {
            binding
                .sig
                .validate()
                .map_err(|reason| ScriptError::new(codes::FUNCTION_INVALID, reason))?;
            if !crate::valid_function_name(&binding.name) {
                return Err(ScriptError::new(
                    codes::FUNCTION_INVALID,
                    "invalid host function name",
                ));
            }
        }
        Ok(Self {
            program,
            bindings,
            scope: Scope::new(),
            state: Dynamic::from(Map::new()),
            timers: Vec::new(),
            next_timer: 1,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn run_handlers(
        &mut self,
        trigger: &Trigger<'_>,
        sender: SyncSender<Request>,
        ops: Arc<AtomicU64>,
        host_cost: Arc<AtomicU64>,
        timer_count: Arc<AtomicU64>,
        fuel: u64,
        max_call_depth: u16,
        trap: Arc<Mutex<Option<Trap>>>,
    ) -> Result<(), Fault> {
        let mut engine = engine();
        engine.set_max_call_levels(usize::from(max_call_depth));
        engine.set_max_operations(fuel);
        let progress_base = Arc::new(AtomicU64::new(0));
        let base_for_callback = Arc::clone(&progress_base);
        let ops_for_callback = Arc::clone(&ops);
        engine.on_progress(move |count| {
            let total = base_for_callback
                .load(Ordering::SeqCst)
                .saturating_add(count);
            ops_for_callback.store(total, Ordering::SeqCst);
            (total.saturating_add(host_cost.load(Ordering::SeqCst)) > fuel)
                .then(|| Dynamic::from("script fuel exhausted"))
        });
        install_host_functions(&mut engine, &self.bindings, &sender, &trap);
        let call = |instance: &mut Self, name: &str, args: Vec<Dynamic>| -> Result<(), Fault> {
            if !instance.program.has_handler(name, args.len()) {
                return Ok(());
            }
            progress_base.store(ops.load(Ordering::SeqCst), Ordering::SeqCst);
            let options = CallFnOptions::new()
                .bind_this_ptr(&mut instance.state)
                .eval_ast(false);
            let result = engine.call_fn_with_options::<Dynamic>(
                options,
                &mut instance.scope,
                &instance.program.ast,
                name,
                args,
            );
            match result {
                Ok(_) => Ok(()),
                Err(error) => Err(Fault {
                    trap: trap
                        .lock()
                        .ok()
                        .and_then(|mut slot| slot.take())
                        .unwrap_or_else(|| map_error(&error)),
                    site: site(error.position()),
                }),
            }
        };
        match trigger {
            Trigger::Start => call(self, "on_start", vec![])?,
            Trigger::Update { dt } => {
                let dt = f64::from(*dt);
                if !dt.is_finite() || dt < 0.0 {
                    return Err(Fault {
                        trap: Trap::NonFinite,
                        site: None,
                    });
                }
                let mut due = Vec::new();
                self.timers.retain_mut(|timer| {
                    timer.remaining -= dt;
                    if timer.remaining <= 0.0 {
                        due.push(timer.function.clone());
                        false
                    } else {
                        true
                    }
                });
                timer_count.store(self.timers.len() as u64, Ordering::SeqCst);
                for function in due {
                    call(self, &function, vec![])?;
                }
                call(self, "on_update", vec![Dynamic::from(dt)])?;
            }
            Trigger::Event {
                name,
                payload,
                source,
            } => {
                if self
                    .program
                    .subscriptions()
                    .binary_search_by(|event| event.as_str().cmp(name))
                    .is_ok()
                {
                    let payload = json_to_dynamic(payload, 0, &mut 0)
                        .map_err(|trap| Fault { trap, site: None })?;
                    let source = source.map_or(Dynamic::UNIT, Dynamic::from);
                    call(
                        self,
                        "on_event",
                        vec![Dynamic::from((*name).to_owned()), payload, source],
                    )?;
                }
            }
            Trigger::Timer { id } => {
                if let Some(timer) = self.timers.iter().find(|timer| timer.id == *id).cloned() {
                    call(self, &timer.function, vec![])?;
                }
            }
            Trigger::Stop => call(self, "on_stop", vec![])?,
        }
        Ok(())
    }

    /// Runs one trigger, then enforces the persistent state budget. State that is
    /// too large or nested deeper than the bridge walks is discarded here, on the
    /// worker thread, so the deep structure is never dropped on the host's stack.
    #[allow(clippy::too_many_arguments)]
    fn run(
        &mut self,
        trigger: &Trigger<'_>,
        sender: SyncSender<Request>,
        ops: Arc<AtomicU64>,
        host_cost: Arc<AtomicU64>,
        timer_count: Arc<AtomicU64>,
        fuel: u64,
        max_state_bytes: usize,
        max_call_depth: u16,
        trap: Arc<Mutex<Option<Trap>>>,
    ) -> Result<(), Fault> {
        let result = self.run_handlers(
            trigger,
            sender,
            ops,
            host_cost,
            timer_count,
            fuel,
            max_call_depth,
            trap,
        );
        let too_big =
            dynamic_size(&self.state, 0, &mut 0).map_or(true, |size| size > max_state_bytes);
        if too_big {
            self.state = Dynamic::from(Map::new());
        }
        match result {
            Ok(()) if too_big => Err(Fault {
                trap: Trap::Memory,
                site: None,
            }),
            other => other,
        }
    }
}

impl ScriptInstance for TextInstance {
    fn handle(
        &mut self,
        trigger: &Trigger<'_>,
        host: &mut dyn ScriptHost,
        budget: &mut Budget,
    ) -> Result<(), Fault> {
        if budget.fuel == 0 {
            return Err(Fault {
                trap: Trap::Fuel,
                site: None,
            });
        }
        let (sender, receiver) = mpsc::sync_channel(0);
        let ops = Arc::new(AtomicU64::new(0));
        let host_cost = Arc::new(AtomicU64::new(0));
        let timer_count = Arc::new(AtomicU64::new(self.timers.len() as u64));
        let trapped = Arc::new(Mutex::new(None));
        let fuel = budget.fuel;
        let max_state_bytes = budget.max_state_bytes;
        let max_call_depth = budget.max_call_depth;
        let mut pending = Vec::new();
        let program = Arc::clone(&self.program);
        let result = thread::scope(|scope| {
            let ops_worker = Arc::clone(&ops);
            let cost_worker = Arc::clone(&host_cost);
            let count_worker = Arc::clone(&timer_count);
            let trap_worker = Arc::clone(&trapped);
            let instance = &mut *self;
            let worker = thread::Builder::new()
                .stack_size(super::WORKER_STACK_BYTES)
                .spawn_scoped(scope, move || {
                    instance.run(
                        trigger,
                        sender,
                        ops_worker,
                        cost_worker,
                        count_worker,
                        fuel,
                        max_state_bytes,
                        max_call_depth,
                        trap_worker,
                    )
                })
                .map_err(|e| Fault {
                    trap: Trap::host(codes::HOST_FAILED, format!("text worker: {e}")),
                    site: None,
                })?;
            let mut counted_ops = 0;
            while let Ok(request) = receiver.recv() {
                let current = ops.load(Ordering::SeqCst);
                let charge = budget.charge(current.saturating_sub(counted_ops));
                counted_ops = current;
                match request {
                    Request::Call {
                        binding,
                        args,
                        reply,
                    } => {
                        let answer = charge
                            .and_then(|_| budget.charge(u64::from(binding.sig.cost)))
                            .and_then(|_| host.call(binding.id, &args));
                        host_cost.fetch_add(u64::from(binding.sig.cost), Ordering::SeqCst);
                        let _ = reply.send(answer);
                    }
                    Request::After {
                        delay,
                        function,
                        reply,
                    } => {
                        let answer = charge.and_then(|_| {
                            if !(0.0..=3600.0).contains(&delay) || !delay.is_finite() {
                                return Err(Trap::host(
                                    codes::HOST_ARGUMENT,
                                    "invalid timer delay",
                                ));
                            }
                            if !program.has_handler(&function, 0) {
                                return Err(Trap::host(
                                    codes::TEXT_HANDLER,
                                    "timer function is missing or takes arguments",
                                ));
                            }
                            if pending.len() + timer_count.load(Ordering::SeqCst) as usize
                                >= usize::from(budget.max_pending)
                            {
                                return Err(Trap::Memory);
                            }
                            pending.push((delay, function));
                            Ok(())
                        });
                        let _ = reply.send(answer);
                    }
                    Request::Log { message, reply } => {
                        let answer = charge.and_then(|_| budget.charge(1));
                        if answer.is_ok() {
                            host.log(&message);
                        }
                        host_cost.fetch_add(1, Ordering::SeqCst);
                        let _ = reply.send(answer);
                    }
                }
            }
            budget
                .charge(ops.load(Ordering::SeqCst).saturating_sub(counted_ops))
                .map_err(|trap| Fault { trap, site: None })?;
            worker.join().map_err(|_| Fault {
                trap: Trap::host(codes::HOST_FAILED, "text worker panicked"),
                site: None,
            })?
        });
        if result.is_ok() {
            for (delay, function) in pending {
                self.timers.push(Timer {
                    id: self.next_timer,
                    remaining: delay,
                    function,
                });
                self.next_timer = self.next_timer.wrapping_add(1);
            }
        }
        result
    }

    fn subscriptions(&self) -> &[String] {
        self.program.subscriptions()
    }

    fn debug(&self) -> InstanceDebug {
        InstanceDebug {
            state_bytes: dynamic_size(&self.state, 0, &mut 0).unwrap_or(usize::MAX),
            pending: self.timers.len(),
            variables: Vec::new(),
        }
    }
}

fn site(position: Position) -> Option<Site> {
    Some(Site::Pos {
        line: position.line()? as u32,
        col: position.position().unwrap_or(1) as u32,
    })
}

fn map_error(error: &EvalAltResult) -> Trap {
    match error {
        EvalAltResult::ErrorTooManyOperations(..) => Trap::Fuel,
        EvalAltResult::ErrorTerminated(..) => Trap::Fuel,
        EvalAltResult::ErrorStackOverflow(..) => Trap::CallDepth,
        // Rhai's own size and scope limits are the memory limits of the text backend.
        EvalAltResult::ErrorDataTooLarge(..)
        | EvalAltResult::ErrorTooManyVariables(..)
        | EvalAltResult::ErrorTooManyModules(..) => Trap::Memory,
        _ => Trap::host(codes::HOST_FAILED, error.to_string()),
    }
}

#[derive(Default)]
struct Namespace {
    functions: Vec<(String, HostBinding)>,
    children: BTreeMap<String, Namespace>,
}

impl Namespace {
    fn insert(&mut self, segments: &[&str], binding: HostBinding) {
        if let [function] = segments {
            self.functions.push(((*function).to_owned(), binding));
        } else if let Some((first, rest)) = segments.split_first() {
            self.children
                .entry((*first).to_owned())
                .or_default()
                .insert(rest, binding);
        }
    }

    fn into_module(self, sender: &SyncSender<Request>, trap: &Arc<Mutex<Option<Trap>>>) -> Module {
        let mut module = Module::new();
        for (name, binding) in self.functions {
            register_binding(
                &mut module,
                &name,
                binding,
                sender.clone(),
                Arc::clone(trap),
            );
        }
        for (name, child) in self.children {
            module.set_sub_module(name, Shared::new(child.into_module(sender, trap)));
        }
        module
    }
}

fn install_host_functions(
    engine: &mut Engine,
    bindings: &[HostBinding],
    sender: &SyncSender<Request>,
    trap: &Arc<Mutex<Option<Trap>>>,
) {
    for name in ["print", "debug"] {
        let sender = sender.clone();
        let slot = Arc::clone(trap);
        engine.register_fn(
            name,
            move |message: ImmutableString| -> Result<ImmutableString, Box<EvalAltResult>> {
                if message.len() > crate::MAX_STRING_BYTES {
                    return Err(capture_trap(&slot, Trap::Memory));
                }
                let (reply, answer) = mpsc::sync_channel(0);
                sender
                    .send(Request::Log {
                        message: message.to_string(),
                        reply,
                    })
                    .map_err(|_| {
                        capture_trap(&slot, Trap::host(codes::HOST_FAILED, "log bridge closed"))
                    })?;
                answer
                    .recv()
                    .map_err(|_| {
                        capture_trap(
                            &slot,
                            Trap::host(codes::HOST_FAILED, "log bridge reply closed"),
                        )
                    })?
                    .map_err(|e| capture_trap(&slot, e))?;
                Ok(message)
            },
        );
        let slot = Arc::clone(trap);
        engine.register_fn(
            name,
            move |_value: Dynamic| -> Result<ImmutableString, Box<EvalAltResult>> {
                Err(capture_trap(
                    &slot,
                    Trap::host(codes::HOST_ARGUMENT, "print/debug requires a string"),
                ))
            },
        );
    }
    let mut roots: BTreeMap<String, Namespace> = BTreeMap::new();
    roots.insert("engine".to_owned(), Namespace::default());
    for binding in bindings {
        let mut segments = binding.name.split('.');
        if let Some(root) = segments.next() {
            roots
                .entry(root.to_owned())
                .or_default()
                .insert(&segments.collect::<Vec<_>>(), binding.clone());
        }
    }
    for (name, namespace) in roots {
        let mut module = namespace.into_module(sender, trap);
        if name == "engine" {
            install_after(&mut module, sender.clone(), Arc::clone(trap));
        }
        engine.register_static_module(name, Shared::new(module));
    }
}

fn runtime_error() -> Box<EvalAltResult> {
    EvalAltResult::ErrorRuntime(Dynamic::from("text host call failed"), Position::NONE).into()
}

fn capture_trap(slot: &Arc<Mutex<Option<Trap>>>, trapped: Trap) -> Box<EvalAltResult> {
    if let Ok(mut slot) = slot.lock() {
        *slot = Some(trapped);
    }
    runtime_error()
}

fn invoke(
    binding: &HostBinding,
    values: &[Dynamic],
    sender: &SyncSender<Request>,
    slot: &Arc<Mutex<Option<Trap>>>,
) -> Result<Dynamic, Box<EvalAltResult>> {
    let args = binding
        .sig
        .params
        .iter()
        .zip(values)
        .map(|(param, value)| dynamic_to_value(param.ty, value))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| capture_trap(slot, e))?;
    crate::check_args(&binding.name, &binding.sig, &args)
        .map_err(|e| capture_trap(slot, e.into()))?;
    let (reply, answer) = mpsc::sync_channel(0);
    sender
        .send(Request::Call {
            binding: binding.clone(),
            args,
            reply,
        })
        .map_err(|_| capture_trap(slot, Trap::host(codes::HOST_FAILED, "host bridge closed")))?;
    let value = answer
        .recv()
        .map_err(|_| {
            capture_trap(
                slot,
                Trap::host(codes::HOST_FAILED, "host bridge reply closed"),
            )
        })?
        .map_err(|e| capture_trap(slot, e))?;
    value_to_dynamic(&value).map_err(|e| capture_trap(slot, e))
}

macro_rules! register_arity {
    ($module:expr, $name:expr, $binding:expr, $sender:expr, $slot:expr; $($arg:ident),*) => {{
        let binding = $binding.clone();
        let sender = $sender.clone();
        let slot = Arc::clone(&$slot);
        $module.set_native_fn($name, move |$($arg: Dynamic),*| -> Result<Dynamic, Box<EvalAltResult>> {
            let args: Vec<Dynamic> = vec![$($arg),*];
            invoke(&binding, &args, &sender, &slot)
        });
    }};
}

fn register_binding(
    module: &mut Module,
    name: &str,
    binding: HostBinding,
    sender: SyncSender<Request>,
    slot: Arc<Mutex<Option<Trap>>>,
) {
    match binding.sig.params.len() {
        0 => register_arity!(module, name, binding, sender, slot;),
        1 => register_arity!(module, name, binding, sender, slot; a),
        2 => register_arity!(module, name, binding, sender, slot; a,b),
        3 => register_arity!(module, name, binding, sender, slot; a,b,c),
        4 => register_arity!(module, name, binding, sender, slot; a,b,c,d),
        5 => register_arity!(module, name, binding, sender, slot; a,b,c,d,e),
        6 => register_arity!(module, name, binding, sender, slot; a,b,c,d,e,f),
        7 => register_arity!(module, name, binding, sender, slot; a,b,c,d,e,f,g),
        8 => register_arity!(module, name, binding, sender, slot; a,b,c,d,e,f,g,h),
        9 => register_arity!(module, name, binding, sender, slot; a,b,c,d,e,f,g,h,i),
        10 => register_arity!(module, name, binding, sender, slot; a,b,c,d,e,f,g,h,i,j),
        11 => register_arity!(module, name, binding, sender, slot; a,b,c,d,e,f,g,h,i,j,k),
        12 => register_arity!(module, name, binding, sender, slot; a,b,c,d,e,f,g,h,i,j,k,l),
        13 => register_arity!(module, name, binding, sender, slot; a,b,c,d,e,f,g,h,i,j,k,l,m),
        14 => register_arity!(module, name, binding, sender, slot; a,b,c,d,e,f,g,h,i,j,k,l,m,n),
        15 => register_arity!(module, name, binding, sender, slot; a,b,c,d,e,f,g,h,i,j,k,l,m,n,o),
        16 => register_arity!(module, name, binding, sender, slot; a,b,c,d,e,f,g,h,i,j,k,l,m,n,o,p),
        _ => unreachable!("signature validation caps parameters at 16"),
    }
}

fn install_after(module: &mut Module, sender: SyncSender<Request>, slot: Arc<Mutex<Option<Trap>>>) {
    module.set_native_fn(
        "after",
        move |delay: f64, function: ImmutableString| -> Result<(), Box<EvalAltResult>> {
            let (reply, answer) = mpsc::sync_channel(0);
            sender
                .send(Request::After {
                    delay,
                    function: function.to_string(),
                    reply,
                })
                .map_err(|_| {
                    capture_trap(&slot, Trap::host(codes::HOST_FAILED, "timer bridge closed"))
                })?;
            answer
                .recv()
                .map_err(|_| {
                    capture_trap(
                        &slot,
                        Trap::host(codes::HOST_FAILED, "timer bridge reply closed"),
                    )
                })?
                .map_err(|e| capture_trap(&slot, e))
        },
    );
}

fn argument(message: impl Into<String>) -> Trap {
    Trap::host(codes::HOST_ARGUMENT, message)
}

fn dynamic_to_value(ty: ValueType, value: &Dynamic) -> Result<Value, Trap> {
    if value.is_shared() {
        return Err(Trap::Memory);
    }
    let found = match ty {
        ValueType::Bool => value.clone().try_cast::<bool>().map(Value::Bool),
        ValueType::Int => value.clone().try_cast::<i64>().map(Value::Int),
        ValueType::Float => value.clone().try_cast::<f64>().map(Value::Float),
        ValueType::Str => value
            .clone()
            .try_cast::<ImmutableString>()
            .map(|v| Value::str(v.as_str())),
        // `()` is the null entity handle (no target), for example an `engine::emit`
        // broadcast. A script cannot build any other handle.
        ValueType::Entity if value.is_unit() => Some(Value::Entity(crate::EntityRef(0))),
        ValueType::Entity => value
            .clone()
            .try_cast::<crate::EntityRef>()
            .map(Value::Entity),
        ValueType::Vec3 => value.read_lock::<Array>().and_then(|items| {
            if items.len() != 3 {
                return None;
            }
            let mut out = [0.0f32; 3];
            for (slot, item) in out.iter_mut().zip(items.iter()) {
                *slot = item.clone().try_cast::<f64>()? as f32;
            }
            Some(Value::Vec3(out))
        }),
        ValueType::Json => Some(Value::json(dynamic_to_json(value, 0, &mut 0)?)),
    }
    .ok_or_else(|| argument(format!("expected {}", ty.name())))?;
    validate_value(ty, &found).map_err(argument)?;
    Ok(found)
}

fn value_to_dynamic(value: &Value) -> Result<Dynamic, Trap> {
    match value {
        Value::Unit => Ok(Dynamic::UNIT),
        Value::Bool(v) => Ok(Dynamic::from(*v)),
        Value::Int(v) => Ok(Dynamic::from(*v)),
        Value::Float(v) if v.is_finite() => Ok(Dynamic::from(*v)),
        Value::Float(_) => Err(Trap::NonFinite),
        Value::Str(v) => Ok(Dynamic::from(v.to_string())),
        Value::Vec3(v) if v.iter().all(|n| n.is_finite()) => Ok(Dynamic::from_array(
            v.iter().map(|n| Dynamic::from(f64::from(*n))).collect(),
        )),
        Value::Vec3(_) => Err(Trap::NonFinite),
        Value::Entity(v) => Ok(Dynamic::from(*v)),
        Value::Json(v) => json_to_dynamic(v, 0, &mut 0),
    }
}

fn json_to_dynamic(
    value: &serde_json::Value,
    depth: usize,
    nodes: &mut usize,
) -> Result<Dynamic, Trap> {
    *nodes = nodes.saturating_add(1);
    if depth > 8 || *nodes > 512 {
        return Err(Trap::Memory);
    }
    match value {
        serde_json::Value::Null => Ok(Dynamic::UNIT),
        serde_json::Value::Bool(v) => Ok(Dynamic::from(*v)),
        serde_json::Value::Number(v) => v
            .as_i64()
            .map(Dynamic::from)
            .or_else(|| v.as_f64().filter(|n| n.is_finite()).map(Dynamic::from))
            .ok_or(Trap::NonFinite),
        serde_json::Value::String(v) if v.len() <= crate::MAX_STRING_BYTES => {
            Ok(Dynamic::from(v.clone()))
        }
        serde_json::Value::String(_) => Err(Trap::Memory),
        serde_json::Value::Array(v) if v.len() <= 512 => Ok(Dynamic::from_array(
            v.iter()
                .map(|item| json_to_dynamic(item, depth + 1, nodes))
                .collect::<Result<Array, _>>()?,
        )),
        serde_json::Value::Array(_) => Err(Trap::Memory),
        serde_json::Value::Object(v) if v.len() <= 512 => {
            let mut map = Map::new();
            for (name, item) in v {
                map.insert(
                    name.clone().into(),
                    json_to_dynamic(item, depth + 1, nodes)?,
                );
            }
            Ok(Dynamic::from_map(map))
        }
        serde_json::Value::Object(_) => Err(Trap::Memory),
    }
}

fn dynamic_to_json(
    value: &Dynamic,
    depth: usize,
    nodes: &mut usize,
) -> Result<serde_json::Value, Trap> {
    *nodes = nodes.saturating_add(1);
    if depth > 8 || *nodes > 512 {
        return Err(Trap::Memory);
    }
    if value.is_shared() {
        return Err(Trap::Memory);
    }
    if value.is_unit() {
        return Ok(serde_json::Value::Null);
    }
    if let Some(v) = value.clone().try_cast::<bool>() {
        return Ok(v.into());
    }
    if let Some(v) = value.clone().try_cast::<i64>() {
        return Ok(v.into());
    }
    if let Some(v) = value.clone().try_cast::<f64>() {
        return serde_json::Number::from_f64(v)
            .map(serde_json::Value::Number)
            .ok_or(Trap::NonFinite);
    }
    if let Some(v) = value.clone().try_cast::<ImmutableString>() {
        if v.len() > crate::MAX_STRING_BYTES {
            return Err(Trap::Memory);
        }
        return Ok(v.to_string().into());
    }
    if let Some(v) = value.read_lock::<Array>() {
        if v.len() > 512 {
            return Err(Trap::Memory);
        }
        return Ok(serde_json::Value::Array(
            v.iter()
                .map(|item| dynamic_to_json(item, depth + 1, nodes))
                .collect::<Result<Vec<_>, _>>()?,
        ));
    }
    if let Some(v) = value.read_lock::<Map>() {
        if v.len() > 512 {
            return Err(Trap::Memory);
        }
        let mut map = serde_json::Map::new();
        for (key, item) in v.iter() {
            map.insert(key.to_string(), dynamic_to_json(item, depth + 1, nodes)?);
        }
        return Ok(serde_json::Value::Object(map));
    }
    Err(argument("unsupported JSON value"))
}

fn dynamic_size(value: &Dynamic, depth: usize, nodes: &mut usize) -> Result<usize, Trap> {
    *nodes = nodes.saturating_add(1);
    if depth > 8 || *nodes > 512 || value.is_shared() {
        return Err(Trap::Memory);
    }
    if let Some(v) = value.read_lock::<ImmutableString>() {
        return Ok(16 + v.len());
    }
    if let Some(v) = value.read_lock::<Array>() {
        return v.iter().try_fold(16usize, |size, item| {
            size.checked_add(dynamic_size(item, depth + 1, nodes)?)
                .ok_or(Trap::Memory)
        });
    }
    if let Some(v) = value.read_lock::<Map>() {
        return v.iter().try_fold(16usize, |size, (key, item)| {
            let item_size = dynamic_size(item, depth + 1, nodes)?;
            size.checked_add(key.len())
                .and_then(|size| size.checked_add(item_size))
                .ok_or(Trap::Memory)
        });
    }
    if value.is_unit()
        || value.read_lock::<bool>().is_some()
        || value.read_lock::<i64>().is_some()
        || value.read_lock::<f64>().is_some()
        || value.read_lock::<crate::EntityRef>().is_some()
    {
        Ok(16)
    } else {
        Err(Trap::Memory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FnSig, ValueType};

    struct Recorder {
        calls: usize,
        fail: bool,
        logs: Vec<String>,
    }

    impl ScriptHost for Recorder {
        fn call(&mut self, _function: FnId, _args: &[Value]) -> Result<Value, Trap> {
            self.calls += 1;
            if self.fail {
                Err(Trap::host(codes::HOST_FAILED, "recorder rejected call"))
            } else {
                Ok(Value::Unit)
            }
        }

        fn emit(
            &mut self,
            _name: &str,
            _payload: Value,
            _target: Option<crate::EntityRef>,
        ) -> Result<(), Trap> {
            Ok(())
        }

        fn log(&mut self, message: &str) {
            self.logs.push(message.to_owned());
        }

        fn subject(&self) -> crate::EntityRef {
            crate::EntityRef(1)
        }
    }

    #[test]
    fn json_boundary_counts_nodes_across_siblings() {
        let document =
            serde_json::Value::Array((0..512).map(|_| serde_json::Value::Bool(true)).collect());
        assert!(matches!(
            json_to_dynamic(&document, 0, &mut 0),
            Err(Trap::Memory)
        ));
        let value = Dynamic::from_array(vec![Dynamic::from(true); 512]);
        assert_eq!(dynamic_to_json(&value, 0, &mut 0), Err(Trap::Memory));
    }

    #[test]
    fn host_failure_releases_scoped_bridge() {
        let program =
            Arc::new(TextProgram::compile(b"fn on_start() { game::fail(true); }").unwrap());
        let bindings = vec![HostBinding {
            name: "game.fail".to_owned(),
            id: FnId(0),
            sig: FnSig::new(&[("value", ValueType::Bool)], None),
        }];
        let mut instance = TextInstance::new(program, bindings).unwrap();
        let mut host = Recorder {
            calls: 0,
            fail: true,
            logs: vec![],
        };
        let fault = instance
            .handle(&Trigger::Start, &mut host, &mut Budget::default())
            .unwrap_err();
        assert_eq!(fault.code(), codes::HOST_FAILED);
        assert_eq!(host.calls, 1);
    }

    #[test]
    fn fuel_is_cumulative_across_timer_and_update_handlers() {
        let source = br#"
            fn on_start() { this.count = 0; engine::after(0.0, "fire"); }
            fn fire() { for n in 0..70 { this.count += 1; } }
            fn on_update(dt) { for n in 0..70 { this.count += 1; } }
        "#;
        let program = Arc::new(TextProgram::compile(source).unwrap());
        let mut instance = TextInstance::new(program, vec![]).unwrap();
        let mut host = Recorder {
            calls: 0,
            fail: false,
            logs: vec![],
        };
        instance
            .handle(&Trigger::Start, &mut host, &mut Budget::default())
            .unwrap();
        let mut budget = Budget::with_fuel(300);
        let fault = instance
            .handle(&Trigger::Update { dt: 0.1 }, &mut host, &mut budget)
            .unwrap_err();
        assert_eq!(fault.code(), codes::FUEL_EXHAUSTED, "{fault}");
    }

    #[test]
    fn due_timers_release_capacity_before_callbacks_schedule_more() {
        let source = br#"
            fn on_start() {
                for n in 0..63 { engine::after(0.0, "noop"); }
                engine::after(0.0, "replace");
            }
            fn noop() {}
            fn replace() { engine::after(1.0, "noop"); }
        "#;
        let program = Arc::new(TextProgram::compile(source).unwrap());
        let mut instance = TextInstance::new(program, vec![]).unwrap();
        let mut host = Recorder {
            calls: 0,
            fail: false,
            logs: vec![],
        };
        instance
            .handle(&Trigger::Start, &mut host, &mut Budget::default())
            .unwrap();
        assert_eq!(instance.debug().pending, 64);
        instance
            .handle(
                &Trigger::Update { dt: 0.1 },
                &mut host,
                &mut Budget::default(),
            )
            .unwrap();
        assert_eq!(instance.debug().pending, 1);
    }

    #[test]
    fn print_and_debug_go_to_the_host_log() {
        let program = Arc::new(
            TextProgram::compile(br#"fn on_start() { print("hello"); debug("world"); }"#).unwrap(),
        );
        let mut instance = TextInstance::new(program, vec![]).unwrap();
        let mut host = Recorder {
            calls: 0,
            fail: false,
            logs: vec![],
        };
        instance
            .handle(&Trigger::Start, &mut host, &mut Budget::default())
            .unwrap();
        assert_eq!(host.logs, ["hello", "world"]);
    }

    #[test]
    fn print_and_debug_reject_non_strings() {
        for source in [
            br#"fn on_start() { print(42); }"#.as_slice(),
            br#"fn on_start() { debug([1]); }"#.as_slice(),
        ] {
            let program = Arc::new(TextProgram::compile(source).unwrap());
            let mut instance = TextInstance::new(program, vec![]).unwrap();
            let mut host = Recorder {
                calls: 0,
                fail: false,
                logs: vec![],
            };
            let fault = instance
                .handle(&Trigger::Start, &mut host, &mut Budget::default())
                .unwrap_err();
            assert_eq!(fault.code(), codes::HOST_ARGUMENT, "{fault}");
            assert!(host.logs.is_empty());
        }
    }
}
