//! The tick executor: runs one agent's [`TreeInstance`] over a shared tree.
//!
//! Execution is a resumable depth-first walk. Every tick the walk restarts at the
//! root and descends through the nodes marked active, so a running branch keeps
//! its place without a stored call stack. Nothing here reads a clock or an RNG
//! that is not seeded by authored data: equal inputs and `dt` sequences give equal
//! outputs.
use super::{
    compile::{
        BehaviorTree, Condition, CooldownLock, MoveTarget, NativeSite, NodeKind, Operator, Policy,
        ServiceKind,
    },
    host::{Host, MoveStatus, NativeCall, Status},
};
use crate::{
    blackboard::{BbValue, Blackboard, BlackboardError},
    rng::SplitMix64,
};
use serde::Serialize;
use serde_json::Value;
use std::{
    cmp::Ordering,
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

/// Most entries kept in the transition/abort log.
pub const MAX_LOG: usize = 64;
/// Most traversals one tick performs while observer aborts keep firing.
pub const MAX_PASSES: u32 = 4;
/// Tolerance when comparing accumulated seconds with authored durations.
const EPS: f64 = 1e-6;

/// Why a running branch was interrupted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AbortReason {
    /// A condition with `self` aborts turned false.
    ObserverSelf,
    /// A higher priority condition turned true.
    ObserverLowerPriority,
    /// A time limit expired.
    TimeLimit,
    /// A parallel node decided while this child was still running.
    ParallelPolicy,
    /// An ancestor was aborted.
    ParentAborted,
    /// The whole tree was stopped (agent disabled or removed).
    TreeStopped,
}

impl AbortReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ObserverSelf => "observer_self",
            Self::ObserverLowerPriority => "observer_lower_priority",
            Self::TimeLimit => "time_limit",
            Self::ParallelPolicy => "parallel_policy",
            Self::ParentAborted => "parent_aborted",
            Self::TreeStopped => "tree_stopped",
        }
    }
}

/// An interruption worth telling the game about (descendants of an aborted
/// branch are not listed separately).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AbortRecord {
    pub node: String,
    pub reason: AbortReason,
    /// The blackboard key that triggered an observer abort.
    pub key: Option<String>,
}

/// What one tick did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TickReport {
    /// Set when the root completed during this tick; it restarts next tick.
    pub finished: Option<Status>,
    /// Traversals performed (more than one only after observer aborts).
    pub passes: u32,
    pub aborts: Vec<AbortRecord>,
}

/// What a log entry records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LogEvent {
    Started,
    Finished(Status),
    Aborted(AbortReason),
}

#[derive(Clone, Copy, Debug)]
struct LogEntry {
    tick: u64,
    node: usize,
    kind: LogEvent,
}

#[derive(Clone, Debug, Default)]
struct ServiceState {
    started: bool,
    elapsed: f64,
    last_tick: u64,
}

#[derive(Clone, Debug)]
struct NodeState {
    active: bool,
    last: Option<Status>,
    /// Per-activation flag: condition passed, cooldown checked, native/move started.
    started: bool,
    cursor: usize,
    order: Vec<usize>,
    results: Vec<Option<Status>>,
    counter: u32,
    elapsed: f64,
    /// A decorator may not restart its child before this tick.
    defer_until: u64,
    ticked: u64,
    locked_until: f64,
    /// Latest evaluation of an observer condition, to detect flips.
    condition: bool,
    abort_flag: bool,
    rng: SplitMix64,
    services: Vec<ServiceState>,
}

struct State {
    nodes: Vec<NodeState>,
    blackboard: Blackboard,
    time: f64,
    ticks: u64,
    log: VecDeque<LogEntry>,
    last_finished: Option<Status>,
}

/// One agent's running tree: per-node state plus its blackboard.
pub struct TreeInstance {
    tree: Arc<BehaviorTree>,
    state: State,
}

fn evaluate(condition: &Condition, blackboard: &Blackboard) -> bool {
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

impl TreeInstance {
    /// Creates an agent over `tree`. `overrides` set initial blackboard values by
    /// key; `seed` is mixed into every random selector so agents differ.
    ///
    /// # Errors
    /// An override naming an undeclared key or holding the wrong type.
    pub fn new(
        tree: Arc<BehaviorTree>,
        overrides: &BTreeMap<String, Value>,
        seed: u64,
    ) -> Result<Self, BlackboardError> {
        let mut blackboard = Blackboard::from_variables(tree.variables())?;
        for (key, value) in overrides {
            blackboard.set_json(key, value)?;
        }
        blackboard.take_changes();
        let nodes = tree
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                let authored = match node.kind {
                    NodeKind::RandomSelector { seed } => seed,
                    _ => 0,
                };
                let condition = match &node.kind {
                    NodeKind::Condition(c) => evaluate(c, &blackboard),
                    _ => false,
                };
                NodeState {
                    active: false,
                    last: None,
                    started: false,
                    cursor: 0,
                    order: Vec::new(),
                    results: Vec::new(),
                    counter: 0,
                    elapsed: 0.0,
                    defer_until: 0,
                    ticked: 0,
                    locked_until: 0.0,
                    condition,
                    abort_flag: false,
                    rng: SplitMix64::derive(authored ^ seed, index as u64),
                    services: vec![ServiceState::default(); node.services.len()],
                }
            })
            .collect();
        Ok(Self {
            tree,
            state: State {
                nodes,
                blackboard,
                time: 0.0,
                ticks: 0,
                log: VecDeque::new(),
                last_finished: None,
            },
        })
    }

    pub fn tree(&self) -> &Arc<BehaviorTree> {
        &self.tree
    }
    pub fn blackboard(&self) -> &Blackboard {
        &self.state.blackboard
    }
    /// Writes made here notify observers on the next [`tick`](Self::tick).
    pub fn blackboard_mut(&mut self) -> &mut Blackboard {
        &mut self.state.blackboard
    }
    pub fn ticks(&self) -> u64 {
        self.state.ticks
    }
    /// Outcome of the most recent complete run of the root.
    pub fn last_finished(&self) -> Option<Status> {
        self.state.last_finished
    }

    /// Advances the tree by `dt` seconds.
    ///
    /// Pending blackboard changes are first checked against observer conditions.
    /// After the walk, changes made during it are checked too; if they aborted
    /// something the walk repeats (with `dt` 0) so a preemption lands in the tick
    /// that caused it, at most [`MAX_PASSES`] times.
    pub fn tick(&mut self, dt: f32, host: &mut dyn Host) -> TickReport {
        let tree = Arc::clone(&self.tree);
        self.state.ticks += 1;
        self.state.time += f64::from(dt);
        let mut exec = Exec {
            tree: &tree,
            st: &mut self.state,
            host,
            dt,
            aborts: Vec::new(),
        };
        let mut report = TickReport::default();
        exec.process_observers();
        for pass in 0..MAX_PASSES {
            exec.dt = if pass == 0 { dt } else { 0.0 };
            report.passes += 1;
            let status = exec.exec(0);
            if status != Status::Running {
                report.finished = Some(status);
                exec.st.last_finished = Some(status);
            }
            if !exec.st.blackboard.has_changes() || exec.process_observers() == 0 {
                break;
            }
        }
        report.aborts = exec.aborts;
        report
    }

    /// Aborts everything that is running (agent disabled or removed).
    pub fn stop(&mut self, host: &mut dyn Host) -> Vec<AbortRecord> {
        let tree = Arc::clone(&self.tree);
        let mut exec = Exec {
            tree: &tree,
            st: &mut self.state,
            host,
            dt: 0.0,
            aborts: Vec::new(),
        };
        exec.abort_subtree(0, AbortReason::TreeStopped, None);
        exec.aborts
    }

    /// Node ids from the root down to the deepest running node; the first running
    /// child is followed where a parallel node has several. Cheap enough to call
    /// every frame.
    pub fn active_path(&self) -> Vec<String> {
        let tree = &*self.tree;
        let mut path = Vec::new();
        let mut current = 0usize;
        while self.state.nodes[current].active && path.len() <= super::kind::MAX_DEPTH {
            path.push(tree.nodes[current].id.clone());
            match tree.nodes[current]
                .children
                .iter()
                .find(|c| self.state.nodes[**c].active)
            {
                Some(next) => current = *next,
                None => break,
            }
        }
        path
    }

    /// Bounded debug view: per-node last status, the active path, counters and the
    /// recent transition and abort log.
    pub fn debug(&self) -> TreeDebug {
        let tree = &*self.tree;
        let nodes: Vec<NodeDebug> = tree
            .nodes
            .iter()
            .zip(&self.state.nodes)
            .map(|(node, state)| NodeDebug {
                id: node.id.clone(),
                node_type: node.node_type.clone(),
                running: state.active,
                last: state.last,
            })
            .collect();
        TreeDebug {
            ticks: self.state.ticks,
            time: self.state.time,
            last_finished: self.state.last_finished,
            active_path: self.active_path(),
            nodes,
            log: self
                .state
                .log
                .iter()
                .map(|entry| LogRecord {
                    tick: entry.tick,
                    node: tree.nodes[entry.node].id.clone(),
                    event: entry.kind,
                })
                .collect(),
        }
    }
}

/// Last known state of one node.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NodeDebug {
    pub id: String,
    pub node_type: String,
    /// Currently between start and completion.
    pub running: bool,
    /// Result of its latest completed run; absent if it never completed.
    pub last: Option<Status>,
}

/// One entry of the bounded transition log.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LogRecord {
    pub tick: u64,
    pub node: String,
    pub event: LogEvent,
}

/// The bounded debug view of an agent.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TreeDebug {
    pub ticks: u64,
    /// Accumulated tree time in seconds.
    pub time: f64,
    /// Outcome of the most recent complete run of the root.
    pub last_finished: Option<Status>,
    /// Node ids from the root down to the deepest running node (first running
    /// child where a parallel node has several).
    pub active_path: Vec<String>,
    /// Every node in depth-first order.
    pub nodes: Vec<NodeDebug>,
    /// The last [`MAX_LOG`] starts, completions and aborts, oldest first.
    pub log: Vec<LogRecord>,
}

struct Exec<'a> {
    tree: &'a BehaviorTree,
    st: &'a mut State,
    host: &'a mut dyn Host,
    dt: f32,
    aborts: Vec<AbortRecord>,
}

impl Exec<'_> {
    fn log(&mut self, node: usize, kind: LogEvent) {
        if self.st.log.len() == MAX_LOG {
            self.st.log.pop_front();
        }
        self.st.log.push_back(LogEntry {
            tick: self.st.ticks,
            node,
            kind,
        });
    }

    fn exec(&mut self, idx: usize) -> Status {
        if !self.st.nodes[idx].active {
            self.enter(idx);
        }
        let status = self.update(idx);
        if status != Status::Running {
            self.leave(idx, status);
        }
        status
    }

    fn enter(&mut self, idx: usize) {
        let node = &self.tree.nodes[idx];
        let state = &mut self.st.nodes[idx];
        state.active = true;
        state.started = false;
        state.cursor = 0;
        state.counter = 0;
        state.elapsed = 0.0;
        state.defer_until = 0;
        state.ticked = 0;
        state.abort_flag = false;
        for service in &mut state.services {
            *service = ServiceState::default();
        }
        match node.kind {
            NodeKind::Parallel { .. } => state.results = vec![None; node.children.len()],
            NodeKind::RandomSelector { .. } => {
                let mut order: Vec<usize> = (0..node.children.len()).collect();
                state.rng.shuffle(&mut order);
                state.order = order;
            }
            _ => {}
        }
        self.log(idx, LogEvent::Started);
    }

    fn leave(&mut self, idx: usize, status: Status) {
        self.deactivate_services(idx);
        let state = &mut self.st.nodes[idx];
        state.active = false;
        state.last = Some(status);
        self.log(idx, LogEvent::Finished(status));
    }

    /// Interrupts a running subtree, deepest nodes first, giving native tasks and
    /// services their `abort` callback.
    fn abort_subtree(&mut self, idx: usize, reason: AbortReason, key: Option<String>) {
        if !self.st.nodes[idx].active {
            return;
        }
        let tree = self.tree;
        if reason != AbortReason::ParentAborted {
            self.aborts.push(AbortRecord {
                node: tree.nodes[idx].id.clone(),
                reason,
                key,
            });
        }
        for &child in tree.nodes[idx].children.iter().rev() {
            self.abort_subtree(child, AbortReason::ParentAborted, None);
        }
        if self.st.nodes[idx].started {
            match &tree.nodes[idx].kind {
                NodeKind::Native(site) => self.native_abort(&tree.nodes[idx].id, site),
                NodeKind::MoveTo { .. } => self.host.cancel_move(),
                _ => {}
            }
        }
        self.deactivate_services(idx);
        let state = &mut self.st.nodes[idx];
        state.active = false;
        state.started = false;
        self.log(idx, LogEvent::Aborted(reason));
    }

    fn native_abort(&mut self, node: &str, site: &NativeSite) {
        self.host.native_abort(
            &NativeCall {
                site: site.site,
                node,
                name: &site.name,
                params: &site.params,
            },
            &mut self.st.blackboard,
        );
    }

    fn deactivate_services(&mut self, idx: usize) {
        let tree = self.tree;
        for (slot, service) in tree.nodes[idx].services.iter().enumerate() {
            if !self.st.nodes[idx].services[slot].started {
                continue;
            }
            self.st.nodes[idx].services[slot].started = false;
            if let ServiceKind::Native(site) = &service.kind {
                self.native_abort(&service.id, site);
            }
        }
    }

    /// Checks pending blackboard changes against observer conditions; returns how
    /// many aborts they caused.
    fn process_observers(&mut self) -> usize {
        let changed = self.st.blackboard.take_changes();
        let tree = self.tree;
        let mut count = 0;
        for key in changed {
            let Some(list) = tree.observers_by_key.get(&key) else {
                continue;
            };
            for &slot in list {
                let observer = &tree.observers[slot];
                let NodeKind::Condition(condition) = &tree.nodes[observer.node].kind else {
                    continue;
                };
                let now = evaluate(condition, &self.st.blackboard);
                let was = std::mem::replace(&mut self.st.nodes[observer.node].condition, now);
                let active = self.st.nodes[observer.node].active;
                if active && observer.abort.aborts_self() && !now {
                    self.st.nodes[observer.node].abort_flag = true;
                    self.aborts.push(AbortRecord {
                        node: tree.nodes[observer.node].id.clone(),
                        reason: AbortReason::ObserverSelf,
                        key: Some(key.clone()),
                    });
                    count += 1;
                } else if !active
                    && now
                    && !was
                    && observer.abort.aborts_lower()
                    && let Some((composite, branch)) = observer.priority
                {
                    let state = &self.st.nodes[composite];
                    if state.active && state.cursor > branch {
                        let running = tree.nodes[composite].children[state.cursor];
                        self.abort_subtree(
                            running,
                            AbortReason::ObserverLowerPriority,
                            Some(key.clone()),
                        );
                        self.st.nodes[composite].cursor = branch;
                        count += 1;
                    }
                }
            }
        }
        count
    }

    fn first_child(&mut self, idx: usize) -> Status {
        match self.tree.nodes[idx].children.first() {
            Some(&child) => self.exec(child),
            None => Status::Failure,
        }
    }

    fn update(&mut self, idx: usize) -> Status {
        let tree = self.tree;
        match &tree.nodes[idx].kind {
            NodeKind::Root => self.first_child(idx),
            NodeKind::Sequence => {
                self.tick_services(idx);
                self.sequence(idx)
            }
            NodeKind::Selector => {
                self.tick_services(idx);
                self.selector(idx, false)
            }
            NodeKind::RandomSelector { .. } => {
                self.tick_services(idx);
                self.selector(idx, true)
            }
            NodeKind::Parallel { success, failure } => {
                self.tick_services(idx);
                self.parallel(idx, *success, *failure)
            }
            NodeKind::Inverter => match self.first_child(idx) {
                Status::Success => Status::Failure,
                Status::Failure => Status::Success,
                Status::Running => Status::Running,
            },
            NodeKind::ForceSuccess => match self.first_child(idx) {
                Status::Running => Status::Running,
                _ => Status::Success,
            },
            NodeKind::ForceFailure => match self.first_child(idx) {
                Status::Running => Status::Running,
                _ => Status::Failure,
            },
            NodeKind::Repeat {
                count,
                stop_on_failure,
            } => self.repeat(idx, *count, *stop_on_failure),
            NodeKind::Retry { attempts } => self.retry(idx, *attempts),
            NodeKind::Cooldown { duration, lock_on } => self.cooldown(idx, *duration, *lock_on),
            NodeKind::TimeLimit { seconds } => self.time_limit(idx, *seconds),
            NodeKind::Condition(_) => self.condition(idx),
            NodeKind::Wait { seconds } => {
                let state = &mut self.st.nodes[idx];
                if state.ticked != self.st.ticks {
                    state.ticked = self.st.ticks;
                    state.elapsed += f64::from(self.dt);
                }
                if state.elapsed + EPS >= *seconds {
                    Status::Success
                } else {
                    Status::Running
                }
            }
            NodeKind::SetBlackboard { key, value } => {
                match self.st.blackboard.set(key, value.clone()) {
                    Ok(_) => Status::Success,
                    Err(_) => Status::Failure,
                }
            }
            NodeKind::ClearBlackboard { key } => match self.st.blackboard.clear(key) {
                Ok(_) => Status::Success,
                Err(_) => Status::Failure,
            },
            NodeKind::EmitEvent { event, payload } => {
                self.host.emit_event(event, payload);
                Status::Success
            }
            NodeKind::MoveTo { target, timeout } => self.move_to(idx, target, *timeout),
            NodeKind::Native(site) => self.native(idx, site),
        }
    }

    fn sequence(&mut self, idx: usize) -> Status {
        let children = &self.tree.nodes[idx].children;
        loop {
            let cursor = self.st.nodes[idx].cursor;
            let Some(&child) = children.get(cursor) else {
                return Status::Success;
            };
            match self.exec(child) {
                Status::Running => return Status::Running,
                Status::Failure => return Status::Failure,
                Status::Success => self.st.nodes[idx].cursor += 1,
            }
        }
    }

    fn selector(&mut self, idx: usize, random: bool) -> Status {
        let children = &self.tree.nodes[idx].children;
        loop {
            let cursor = self.st.nodes[idx].cursor;
            let slot = if random {
                self.st.nodes[idx].order.get(cursor).copied()
            } else {
                Some(cursor)
            };
            let Some(&child) = slot.and_then(|slot| children.get(slot)) else {
                return Status::Failure;
            };
            match self.exec(child) {
                Status::Running => return Status::Running,
                Status::Success => return Status::Success,
                Status::Failure => self.st.nodes[idx].cursor += 1,
            }
        }
    }

    fn parallel(&mut self, idx: usize, success: Policy, failure: Policy) -> Status {
        let children = &self.tree.nodes[idx].children;
        let total = children.len();
        let mut results = std::mem::take(&mut self.st.nodes[idx].results);
        for (slot, &child) in children.iter().enumerate() {
            if results[slot].is_none() {
                let status = self.exec(child);
                if status != Status::Running {
                    results[slot] = Some(status);
                }
            }
        }
        let succeeded = results
            .iter()
            .filter(|r| **r == Some(Status::Success))
            .count();
        let failed = results
            .iter()
            .filter(|r| **r == Some(Status::Failure))
            .count();
        let failure_hit = match failure {
            Policy::RequireOne => failed >= 1,
            Policy::RequireAll => failed == total,
        };
        let success_hit = match success {
            Policy::RequireOne => succeeded >= 1,
            Policy::RequireAll => succeeded == total,
        };
        let outcome = if failure_hit {
            Some(Status::Failure)
        } else if success_hit {
            Some(Status::Success)
        } else if succeeded + failed == total {
            Some(Status::Failure)
        } else {
            None
        };
        if outcome.is_some() {
            for (slot, &child) in children.iter().enumerate() {
                if results[slot].is_none() {
                    self.abort_subtree(child, AbortReason::ParallelPolicy, None);
                }
            }
        }
        self.st.nodes[idx].results = results;
        outcome.unwrap_or(Status::Running)
    }

    /// Whether a restarting decorator must wait for a later tick.
    fn deferred(&self, idx: usize) -> bool {
        self.st.ticks < self.st.nodes[idx].defer_until
    }

    fn repeat(&mut self, idx: usize, count: u32, stop_on_failure: bool) -> Status {
        if self.deferred(idx) {
            return Status::Running;
        }
        match self.first_child(idx) {
            Status::Running => Status::Running,
            Status::Failure if stop_on_failure => Status::Failure,
            _ => {
                let ticks = self.st.ticks;
                let state = &mut self.st.nodes[idx];
                state.counter += 1;
                if state.counter >= count {
                    Status::Success
                } else {
                    state.defer_until = ticks + 1;
                    Status::Running
                }
            }
        }
    }

    fn retry(&mut self, idx: usize, attempts: u32) -> Status {
        if self.deferred(idx) {
            return Status::Running;
        }
        match self.first_child(idx) {
            Status::Running => Status::Running,
            Status::Success => Status::Success,
            Status::Failure => {
                let ticks = self.st.ticks;
                let state = &mut self.st.nodes[idx];
                state.counter += 1;
                if state.counter >= attempts {
                    Status::Failure
                } else {
                    state.defer_until = ticks + 1;
                    Status::Running
                }
            }
        }
    }

    fn cooldown(&mut self, idx: usize, duration: f64, lock_on: CooldownLock) -> Status {
        let time = self.st.time;
        if !self.st.nodes[idx].started {
            self.st.nodes[idx].started = true;
            if time + EPS < self.st.nodes[idx].locked_until {
                return Status::Failure;
            }
        }
        let status = self.first_child(idx);
        let locks = match (status, lock_on) {
            (Status::Running, _) => false,
            (_, CooldownLock::Any) => true,
            (Status::Success, CooldownLock::Success) | (Status::Failure, CooldownLock::Failure) => {
                true
            }
            _ => false,
        };
        if locks {
            self.st.nodes[idx].locked_until = time + duration;
        }
        status
    }

    fn time_limit(&mut self, idx: usize, seconds: f64) -> Status {
        if self.st.nodes[idx].elapsed + EPS >= seconds {
            if let Some(&child) = self.tree.nodes[idx].children.first() {
                self.abort_subtree(child, AbortReason::TimeLimit, None);
            }
            return Status::Failure;
        }
        let status = self.first_child(idx);
        let state = &mut self.st.nodes[idx];
        if state.ticked != self.st.ticks {
            state.ticked = self.st.ticks;
            state.elapsed += f64::from(self.dt);
        }
        status
    }

    fn condition(&mut self, idx: usize) -> Status {
        let tree = self.tree;
        let NodeKind::Condition(condition) = &tree.nodes[idx].kind else {
            return Status::Failure;
        };
        if self.st.nodes[idx].abort_flag {
            self.st.nodes[idx].abort_flag = false;
            if let Some(&child) = tree.nodes[idx].children.first() {
                self.abort_subtree(child, AbortReason::ParentAborted, None);
            }
            return Status::Failure;
        }
        if !self.st.nodes[idx].started {
            self.st.nodes[idx].started = true;
            let holds = evaluate(condition, &self.st.blackboard);
            self.st.nodes[idx].condition = holds;
            if !holds {
                return Status::Failure;
            }
        }
        self.first_child(idx)
    }

    fn move_to(&mut self, idx: usize, target: &MoveTarget, timeout: f64) -> Status {
        if !self.st.nodes[idx].started {
            let point = match target {
                MoveTarget::Point(point) => *point,
                MoveTarget::Key(key) => match self.st.blackboard.get(key) {
                    Some(BbValue::Vec3(point)) => *point,
                    _ => return Status::Failure,
                },
            };
            self.st.nodes[idx].started = true;
            self.host.request_move(point);
        }
        let state = &mut self.st.nodes[idx];
        if state.ticked != self.st.ticks {
            state.ticked = self.st.ticks;
            state.elapsed += f64::from(self.dt);
        }
        let elapsed = state.elapsed;
        match self.host.move_status() {
            MoveStatus::Arrived => Status::Success,
            MoveStatus::Failed => Status::Failure,
            MoveStatus::Moving if timeout > 0.0 && elapsed + EPS >= timeout => {
                self.host.cancel_move();
                Status::Failure
            }
            MoveStatus::Moving => Status::Running,
        }
    }

    fn native(&mut self, idx: usize, site: &NativeSite) -> Status {
        let tree = self.tree;
        let call = NativeCall {
            site: site.site,
            node: &tree.nodes[idx].id,
            name: &site.name,
            params: &site.params,
        };
        if !self.st.nodes[idx].started {
            self.st.nodes[idx].started = true;
            self.st.nodes[idx].ticked = self.st.ticks;
            return self.host.native_start(&call, &mut self.st.blackboard);
        }
        if self.st.nodes[idx].ticked == self.st.ticks {
            return Status::Running;
        }
        self.st.nodes[idx].ticked = self.st.ticks;
        self.host
            .native_tick(&call, &mut self.st.blackboard, self.dt)
    }

    /// Runs the services of a composite that are due. A service runs when its
    /// branch activates and then whenever `interval` seconds have accumulated;
    /// at most once per tick.
    fn tick_services(&mut self, idx: usize) {
        let tree = self.tree;
        for (slot, service) in tree.nodes[idx].services.iter().enumerate() {
            let ticks = self.st.ticks;
            let state = &mut self.st.nodes[idx].services[slot];
            if state.last_tick == ticks {
                continue;
            }
            state.last_tick = ticks;
            state.elapsed += f64::from(self.dt);
            let first = !state.started;
            if !first && state.elapsed + EPS < service.interval {
                continue;
            }
            state.started = true;
            state.elapsed = if first {
                0.0
            } else {
                (state.elapsed - service.interval).clamp(0.0, service.interval)
            };
            match &service.kind {
                ServiceKind::Native(site) => {
                    let call = NativeCall {
                        site: site.site,
                        node: &service.id,
                        name: &site.name,
                        params: &site.params,
                    };
                    if first {
                        self.host.native_start(&call, &mut self.st.blackboard);
                    } else {
                        self.host.native_tick(
                            &call,
                            &mut self.st.blackboard,
                            service.interval as f32,
                        );
                    }
                }
                ServiceKind::SetBlackboard { key, value } => {
                    let _ = self.st.blackboard.set(key, value.clone());
                }
                ServiceKind::EmitEvent { event, payload } => {
                    self.host.emit_event(event, payload);
                }
            }
        }
    }
}
