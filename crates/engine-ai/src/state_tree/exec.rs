//! The tick executor: runs one agent's [`StateTreeInstance`] over a shared tree.
//!
//! # Tick
//!
//! 1. Evaluators run in authoring order (a native evaluator's first run is its
//!    `start`) and write the blackboard.
//! 2. On the first tick the initial states are selected and entered. On later
//!    ticks the running tasks of the active states tick, top state first.
//! 3. Each active state latches its completion result (see below).
//! 4. Armed delayed transitions advance their timers by `dt`.
//! 5. Transitions are evaluated: candidates of all active states are ordered by
//!    priority (high first), then state depth (deep first), then authoring order;
//!    the first whose trigger fired, conditions hold, delay elapsed and target is
//!    selectable fires. Entering the new states may complete tasks at once, so
//!    evaluation repeats (without ticking tasks or evaluators again) up to
//!    [`MAX_TRANSITIONS_PER_TICK`] times.
//! 6. A completion that no transition could ever handle completes the tree.
//!
//! Nothing here reads a clock or an RNG that is not seeded by authored data:
//! equal inputs and `dt` sequences give equal outputs.
use super::{
    compile::{
        Completion, ComputeOp, Cond, EvalKind, MoveTarget, Selection, StateDef, StateTree, Target,
        TaskKind, Trigger,
    },
    host::StateTreeHost,
};
use crate::{
    behavior_tree::{MoveStatus, NativeCall, Operator, Status},
    blackboard::{BbValue, Blackboard, BlackboardError},
    rng::SplitMix64,
};
use serde::Serialize;
use serde_json::Value;
use std::{
    cmp::{Ordering, Reverse},
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

/// Most entries kept in the transition log.
pub const MAX_LOG: usize = 64;
/// Most transitions one tick may fire (each fire can chain into the next).
pub const MAX_TRANSITIONS_PER_TICK: u32 = 8;
/// Most events an instance holds between two ticks; later ones are dropped.
pub const MAX_PENDING_EVENTS: usize = 32;
/// Tolerance when comparing accumulated seconds with authored durations.
const EPS: f64 = 1e-6;

/// What the tree is doing as a whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Created, no tick yet.
    Idle,
    Running,
    /// A transition or an unhandled completion ended the tree.
    Finished(Status),
    /// [`StateTreeInstance::stop`] ended the tree.
    Stopped,
}

/// What a log entry records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LogEvent {
    Entered,
    Exited,
    Transition,
    Finished(Status),
}

#[derive(Clone, Copy, Debug)]
enum LogTarget {
    State(usize),
    Transition(usize),
    Tree,
}

#[derive(Clone, Copy, Debug)]
struct LogEntry {
    tick: u64,
    event: LogEvent,
    target: LogTarget,
}

/// One fired transition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TransitionRecord {
    /// Graph node id of the transition.
    pub transition: String,
    /// The state that owns the transition.
    pub from: String,
    /// The leaf state entered; absent when the transition completed the tree.
    pub to: Option<String>,
    pub trigger: &'static str,
}

/// What one tick did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TickReport {
    /// Set when the tree completed during this tick.
    pub finished: Option<Status>,
    /// Transitions fired, in order.
    pub transitions: Vec<TransitionRecord>,
    /// State ids exited this tick, in order (child before parent).
    pub exited: Vec<String>,
    /// State ids entered this tick, in order (parent before child).
    pub entered: Vec<String>,
    /// Transition evaluation passes performed.
    pub passes: u32,
}

#[derive(Clone, Copy, Debug, Default)]
struct TaskRt {
    started: bool,
    done: Option<Status>,
    elapsed: f64,
}

#[derive(Clone, Debug, Default)]
struct StateRt {
    tasks: Vec<TaskRt>,
    result: Option<Status>,
}

#[derive(Clone, Debug)]
struct TransRt {
    armed: bool,
    elapsed: f64,
    delay: f64,
    rng: SplitMix64,
}

struct State {
    phase: Phase,
    blackboard: Blackboard,
    time: f64,
    ticks: u64,
    /// Active states from the top-level state down to the leaf.
    path: Vec<usize>,
    states: Vec<StateRt>,
    trans: Vec<TransRt>,
    evaluators_started: Vec<bool>,
    events: Vec<String>,
    dropped_events: u64,
    log: VecDeque<LogEntry>,
    last_finished: Option<Status>,
}

/// One agent's running state tree: active path, per-state runtime and blackboard.
pub struct StateTreeInstance {
    tree: Arc<StateTree>,
    state: State,
}

/// A state's completion from its own tasks, if decided.
fn state_result(def: &StateDef, tasks: &[TaskRt]) -> Option<Status> {
    let mut considered = def
        .tasks
        .iter()
        .zip(tasks)
        .filter(|(task, _)| task.considered);
    match def.completion {
        Completion::Any => considered.find_map(|(_, rt)| rt.done),
        Completion::All => {
            let (mut any, mut all) = (false, true);
            for (_, rt) in considered {
                any = true;
                match rt.done {
                    Some(Status::Failure) => return Some(Status::Failure),
                    Some(_) => {}
                    None => all = false,
                }
            }
            (any && all).then_some(Status::Success)
        }
    }
}

fn evaluate(condition: &Cond, blackboard: &Blackboard) -> bool {
    let current = blackboard.get(&condition.key);
    match condition.op {
        Operator::IsSet => current.is_some(),
        Operator::IsNotSet => current.is_none(),
        op => {
            let (Some(left), Some(right)) = (current, condition.value.as_ref()) else {
                return false;
            };
            match op {
                Operator::Eq => left.loosely_equals(right),
                Operator::Ne => !left.loosely_equals(right),
                Operator::Lt => left.partial_compare(right) == Some(Ordering::Less),
                Operator::Le => matches!(
                    left.partial_compare(right),
                    Some(Ordering::Less | Ordering::Equal)
                ),
                Operator::Gt => left.partial_compare(right) == Some(Ordering::Greater),
                _ => matches!(
                    left.partial_compare(right),
                    Some(Ordering::Greater | Ordering::Equal)
                ),
            }
        }
    }
}

fn as_f64(value: &BbValue) -> Option<f64> {
    match value {
        BbValue::Int(v) => Some(*v as f64),
        BbValue::Float(v) => Some(*v),
        _ => None,
    }
}

/// Result of a compute evaluator; `None` (clearing the key) for unset or
/// mistyped operands and for non-finite results.
fn compute(
    op: ComputeOp,
    a: Option<&BbValue>,
    b: Option<&BbValue>,
    int_out: bool,
) -> Option<BbValue> {
    let a = a?;
    match op {
        ComputeOp::Not => match a {
            BbValue::Bool(v) => Some(BbValue::Bool(!v)),
            _ => None,
        },
        ComputeOp::And | ComputeOp::Or => match (a, b?) {
            (BbValue::Bool(x), BbValue::Bool(y)) => Some(BbValue::Bool(if op == ComputeOp::And {
                *x && *y
            } else {
                *x || *y
            })),
            _ => None,
        },
        ComputeOp::Distance => match (a, b?) {
            (BbValue::Vec3(x), BbValue::Vec3(y)) => {
                let square = |i: usize| {
                    let d = f64::from(x[i]) - f64::from(y[i]);
                    d * d
                };
                Some(BbValue::Float((square(0) + square(1) + square(2)).sqrt()))
            }
            _ => None,
        },
        ComputeOp::Add | ComputeOp::Sub | ComputeOp::Mul | ComputeOp::Min | ComputeOp::Max => {
            let b = b?;
            if int_out {
                let (BbValue::Int(x), BbValue::Int(y)) = (a, b) else {
                    return None;
                };
                return Some(BbValue::Int(match op {
                    ComputeOp::Add => x.saturating_add(*y),
                    ComputeOp::Sub => x.saturating_sub(*y),
                    ComputeOp::Mul => x.saturating_mul(*y),
                    ComputeOp::Min => *x.min(y),
                    _ => *x.max(y),
                }));
            }
            let (x, y) = (as_f64(a)?, as_f64(b)?);
            let value = match op {
                ComputeOp::Add => x + y,
                ComputeOp::Sub => x - y,
                ComputeOp::Mul => x * y,
                ComputeOp::Min => x.min(y),
                _ => x.max(y),
            };
            value.is_finite().then_some(BbValue::Float(value))
        }
    }
}

impl StateTreeInstance {
    /// Creates an agent over `tree`. `overrides` set initial blackboard values by
    /// key; `seed` is mixed into every transition's delay variance stream.
    ///
    /// # Errors
    /// An override naming an undeclared key or holding the wrong type.
    pub fn new(
        tree: Arc<StateTree>,
        overrides: &BTreeMap<String, Value>,
        seed: u64,
    ) -> Result<Self, BlackboardError> {
        let mut blackboard = Blackboard::from_variables(tree.variables())?;
        for (key, value) in overrides {
            blackboard.set_json(key, value)?;
        }
        blackboard.take_changes();
        let states = tree
            .states
            .iter()
            .map(|s| StateRt {
                tasks: vec![TaskRt::default(); s.tasks.len()],
                result: None,
            })
            .collect();
        let trans = tree
            .transitions
            .iter()
            .enumerate()
            .map(|(index, t)| TransRt {
                armed: false,
                elapsed: 0.0,
                delay: t.delay,
                rng: SplitMix64::derive(seed, index as u64),
            })
            .collect();
        let evaluators_started = vec![false; tree.evaluators.len()];
        Ok(Self {
            tree,
            state: State {
                phase: Phase::Idle,
                blackboard,
                time: 0.0,
                ticks: 0,
                path: Vec::new(),
                states,
                trans,
                evaluators_started,
                events: Vec::new(),
                dropped_events: 0,
                log: VecDeque::new(),
                last_finished: None,
            },
        })
    }

    pub fn tree(&self) -> &Arc<StateTree> {
        &self.tree
    }
    pub fn blackboard(&self) -> &Blackboard {
        &self.state.blackboard
    }
    /// Writes made here are visible to the next tick's transitions.
    pub fn blackboard_mut(&mut self) -> &mut Blackboard {
        &mut self.state.blackboard
    }
    pub fn ticks(&self) -> u64 {
        self.state.ticks
    }
    pub fn phase(&self) -> Phase {
        self.state.phase
    }
    /// How the tree ended, once it has.
    pub fn last_finished(&self) -> Option<Status> {
        self.state.last_finished
    }
    /// Events dropped because the pending queue was full.
    pub fn dropped_events(&self) -> u64 {
        self.state.dropped_events
    }

    /// Queues a named event for the next tick's `on_event` transitions. Events no
    /// transition listens for are ignored. Returns whether it was queued.
    pub fn send_event(&mut self, name: &str) -> bool {
        if !self.tree.trigger_events.contains(name) {
            return false;
        }
        if self.state.events.len() >= MAX_PENDING_EVENTS {
            self.state.dropped_events += 1;
            return false;
        }
        self.state.events.push(name.to_owned());
        true
    }

    /// Advances the tree by `dt` seconds. A finished or stopped tree does nothing.
    pub fn tick(&mut self, dt: f32, host: &mut dyn StateTreeHost) -> TickReport {
        let tree = Arc::clone(&self.tree);
        let mut exec = Exec {
            tree: &tree,
            st: &mut self.state,
            host,
            dt,
            report: TickReport::default(),
        };
        if matches!(exec.st.phase, Phase::Finished(_) | Phase::Stopped) {
            exec.st.events.clear();
            return exec.report;
        }
        exec.st.ticks += 1;
        exec.st.time += f64::from(dt);
        exec.st.blackboard.take_changes();
        exec.run_evaluators();
        if exec.st.phase == Phase::Idle {
            exec.begin();
        } else {
            exec.tick_tasks();
        }
        exec.latch_results();
        exec.advance_timers();
        exec.transitions();
        exec.st.events.clear();
        exec.report
    }

    /// Exits every active state (child before parent), stops native evaluators and
    /// ends the tree without a result (agent disabled or removed). Returns the
    /// exited state ids.
    pub fn stop(&mut self, host: &mut dyn StateTreeHost) -> Vec<String> {
        let tree = Arc::clone(&self.tree);
        let mut exec = Exec {
            tree: &tree,
            st: &mut self.state,
            host,
            dt: 0.0,
            report: TickReport::default(),
        };
        if matches!(exec.st.phase, Phase::Idle | Phase::Running) {
            exec.exit_from(0);
            exec.stop_evaluators();
            exec.st.phase = Phase::Stopped;
        }
        exec.report.exited
    }

    /// State ids from the top-level state down to the active leaf.
    pub fn active_path(&self) -> Vec<String> {
        self.state
            .path
            .iter()
            .map(|&s| self.tree.states[s].id.clone())
            .collect()
    }

    /// Bounded debug view: phase, active path, per-state results and the recent
    /// transition log.
    pub fn debug(&self) -> StateTreeDebug {
        let tree = &*self.tree;
        let states = tree
            .states
            .iter()
            .zip(&self.state.states)
            .enumerate()
            .map(|(index, (def, rt))| StateDebug {
                id: def.id.clone(),
                active: self.state.path.contains(&index),
                result: rt.result,
            })
            .collect();
        let log = self
            .state
            .log
            .iter()
            .map(|entry| LogRecord {
                tick: entry.tick,
                event: entry.event,
                id: match entry.target {
                    LogTarget::State(s) => tree.states[s].id.clone(),
                    LogTarget::Transition(t) => tree.transitions[t].id.clone(),
                    LogTarget::Tree => String::new(),
                },
            })
            .collect();
        StateTreeDebug {
            ticks: self.state.ticks,
            time: self.state.time,
            phase: self.state.phase,
            active_path: self.active_path(),
            states,
            log,
        }
    }
}

/// Last known state of one state node.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StateDebug {
    pub id: String,
    pub active: bool,
    /// The completion latched by the state's own tasks while active.
    pub result: Option<Status>,
}

/// One entry of the bounded transition log.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LogRecord {
    pub tick: u64,
    pub event: LogEvent,
    /// State id for enter/exit, transition id for transitions, empty for the tree.
    pub id: String,
}

/// The bounded debug view of an agent.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StateTreeDebug {
    pub ticks: u64,
    /// Accumulated tree time in seconds.
    pub time: f64,
    pub phase: Phase,
    pub active_path: Vec<String>,
    /// Every state in depth-first order.
    pub states: Vec<StateDebug>,
    /// The last [`MAX_LOG`] entries, oldest first.
    pub log: Vec<LogRecord>,
}

/// A transition's effect, resolved against the current blackboard.
enum Plan {
    /// Keep `retained` leading states of the path, exit the rest, enter `enter`.
    Goto {
        retained: usize,
        enter: Vec<usize>,
    },
    Finish(Status),
}

struct Exec<'a> {
    tree: &'a StateTree,
    st: &'a mut State,
    host: &'a mut dyn StateTreeHost,
    dt: f32,
    report: TickReport,
}

fn done_of(status: Status) -> Option<Status> {
    (status != Status::Running).then_some(status)
}

impl Exec<'_> {
    fn log(&mut self, event: LogEvent, target: LogTarget) {
        if self.st.log.len() == MAX_LOG {
            self.st.log.pop_front();
        }
        self.st.log.push_back(LogEntry {
            tick: self.st.ticks,
            event,
            target,
        });
    }

    fn conds_pass(&self, conditions: &[Cond]) -> bool {
        conditions.iter().all(|c| evaluate(c, &self.st.blackboard))
    }

    fn write(&mut self, key: &str, value: Option<BbValue>) {
        let ok = match value {
            Some(value) => self.st.blackboard.set(key, value).is_ok(),
            None => false,
        };
        if !ok {
            // A missing or mistyped source clears the destination: derived keys
            // never go stale.
            let _ = self.st.blackboard.clear(key);
        }
    }

    fn run_evaluators(&mut self) {
        let tree = self.tree;
        let dt = self.dt;
        for (index, evaluator) in tree.evaluators.iter().enumerate() {
            match &evaluator.kind {
                EvalKind::Input { input, key } => {
                    let value = self.host.input(input);
                    self.write(key, value);
                }
                EvalKind::Copy { from, key } => {
                    let value = self.st.blackboard.get(from).cloned();
                    self.write(key, value);
                }
                EvalKind::Compute {
                    op,
                    a,
                    b,
                    key,
                    int_out,
                } => {
                    let value = compute(
                        *op,
                        self.st.blackboard.get(a),
                        b.as_deref().and_then(|b| self.st.blackboard.get(b)),
                        *int_out,
                    );
                    self.write(key, value);
                }
                EvalKind::Native(site) => {
                    let call = NativeCall {
                        site: site.site,
                        node: &evaluator.id,
                        name: &site.name,
                        params: &site.params,
                    };
                    if std::mem::replace(&mut self.st.evaluators_started[index], true) {
                        self.host.evaluator_tick(&call, &mut self.st.blackboard, dt);
                    } else {
                        self.host.evaluator_start(&call, &mut self.st.blackboard);
                    }
                }
            }
        }
    }

    fn stop_evaluators(&mut self) {
        let tree = self.tree;
        for (index, evaluator) in tree.evaluators.iter().enumerate() {
            if let EvalKind::Native(site) = &evaluator.kind
                && std::mem::replace(&mut self.st.evaluators_started[index], false)
            {
                let call = NativeCall {
                    site: site.site,
                    node: &evaluator.id,
                    name: &site.name,
                    params: &site.params,
                };
                self.host.evaluator_stop(&call, &mut self.st.blackboard);
            }
        }
    }

    /// Selects and enters the first selectable top-level state, or fails the tree.
    fn begin(&mut self) {
        self.st.phase = Phase::Running;
        let tree = self.tree;
        for &top in &tree.top {
            let mut chosen = Vec::new();
            if self.select(top, &mut chosen) {
                self.enter_all(&chosen);
                return;
            }
        }
        self.finish(Status::Failure);
    }

    /// Appends `state` and its selected descendants to `out`; false (leaving `out`
    /// as it was) when the state cannot be selected.
    fn select(&self, state: usize, out: &mut Vec<usize>) -> bool {
        let def = &self.tree.states[state];
        if !self.conds_pass(&def.enter) {
            return false;
        }
        out.push(state);
        if def.selection != Selection::SelfOnly {
            let mark = out.len();
            for &child in &def.children {
                if self.select(child, out) {
                    return true;
                }
                out.truncate(mark);
            }
            if !def.children.is_empty() && def.selection == Selection::ChildrenInOrder {
                out.pop();
                return false;
            }
        }
        true
    }

    fn call<'t>(
        &self,
        id: &'t str,
        site: &'t crate::behavior_tree::compile::NativeSite,
    ) -> NativeCall<'t> {
        NativeCall {
            site: site.site,
            node: id,
            name: &site.name,
            params: &site.params,
        }
    }

    fn enter_all(&mut self, states: &[usize]) {
        for &state in states {
            self.enter(state);
        }
    }

    /// Enters one state: it becomes active, then its tasks start in order.
    fn enter(&mut self, state: usize) {
        let tree = self.tree;
        let def = &tree.states[state];
        self.st.path.push(state);
        self.st.states[state] = StateRt {
            tasks: vec![TaskRt::default(); def.tasks.len()],
            result: None,
        };
        for &t in &def.transitions {
            let rt = &mut self.st.trans[t];
            rt.armed = false;
            rt.elapsed = 0.0;
        }
        self.log(LogEvent::Entered, LogTarget::State(state));
        self.report.entered.push(def.id.clone());
        for task in 0..def.tasks.len() {
            self.start_task(state, task);
        }
    }

    fn start_task(&mut self, state: usize, task: usize) {
        let tree = self.tree;
        let def = &tree.states[state].tasks[task];
        let done = match &def.kind {
            TaskKind::Wait { seconds } => (*seconds <= EPS).then_some(Status::Success),
            TaskKind::SetBlackboard { key, value } => {
                let ok = self.st.blackboard.set(key, value.clone()).is_ok();
                Some(if ok { Status::Success } else { Status::Failure })
            }
            TaskKind::ClearBlackboard { key } => {
                let ok = self.st.blackboard.clear(key).is_ok();
                Some(if ok { Status::Success } else { Status::Failure })
            }
            TaskKind::EmitEvent { event, payload } => {
                self.host.emit_event(event, payload);
                Some(Status::Success)
            }
            TaskKind::MoveTo { target, .. } => {
                let point = match target {
                    MoveTarget::Point(point) => Some(*point),
                    MoveTarget::Key(key) => match self.st.blackboard.get(key) {
                        Some(BbValue::Vec3(point)) => Some(*point),
                        _ => None,
                    },
                };
                match point {
                    Some(point) => {
                        self.host.request_move(point);
                        None
                    }
                    None => Some(Status::Failure),
                }
            }
            TaskKind::Native(site) => {
                let call = self.call(&def.id, site);
                done_of(self.host.native_start(&call, &mut self.st.blackboard))
            }
        };
        self.st.states[state].tasks[task] = TaskRt {
            started: true,
            done,
            elapsed: 0.0,
        };
    }

    fn tick_tasks(&mut self) {
        let tree = self.tree;
        for index in 0..self.st.path.len() {
            let state = self.st.path[index];
            for task in 0..tree.states[state].tasks.len() {
                self.tick_task(state, task);
            }
        }
    }

    fn tick_task(&mut self, state: usize, task: usize) {
        let tree = self.tree;
        let def = &tree.states[state].tasks[task];
        let rt = self.st.states[state].tasks[task];
        if !rt.started || rt.done.is_some() {
            return;
        }
        let dt = self.dt;
        let mut elapsed = rt.elapsed;
        let done = match &def.kind {
            TaskKind::Wait { seconds } => {
                elapsed += f64::from(dt);
                (elapsed + EPS >= *seconds).then_some(Status::Success)
            }
            TaskKind::MoveTo { timeout, .. } => {
                elapsed += f64::from(dt);
                match self.host.move_status() {
                    MoveStatus::Arrived => Some(Status::Success),
                    MoveStatus::Failed => Some(Status::Failure),
                    MoveStatus::Moving if *timeout > 0.0 && elapsed + EPS >= *timeout => {
                        self.host.cancel_move();
                        Some(Status::Failure)
                    }
                    MoveStatus::Moving => None,
                }
            }
            TaskKind::Native(site) => {
                let call = self.call(&def.id, site);
                done_of(self.host.native_tick(&call, &mut self.st.blackboard, dt))
            }
            _ => None,
        };
        let slot = &mut self.st.states[state].tasks[task];
        slot.elapsed = elapsed;
        slot.done = done;
    }

    /// Records the first completion each active state reaches; it stays until the
    /// state exits.
    fn latch_results(&mut self) {
        let tree = self.tree;
        for index in 0..self.st.path.len() {
            let state = self.st.path[index];
            let rt = &self.st.states[state];
            if rt.result.is_none() {
                let result = state_result(&tree.states[state], &rt.tasks);
                self.st.states[state].result = result;
            }
        }
    }

    fn advance_timers(&mut self) {
        let tree = self.tree;
        let dt = f64::from(self.dt);
        for &state in &self.st.path {
            for &t in &tree.states[state].transitions {
                let rt = &mut self.st.trans[t];
                if rt.armed {
                    rt.elapsed += dt;
                }
            }
        }
    }

    /// Depth in the path and result of the deepest completed active state.
    fn completion(&self) -> Option<(usize, Status)> {
        self.st
            .path
            .iter()
            .enumerate()
            .rev()
            .find_map(|(depth, &s)| self.st.states[s].result.map(|r| (depth, r)))
    }

    fn transitions(&mut self) {
        for _ in 0..MAX_TRANSITIONS_PER_TICK {
            if self.st.phase != Phase::Running {
                return;
            }
            self.report.passes += 1;
            match self.find() {
                Some((t, plan)) => self.apply(t, plan),
                None => {
                    self.finish_unhandled();
                    return;
                }
            }
        }
    }

    /// The first transition that fires now, with its resolved effect.
    fn find(&mut self) -> Option<(usize, Plan)> {
        let tree = self.tree;
        let mut candidates: Vec<usize> = self
            .st
            .path
            .iter()
            .flat_map(|&s| tree.states[s].transitions.iter().copied())
            .collect();
        candidates.sort_by_key(|&t| {
            let def = &tree.transitions[t];
            (
                Reverse(def.priority),
                Reverse(tree.states[def.owner].depth),
                t,
            )
        });
        let completion = self.completion();
        for t in candidates {
            let def = &tree.transitions[t];
            let depth = tree.states[def.owner].depth;
            let armed = self.st.trans[t].armed;
            let event_here = def
                .event
                .as_ref()
                .is_some_and(|e| self.st.events.contains(e));
            let triggered = match def.trigger {
                Trigger::Tick => true,
                Trigger::Completed => completion.is_some_and(|(d, _)| depth <= d),
                Trigger::Succeeded => {
                    completion.is_some_and(|(d, r)| depth <= d && r == Status::Success)
                }
                Trigger::Failed => {
                    completion.is_some_and(|(d, r)| depth <= d && r == Status::Failure)
                }
                Trigger::Event => armed || event_here,
            };
            if !triggered || !self.conds_pass(&def.conditions) {
                let rt = &mut self.st.trans[t];
                rt.armed = false;
                rt.elapsed = 0.0;
                continue;
            }
            if !armed {
                let rt = &mut self.st.trans[t];
                rt.delay = if def.variance > 0.0 {
                    let unit = (rt.rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
                    (def.delay + (unit * 2.0 - 1.0) * def.variance).max(0.0)
                } else {
                    def.delay
                };
                if rt.delay > EPS {
                    rt.armed = true;
                    rt.elapsed = 0.0;
                    if def.trigger == Trigger::Event {
                        self.consume_event(t);
                    }
                    continue;
                }
            } else if self.st.trans[t].elapsed + EPS < self.st.trans[t].delay {
                continue;
            }
            if let Some(plan) = self.resolve(t) {
                if def.trigger == Trigger::Event {
                    self.consume_event(t);
                }
                return Some((t, plan));
            }
        }
        None
    }

    fn consume_event(&mut self, t: usize) {
        if let Some(name) = &self.tree.transitions[t].event
            && let Some(at) = self.st.events.iter().position(|e| e == name)
        {
            self.st.events.remove(at);
        }
    }

    fn resolve(&self, t: usize) -> Option<Plan> {
        let tree = self.tree;
        let def = &tree.transitions[t];
        let owner = &tree.states[def.owner];
        match def.target {
            Target::Succeeded => Some(Plan::Finish(Status::Success)),
            Target::Failed => Some(Plan::Finish(Status::Failure)),
            Target::State(state) => self.goto(state),
            Target::Parent => owner.parent.and_then(|p| self.goto(p)),
            Target::NextSibling => {
                let siblings = match owner.parent {
                    Some(parent) => &tree.states[parent].children,
                    None => &tree.top,
                };
                let at = siblings.iter().position(|s| *s == def.owner)?;
                siblings[at + 1..].iter().find_map(|&s| self.goto(s))
            }
        }
    }

    /// Plans entering `target`: ancestors already active stay, the target and
    /// everything below are (re-)entered, every newly entered state's enter
    /// conditions must hold.
    fn goto(&self, target: usize) -> Option<Plan> {
        let tree = self.tree;
        let mut chain = vec![target];
        let mut at = target;
        while let Some(parent) = tree.states[at].parent {
            chain.push(parent);
            at = parent;
        }
        chain.reverse();
        let last = chain.len() - 1;
        let retained = (0..last)
            .take_while(|&i| self.st.path.get(i) == Some(&chain[i]))
            .count();
        let mut enter = Vec::new();
        for &ancestor in &chain[retained..last] {
            if !self.conds_pass(&tree.states[ancestor].enter) {
                return None;
            }
            enter.push(ancestor);
        }
        self.select(target, &mut enter)
            .then_some(Plan::Goto { retained, enter })
    }

    fn apply(&mut self, t: usize, plan: Plan) {
        let tree = self.tree;
        let def = &tree.transitions[t];
        let rt = &mut self.st.trans[t];
        rt.armed = false;
        rt.elapsed = 0.0;
        self.log(LogEvent::Transition, LogTarget::Transition(t));
        let to = match &plan {
            Plan::Goto { enter, .. } => enter.last().map(|s| tree.states[*s].id.clone()),
            Plan::Finish(_) => None,
        };
        self.report.transitions.push(TransitionRecord {
            transition: def.id.clone(),
            from: tree.states[def.owner].id.clone(),
            to,
            trigger: def.trigger.as_str(),
        });
        match plan {
            Plan::Goto { retained, enter } => {
                self.exit_from(retained);
                self.enter_all(&enter);
                self.latch_results();
            }
            Plan::Finish(status) => self.finish(status),
        }
    }

    /// Completes the tree when a state completed and no transition at or above it
    /// reacts to that completion (one that merely cannot fire yet still counts).
    fn finish_unhandled(&mut self) {
        let tree = self.tree;
        let Some((depth, result)) = self.completion() else {
            return;
        };
        let handled = self.st.path[..=depth].iter().any(|&s| {
            tree.states[s].transitions.iter().any(|&t| {
                matches!(
                    (tree.transitions[t].trigger, result),
                    (Trigger::Completed, _)
                        | (Trigger::Succeeded, Status::Success)
                        | (Trigger::Failed, Status::Failure)
                )
            })
        });
        if !handled {
            self.finish(result);
        }
    }

    /// Exits the active states deeper than `keep`, deepest first. Native tasks and
    /// moves still running are aborted.
    fn exit_from(&mut self, keep: usize) {
        let tree = self.tree;
        while self.st.path.len() > keep {
            let Some(state) = self.st.path.pop() else {
                break;
            };
            let def = &tree.states[state];
            for (index, task) in def.tasks.iter().enumerate() {
                let rt = self.st.states[state].tasks[index];
                if !rt.started || rt.done.is_some() {
                    continue;
                }
                match &task.kind {
                    TaskKind::Native(site) => {
                        let call = self.call(&task.id, site);
                        self.host.native_abort(&call, &mut self.st.blackboard);
                    }
                    TaskKind::MoveTo { .. } => self.host.cancel_move(),
                    _ => {}
                }
            }
            for &t in &def.transitions {
                let rt = &mut self.st.trans[t];
                rt.armed = false;
                rt.elapsed = 0.0;
            }
            self.st.states[state].result = None;
            self.log(LogEvent::Exited, LogTarget::State(state));
            self.report.exited.push(def.id.clone());
        }
    }

    fn finish(&mut self, status: Status) {
        self.exit_from(0);
        self.stop_evaluators();
        self.st.phase = Phase::Finished(status);
        self.st.last_finished = Some(status);
        self.log(LogEvent::Finished(status), LogTarget::Tree);
        self.report.finished = Some(status);
    }
}
