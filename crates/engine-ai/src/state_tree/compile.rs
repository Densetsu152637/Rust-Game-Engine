//! Compiles a validated `state_tree` [`GraphDocument`] into an immutable tree.
//!
//! The compiled [`StateTree`] is plain shared data; every agent runs its own
//! [`StateTreeInstance`](super::StateTreeInstance) over one `Arc<StateTree>`.
use super::kind::{
    self, MAX_CHILDREN, MAX_CONDITIONS, MAX_DEPTH, MAX_EVALUATORS, MAX_TASKS, MAX_TRANSITIONS,
    node, port,
};
use crate::{
    behavior_tree::compile::{NativeSite, Operator, Params, valid_event_name, valid_native_name},
    blackboard::BbValue,
};
use engine_graph::{
    GraphDiagnostic, GraphDocument, GraphIndex, GraphVariable, PortType, Strictness, codes,
    validate,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Diagnostic codes added by the state tree compiler.
pub mod st_codes {
    pub const ROOT_EMPTY: &str = "st.root.empty";
    pub const EVALUATORS_LIMIT: &str = "st.root.evaluators";
    pub const CHILDREN_LIMIT: &str = "st.state.children";
    pub const TASKS_LIMIT: &str = "st.state.tasks";
    pub const TRANSITIONS_LIMIT: &str = "st.state.transitions";
    pub const CONDITIONS_LIMIT: &str = "st.conditions.limit";
    pub const DEPTH: &str = "st.tree.depth";
    pub const UNREACHABLE: &str = "st.state.unreachable";
    pub const TARGET_UNKNOWN: &str = "st.transition.target_unknown";
    pub const TARGET_INVALID: &str = "st.transition.target";
    pub const TRIGGER_EVENT: &str = "st.transition.event";
    pub const CONDITION_VALUE: &str = "st.condition.value";
    pub const CONDITION_OPERATOR: &str = "st.condition.operator";
    pub const VALUE_TYPE: &str = "st.value.type";
    pub const MOVE_TARGET: &str = "st.move_to.target";
    pub const EVENT_NAME: &str = "st.event.name";
    pub const EVENT_PAYLOAD: &str = "st.event.payload";
    pub const NATIVE_NAME: &str = "st.native.name";
    pub const EVALUATOR: &str = "st.evaluator.invalid";
    /// Every code above.
    pub const ALL: &[&str] = &[
        ROOT_EMPTY,
        EVALUATORS_LIMIT,
        CHILDREN_LIMIT,
        TASKS_LIMIT,
        TRANSITIONS_LIMIT,
        CONDITIONS_LIMIT,
        DEPTH,
        UNREACHABLE,
        TARGET_UNKNOWN,
        TARGET_INVALID,
        TRIGGER_EVENT,
        CONDITION_VALUE,
        CONDITION_OPERATOR,
        VALUE_TYPE,
        MOVE_TARGET,
        EVENT_NAME,
        EVENT_PAYLOAD,
        NATIVE_NAME,
        EVALUATOR,
    ];
}

/// How a state with children picks its active descendants on entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection {
    /// The first selectable child; a state with children is selectable only
    /// through one of them.
    ChildrenInOrder,
    /// The first selectable child, else the state itself as the leaf.
    ChildrenOrSelf,
    /// The state itself; children are only entered by transitions.
    SelfOnly,
}

/// When a state's own tasks complete the state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Completion {
    Any,
    All,
}

/// What makes a transition a candidate to fire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// The state (or a state below it) completed, either way.
    Completed,
    Succeeded,
    Failed,
    /// Every tick.
    Tick,
    /// A named event arrived.
    Event,
}

impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "on_completed",
            Self::Succeeded => "on_succeeded",
            Self::Failed => "on_failed",
            Self::Tick => "on_tick",
            Self::Event => "on_event",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    State(usize),
    Parent,
    NextSibling,
    Succeeded,
    Failed,
}

#[derive(Clone, Debug)]
pub(crate) struct Cond {
    pub key: String,
    pub op: Operator,
    pub value: Option<BbValue>,
}

#[derive(Clone, Debug)]
pub(crate) enum MoveTarget {
    Point([f32; 3]),
    Key(String),
}

#[derive(Clone, Debug)]
pub(crate) enum TaskKind {
    Wait { seconds: f64 },
    SetBlackboard { key: String, value: BbValue },
    ClearBlackboard { key: String },
    EmitEvent { event: String, payload: Value },
    MoveTo { target: MoveTarget, timeout: f64 },
    Native(NativeSite),
}

#[derive(Clone, Debug)]
pub(crate) struct TaskDef {
    pub id: String,
    pub considered: bool,
    pub kind: TaskKind,
}

#[derive(Clone, Debug)]
pub(crate) struct StateDef {
    pub id: String,
    pub parent: Option<usize>,
    /// Ancestors above the state; also its index in the active path.
    pub depth: usize,
    pub children: Vec<usize>,
    pub selection: Selection,
    pub completion: Completion,
    pub enter: Vec<Cond>,
    pub tasks: Vec<TaskDef>,
    /// Indexes into [`StateTree::transitions`], in authoring order.
    pub transitions: Vec<usize>,
}

#[derive(Clone, Debug)]
pub(crate) struct TransitionDef {
    pub id: String,
    pub owner: usize,
    pub trigger: Trigger,
    pub event: Option<String>,
    pub target: Target,
    /// Target state id before resolution.
    pub state_ref: Option<String>,
    pub priority: u32,
    pub delay: f64,
    pub variance: f64,
    pub conditions: Vec<Cond>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ComputeOp {
    Add,
    Sub,
    Mul,
    Min,
    Max,
    Distance,
    Not,
    And,
    Or,
}

#[derive(Clone, Debug)]
pub(crate) enum EvalKind {
    Input {
        input: String,
        key: String,
    },
    Copy {
        from: String,
        key: String,
    },
    Compute {
        op: ComputeOp,
        a: String,
        b: Option<String>,
        key: String,
        /// The destination holds an `Int`.
        int_out: bool,
    },
    Native(NativeSite),
}

#[derive(Clone, Debug)]
pub(crate) struct EvaluatorDef {
    pub id: String,
    pub kind: EvalKind,
}

/// An immutable compiled state tree.
#[derive(Clone, Debug)]
pub struct StateTree {
    pub(crate) states: Vec<StateDef>,
    pub(crate) top: Vec<usize>,
    pub(crate) transitions: Vec<TransitionDef>,
    pub(crate) evaluators: Vec<EvaluatorDef>,
    pub(crate) variables: Vec<GraphVariable>,
    pub(crate) trigger_events: BTreeSet<String>,
    native_sites: usize,
    task_names: BTreeSet<String>,
    evaluator_names: BTreeSet<String>,
    input_names: BTreeSet<String>,
    uses_move_to: bool,
}

impl StateTree {
    /// States in depth-first order (parents before children).
    pub fn state_count(&self) -> usize {
        self.states.len()
    }
    /// Graph node id of the state at `index`.
    pub fn state_id(&self, index: usize) -> Option<&str> {
        self.states.get(index).map(|s| s.id.as_str())
    }
    /// Graph node id of the first state in depth-first order with this id.
    pub fn state_index(&self, id: &str) -> Option<usize> {
        self.states.iter().position(|s| s.id == id)
    }
    pub fn transition_count(&self) -> usize {
        self.transitions.len()
    }
    /// The document variables, which declare the blackboard keys.
    pub fn variables(&self) -> &[GraphVariable] {
        &self.variables
    }
    /// Names of every native task the tree calls.
    pub fn native_task_names(&self) -> &BTreeSet<String> {
        &self.task_names
    }
    /// Names of every native evaluator the tree calls.
    pub fn native_evaluator_names(&self) -> &BTreeSet<String> {
        &self.evaluator_names
    }
    /// Host input names the `copy_evaluator` nodes read.
    pub fn input_names(&self) -> &BTreeSet<String> {
        &self.input_names
    }
    /// Event names that `on_event` transitions react to.
    pub fn trigger_events(&self) -> &BTreeSet<String> {
        &self.trigger_events
    }
    /// Count of native call sites (tasks plus evaluators).
    pub fn native_sites(&self) -> usize {
        self.native_sites
    }
    /// Whether any `move_to` task exists, so hosts can require navigation.
    pub fn uses_move_to(&self) -> bool {
        self.uses_move_to
    }
}

/// Validates `doc` completely, then compiles it.
///
/// # Errors
/// Returns every graph and state tree diagnostic; nothing compiles on error.
pub fn compile(doc: &GraphDocument) -> Result<StateTree, Vec<GraphDiagnostic>> {
    let diagnostics = validate(doc, kind::shared_kind(), Strictness::Complete);
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }
    build(doc)
}

/// The kind's semantic check: compile and report what the generic rules cannot.
pub(crate) fn check_document(doc: &GraphDocument) -> Vec<GraphDiagnostic> {
    build(doc).err().unwrap_or_default()
}

fn build(doc: &GraphDocument) -> Result<StateTree, Vec<GraphDiagnostic>> {
    let mut builder = Builder {
        doc,
        index: GraphIndex::new(doc),
        states: Vec::new(),
        transitions: Vec::new(),
        top: Vec::new(),
        evaluators: Vec::new(),
        diagnostics: Vec::new(),
        native_sites: 0,
        task_names: BTreeSet::new(),
        evaluator_names: BTreeSet::new(),
        input_names: BTreeSet::new(),
        trigger_events: BTreeSet::new(),
        uses_move_to: false,
    };
    let root = doc
        .nodes
        .iter()
        .find(|n| n.node_type == node::ROOT)
        .map(|n| n.id.as_str().to_owned());
    match root {
        Some(root) => builder.visit_root(&root),
        None => builder.error(codes::ROOT_MISSING, "", "the tree has no root node"),
    }
    builder.finish()
}

struct Builder<'a> {
    doc: &'a GraphDocument,
    index: GraphIndex<'a>,
    states: Vec<StateDef>,
    transitions: Vec<TransitionDef>,
    top: Vec<usize>,
    evaluators: Vec<EvaluatorDef>,
    diagnostics: Vec<GraphDiagnostic>,
    native_sites: usize,
    task_names: BTreeSet<String>,
    evaluator_names: BTreeSet<String>,
    input_names: BTreeSet<String>,
    trigger_events: BTreeSet<String>,
    uses_move_to: bool,
}

fn params_of<'n>(doc_node: &'n engine_graph::GraphNode) -> Option<Params<'n>> {
    let def = kind::shared_kind().registry.get(&doc_node.node_type)?;
    Some(Params {
        values: def.effective_params(doc_node),
        id: doc_node.id.as_str(),
    })
}

fn numeric(ty: &PortType) -> bool {
    matches!(ty, PortType::Int | PortType::Float)
}

impl<'a> Builder<'a> {
    fn error(&mut self, code: &str, path: impl Into<String>, message: impl Into<String>) {
        self.diagnostics
            .push(GraphDiagnostic::new(code, path, message));
    }

    fn key_type(&self, key: &str) -> Option<PortType> {
        self.doc.variable(key).map(|v| v.ty.clone())
    }

    fn keep<T>(&mut self, result: Result<T, GraphDiagnostic>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(diagnostic) => {
                self.diagnostics.push(diagnostic);
                None
            }
        }
    }

    fn visit_root(&mut self, root: &str) {
        let evaluators = self.index.targets(root, port::EVALUATORS);
        if evaluators.len() > MAX_EVALUATORS {
            self.error(
                st_codes::EVALUATORS_LIMIT,
                format!("/nodes/{root}"),
                format!("more than {MAX_EVALUATORS} evaluators"),
            );
        }
        for id in evaluators {
            if let Some(evaluator) = self.evaluator(id) {
                self.evaluators.push(evaluator);
            }
        }
        let children = self.index.targets(root, port::CHILDREN);
        if children.is_empty() {
            self.error(
                st_codes::ROOT_EMPTY,
                format!("/nodes/{root}"),
                "the tree needs at least one top-level state",
            );
        }
        if children.len() > MAX_CHILDREN {
            self.error(
                st_codes::CHILDREN_LIMIT,
                format!("/nodes/{root}"),
                format!("more than {MAX_CHILDREN} top-level states"),
            );
        }
        for id in children {
            if let Some(index) = self.visit_state(id, None, 0) {
                self.top.push(index);
            }
        }
    }

    /// Depth-first, pre-order, children in edge order. Returns the state index.
    fn visit_state(&mut self, id: &str, parent: Option<usize>, depth: usize) -> Option<usize> {
        if depth >= MAX_DEPTH {
            self.error(
                st_codes::DEPTH,
                format!("/nodes/{id}"),
                format!("state nesting exceeds {MAX_DEPTH} levels"),
            );
            return None;
        }
        let node = self.index.node(id)?;
        let p = params_of(node)?;
        let selection = self.keep(p.str("selection").map(|s| match s {
            "children_or_self" => Selection::ChildrenOrSelf,
            "self" => Selection::SelfOnly,
            _ => Selection::ChildrenInOrder,
        }))?;
        let completion = self.keep(p.str("completion").map(|s| match s {
            "all" => Completion::All,
            _ => Completion::Any,
        }))?;
        let enter = self.conditions(id);
        let tasks = self.tasks(id);
        let index = self.states.len();
        self.states.push(StateDef {
            id: id.to_owned(),
            parent,
            depth,
            children: Vec::new(),
            selection,
            completion,
            enter,
            tasks,
            transitions: Vec::new(),
        });
        let transitions = self.index.targets(id, port::TRANSITIONS);
        if transitions.len() > MAX_TRANSITIONS {
            self.error(
                st_codes::TRANSITIONS_LIMIT,
                format!("/nodes/{id}"),
                format!("more than {MAX_TRANSITIONS} transitions"),
            );
        }
        for transition in transitions {
            if let Some(def) = self.transition(transition, index) {
                let slot = self.transitions.len();
                self.transitions.push(def);
                self.states[index].transitions.push(slot);
            }
        }
        let targets = self.index.targets(id, port::CHILDREN);
        if targets.len() > MAX_CHILDREN {
            self.error(
                st_codes::CHILDREN_LIMIT,
                format!("/nodes/{id}"),
                format!("more than {MAX_CHILDREN} child states"),
            );
        }
        let mut children = Vec::with_capacity(targets.len());
        for target in targets {
            if let Some(child) = self.visit_state(target, Some(index), depth + 1) {
                children.push(child);
            }
        }
        self.states[index].children = children;
        Some(index)
    }

    fn conditions(&mut self, owner: &str) -> Vec<Cond> {
        let ids = self.index.targets(owner, port::CONDITIONS);
        if ids.len() > MAX_CONDITIONS {
            self.error(
                st_codes::CONDITIONS_LIMIT,
                format!("/nodes/{owner}"),
                format!("more than {MAX_CONDITIONS} conditions"),
            );
        }
        let mut out = Vec::new();
        for id in ids {
            let Some(node) = self.index.node(id) else {
                continue;
            };
            let Some(p) = params_of(node) else {
                continue;
            };
            let parsed = self.condition(&p);
            if let Some(condition) = self.keep(parsed) {
                out.push(condition);
            }
        }
        out
    }

    fn condition(&self, p: &Params<'_>) -> Result<Cond, GraphDiagnostic> {
        let key = p.str("key")?.to_owned();
        let ty = self.key_type(&key).ok_or_else(|| {
            GraphDiagnostic::new(codes::PARAM_VARIABLE, p.path("key"), "unknown variable")
        })?;
        let op = match p.str("operator")? {
            "is_set" => Operator::IsSet,
            "is_not_set" => Operator::IsNotSet,
            "eq" => Operator::Eq,
            "ne" => Operator::Ne,
            "lt" => Operator::Lt,
            "le" => Operator::Le,
            "gt" => Operator::Gt,
            _ => Operator::Ge,
        };
        let value = if matches!(op, Operator::IsSet | Operator::IsNotSet) {
            None
        } else {
            if matches!(
                op,
                Operator::Lt | Operator::Le | Operator::Gt | Operator::Ge
            ) && !numeric(&ty)
            {
                return Err(GraphDiagnostic::new(
                    st_codes::CONDITION_OPERATOR,
                    p.path("operator"),
                    format!("ordering needs an int or float key, {key:?} is {ty:?}"),
                ));
            }
            let raw = p.json("value").ok_or_else(|| {
                GraphDiagnostic::new(
                    st_codes::CONDITION_VALUE,
                    p.path("value"),
                    "this operator compares against a value",
                )
            })?;
            Some(BbValue::from_json(&ty, raw).ok_or_else(|| {
                GraphDiagnostic::new(
                    st_codes::CONDITION_VALUE,
                    p.path("value"),
                    format!("value is not a {ty:?}"),
                )
            })?)
        };
        Ok(Cond { key, op, value })
    }

    fn tasks(&mut self, owner: &str) -> Vec<TaskDef> {
        let ids = self.index.targets(owner, port::TASKS);
        if ids.len() > MAX_TASKS {
            self.error(
                st_codes::TASKS_LIMIT,
                format!("/nodes/{owner}"),
                format!("more than {MAX_TASKS} tasks"),
            );
        }
        let mut out = Vec::new();
        for id in ids {
            let Some(node) = self.index.node(id) else {
                continue;
            };
            let Some(p) = params_of(node) else {
                continue;
            };
            let parsed = self.task(node.node_type.as_str(), &p);
            if let Some(task) = self.keep(parsed) {
                out.push(task);
            }
        }
        out
    }

    fn task(&mut self, node_type: &str, p: &Params<'_>) -> Result<TaskDef, GraphDiagnostic> {
        let kind = match node_type {
            node::WAIT => TaskKind::Wait {
                seconds: p.f64("seconds")?,
            },
            node::SET_BLACKBOARD => {
                let key = p.str("key")?.to_owned();
                let value = self.constant(p, &key)?;
                TaskKind::SetBlackboard { key, value }
            }
            node::CLEAR_BLACKBOARD => TaskKind::ClearBlackboard {
                key: p.str("key")?.to_owned(),
            },
            node::EMIT_EVENT => {
                let (event, payload) = self.event(p)?;
                TaskKind::EmitEvent { event, payload }
            }
            node::MOVE_TO => {
                let target = self.move_target(p)?;
                self.uses_move_to = true;
                TaskKind::MoveTo {
                    target,
                    timeout: p.f64("timeout")?,
                }
            }
            node::NATIVE_TASK => {
                let site = self.native(p)?;
                self.task_names.insert(site.name.clone());
                TaskKind::Native(site)
            }
            other => {
                return Err(GraphDiagnostic::new(
                    codes::NODE_UNKNOWN_TYPE,
                    format!("/nodes/{}", p.id),
                    format!("{other:?} is not a state tree task type"),
                ));
            }
        };
        Ok(TaskDef {
            id: p.id.to_owned(),
            considered: p.bool("considered")?,
            kind,
        })
    }

    fn constant(&self, p: &Params<'_>, key: &str) -> Result<BbValue, GraphDiagnostic> {
        let ty = self.key_type(key).ok_or_else(|| {
            GraphDiagnostic::new(codes::PARAM_VARIABLE, p.path("key"), "unknown variable")
        })?;
        let value = p.json("value").ok_or_else(|| p.missing("value"))?;
        BbValue::from_json(&ty, value).ok_or_else(|| {
            GraphDiagnostic::new(
                st_codes::VALUE_TYPE,
                p.path("value"),
                format!("value is not a {ty:?}"),
            )
        })
    }

    fn event(&self, p: &Params<'_>) -> Result<(String, Value), GraphDiagnostic> {
        let event = p.str("event")?;
        if !valid_event_name(event) {
            return Err(GraphDiagnostic::new(
                st_codes::EVENT_NAME,
                p.path("event"),
                "event names are 1-128 characters without control characters",
            ));
        }
        let payload = p.json("payload").cloned().unwrap_or(Value::Null);
        if !(payload.is_object() || payload.is_null()) {
            return Err(GraphDiagnostic::new(
                st_codes::EVENT_PAYLOAD,
                p.path("payload"),
                "payload must be a JSON object",
            ));
        }
        Ok((event.to_owned(), payload))
    }

    fn move_target(&self, p: &Params<'_>) -> Result<MoveTarget, GraphDiagnostic> {
        let literal = p.json("target");
        let key = p.json("key").and_then(Value::as_str);
        match (literal, key) {
            (Some(value), None) => match BbValue::from_json(&PortType::Vec3, value) {
                Some(BbValue::Vec3(v)) => Ok(MoveTarget::Point(v)),
                _ => Err(GraphDiagnostic::new(
                    st_codes::MOVE_TARGET,
                    p.path("target"),
                    "target must be [x, y, z]",
                )),
            },
            (None, Some(key)) => {
                if self.key_type(key) == Some(PortType::Vec3) {
                    Ok(MoveTarget::Key(key.to_owned()))
                } else {
                    Err(GraphDiagnostic::new(
                        st_codes::MOVE_TARGET,
                        p.path("key"),
                        "the key must be a vec3 variable",
                    ))
                }
            }
            _ => Err(GraphDiagnostic::new(
                st_codes::MOVE_TARGET,
                format!("/nodes/{}", p.id),
                "set exactly one of target and key",
            )),
        }
    }

    fn native(&mut self, p: &Params<'_>) -> Result<NativeSite, GraphDiagnostic> {
        let name = p.str("name")?;
        if !valid_native_name(name) {
            return Err(GraphDiagnostic::new(
                st_codes::NATIVE_NAME,
                p.path("name"),
                "native names are 1-128 characters of letters, digits, '_', '-', '.' or ':'",
            ));
        }
        let site = self.native_sites;
        self.native_sites += 1;
        Ok(NativeSite {
            site,
            name: name.to_owned(),
            params: p.json("params").cloned().unwrap_or(Value::Null),
        })
    }

    fn transition(&mut self, id: &str, owner: usize) -> Option<TransitionDef> {
        let node = self.index.node(id)?;
        let p = params_of(node)?;
        let parsed = self.try_transition(&p, owner);
        self.keep(parsed)
    }

    fn try_transition(
        &mut self,
        p: &Params<'_>,
        owner: usize,
    ) -> Result<TransitionDef, GraphDiagnostic> {
        let trigger = match p.str("trigger")? {
            "on_succeeded" => Trigger::Succeeded,
            "on_failed" => Trigger::Failed,
            "on_tick" => Trigger::Tick,
            "on_event" => Trigger::Event,
            _ => Trigger::Completed,
        };
        let event = p.json("event").and_then(Value::as_str);
        match (trigger, event) {
            (Trigger::Event, None) => {
                return Err(GraphDiagnostic::new(
                    st_codes::TRIGGER_EVENT,
                    p.path("event"),
                    "on_event needs an event name",
                ));
            }
            (Trigger::Event, Some(name)) if !valid_event_name(name) => {
                return Err(GraphDiagnostic::new(
                    st_codes::EVENT_NAME,
                    p.path("event"),
                    "event names are 1-128 characters without control characters",
                ));
            }
            (Trigger::Event, Some(_)) => {}
            (_, Some(_)) => {
                return Err(GraphDiagnostic::new(
                    st_codes::TRIGGER_EVENT,
                    p.path("event"),
                    "an event name is only valid with on_event",
                ));
            }
            (_, None) => {}
        }
        let state_ref = p.json("state").and_then(Value::as_str).map(str::to_owned);
        let target_name = p.str("target")?;
        let target = match target_name {
            "state" => {
                if state_ref.is_none() {
                    return Err(GraphDiagnostic::new(
                        st_codes::TARGET_INVALID,
                        p.path("state"),
                        "target state needs the id of the state to go to",
                    ));
                }
                // Resolved once every state is known.
                Target::Parent
            }
            other => {
                if state_ref.is_some() {
                    return Err(GraphDiagnostic::new(
                        st_codes::TARGET_INVALID,
                        p.path("state"),
                        format!("a state id is only valid with target state, not {other:?}"),
                    ));
                }
                match other {
                    "parent" => Target::Parent,
                    "next_sibling" => Target::NextSibling,
                    "succeeded" => Target::Succeeded,
                    _ => Target::Failed,
                }
            }
        };
        if let Some(name) = event {
            self.trigger_events.insert(name.to_owned());
        }
        Ok(TransitionDef {
            id: p.id.to_owned(),
            owner,
            trigger,
            event: event.map(str::to_owned),
            target,
            state_ref,
            priority: p.u64("priority")? as u32,
            delay: p.f64("delay")?,
            variance: p.f64("delay_variance")?,
            conditions: self.conditions(p.id),
        })
    }

    fn evaluator(&mut self, id: &str) -> Option<EvaluatorDef> {
        let node = self.index.node(id)?;
        let p = params_of(node)?;
        let parsed = self.try_evaluator(node.node_type.as_str(), &p);
        self.keep(parsed)
    }

    fn evaluator_error(p: &Params<'_>, name: &str, message: &str) -> GraphDiagnostic {
        GraphDiagnostic::new(st_codes::EVALUATOR, p.path(name), message)
    }

    fn try_evaluator(
        &mut self,
        node_type: &str,
        p: &Params<'_>,
    ) -> Result<EvaluatorDef, GraphDiagnostic> {
        let kind = match node_type {
            node::COPY_EVALUATOR => {
                let key = p.str("key")?.to_owned();
                let key_ty = self.key_type(&key);
                let input = p.json("input").and_then(Value::as_str);
                let from = p.json("from").and_then(Value::as_str);
                match (input, from) {
                    (Some(input), None) => {
                        if input.is_empty() || input.chars().any(char::is_control) {
                            return Err(Self::evaluator_error(
                                p,
                                "input",
                                "host input names are 1-128 characters without control characters",
                            ));
                        }
                        self.input_names.insert(input.to_owned());
                        EvalKind::Input {
                            input: input.to_owned(),
                            key,
                        }
                    }
                    (None, Some(from)) => {
                        let from_ty = self.key_type(from);
                        let compatible = match (&from_ty, &key_ty) {
                            (Some(a), Some(b)) => {
                                a == b || (*a == PortType::Int && *b == PortType::Float)
                            }
                            _ => false,
                        };
                        if !compatible {
                            return Err(Self::evaluator_error(
                                p,
                                "from",
                                "the source key must have the destination key's type",
                            ));
                        }
                        EvalKind::Copy {
                            from: from.to_owned(),
                            key,
                        }
                    }
                    _ => {
                        return Err(GraphDiagnostic::new(
                            st_codes::EVALUATOR,
                            format!("/nodes/{}", p.id),
                            "set exactly one of input and from",
                        ));
                    }
                }
            }
            node::COMPUTE_EVALUATOR => self.compute(p)?,
            node::NATIVE_EVALUATOR => {
                let site = self.native(p)?;
                self.evaluator_names.insert(site.name.clone());
                EvalKind::Native(site)
            }
            other => {
                return Err(GraphDiagnostic::new(
                    codes::NODE_UNKNOWN_TYPE,
                    format!("/nodes/{}", p.id),
                    format!("{other:?} is not an evaluator type"),
                ));
            }
        };
        Ok(EvaluatorDef {
            id: p.id.to_owned(),
            kind,
        })
    }

    fn compute(&self, p: &Params<'_>) -> Result<EvalKind, GraphDiagnostic> {
        let op = match p.str("op")? {
            "add" => ComputeOp::Add,
            "sub" => ComputeOp::Sub,
            "mul" => ComputeOp::Mul,
            "min" => ComputeOp::Min,
            "max" => ComputeOp::Max,
            "distance" => ComputeOp::Distance,
            "not" => ComputeOp::Not,
            "and" => ComputeOp::And,
            _ => ComputeOp::Or,
        };
        let a = p.str("a")?.to_owned();
        let b = p.json("b").and_then(Value::as_str).map(str::to_owned);
        let key = p.str("key")?.to_owned();
        let unknown = |name: &str| Self::evaluator_error(p, name, "unknown variable");
        let a_ty = self.key_type(&a).ok_or_else(|| unknown("a"))?;
        let out_ty = self.key_type(&key).ok_or_else(|| unknown("key"))?;
        let b_ty = match &b {
            Some(b) => Some(self.key_type(b).ok_or_else(|| unknown("b"))?),
            None => None,
        };
        let int_out = out_ty == PortType::Int;
        let mismatch = |message: &str| Self::evaluator_error(p, "op", message);
        match op {
            ComputeOp::Not => {
                if b.is_some() {
                    return Err(mismatch("not takes one operand"));
                }
                if a_ty != PortType::Bool || out_ty != PortType::Bool {
                    return Err(mismatch("not needs a bool operand and a bool destination"));
                }
            }
            ComputeOp::And | ComputeOp::Or => {
                if b_ty != Some(PortType::Bool)
                    || a_ty != PortType::Bool
                    || out_ty != PortType::Bool
                {
                    return Err(mismatch(
                        "and and or need two bool operands and a bool destination",
                    ));
                }
            }
            ComputeOp::Distance => {
                if a_ty != PortType::Vec3
                    || b_ty != Some(PortType::Vec3)
                    || out_ty != PortType::Float
                {
                    return Err(mismatch(
                        "distance needs two vec3 operands and a float destination",
                    ));
                }
            }
            ComputeOp::Add | ComputeOp::Sub | ComputeOp::Mul | ComputeOp::Min | ComputeOp::Max => {
                let both_numeric = numeric(&a_ty) && b_ty.as_ref().is_some_and(numeric);
                if !both_numeric || !numeric(&out_ty) {
                    return Err(mismatch(
                        "numeric ops need two int or float operands and a numeric destination",
                    ));
                }
                if int_out && (a_ty != PortType::Int || b_ty != Some(PortType::Int)) {
                    return Err(mismatch("an int destination needs two int operands"));
                }
            }
        }
        Ok(EvalKind::Compute {
            op,
            a,
            b,
            key,
            int_out,
        })
    }

    /// Resolves transition targets, checks sibling and parent targets and finds
    /// states no selection or transition can reach.
    fn finish(mut self) -> Result<StateTree, Vec<GraphDiagnostic>> {
        let by_id: BTreeMap<String, usize> = self
            .states
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id.clone(), i))
            .collect();
        for slot in 0..self.transitions.len() {
            let (id, owner, target, state_ref) = {
                let t = &self.transitions[slot];
                (t.id.clone(), t.owner, t.target, t.state_ref.clone())
            };
            if let Some(name) = state_ref {
                match by_id.get(&name) {
                    Some(&state) => self.transitions[slot].target = Target::State(state),
                    None => self.error(
                        st_codes::TARGET_UNKNOWN,
                        format!("/nodes/{id}/params/state"),
                        format!("{name:?} is not a state of this tree"),
                    ),
                }
                continue;
            }
            match target {
                Target::Parent if self.states[owner].parent.is_none() => self.error(
                    st_codes::TARGET_INVALID,
                    format!("/nodes/{id}/params/target"),
                    "a top-level state has no parent to go to",
                ),
                Target::NextSibling => {
                    let siblings = match self.states[owner].parent {
                        Some(parent) => &self.states[parent].children,
                        None => &self.top,
                    };
                    let last = siblings.last() == Some(&owner);
                    if last {
                        self.error(
                            st_codes::TARGET_INVALID,
                            format!("/nodes/{id}/params/target"),
                            "the state has no later sibling to go to",
                        );
                    }
                }
                _ => {}
            }
        }
        let reachable = self.reachable();
        for (index, state) in self.states.iter().enumerate() {
            if !reachable[index] {
                self.diagnostics.push(GraphDiagnostic::new(
                    st_codes::UNREACHABLE,
                    format!("/nodes/{}", state.id),
                    "no selection or transition from a reachable state can enter this state",
                ));
            }
        }
        if !self.diagnostics.is_empty() {
            return Err(self.diagnostics);
        }
        Ok(StateTree {
            states: self.states,
            top: self.top,
            transitions: self.transitions,
            evaluators: self.evaluators,
            variables: self.doc.variables.clone(),
            trigger_events: self.trigger_events,
            native_sites: self.native_sites,
            task_names: self.task_names,
            evaluator_names: self.evaluator_names,
            input_names: self.input_names,
            uses_move_to: self.uses_move_to,
        })
    }

    /// States some selection or transition chain can enter. Top-level states are
    /// selectable at start; a reachable state selects its children unless it is
    /// `self`; transitions of reachable states reach their targets (with the
    /// target's ancestors, which are entered on the way).
    fn reachable(&self) -> Vec<bool> {
        let mut reached = vec![false; self.states.len()];
        let mut stack: Vec<usize> = Vec::new();
        let mark = |reached: &mut Vec<bool>, stack: &mut Vec<usize>, mut state: usize| loop {
            if !reached[state] {
                reached[state] = true;
                stack.push(state);
            }
            match self.states[state].parent {
                Some(parent) if !reached[parent] => state = parent,
                _ => break,
            }
        };
        for &state in &self.top {
            mark(&mut reached, &mut stack, state);
        }
        while let Some(state) = stack.pop() {
            let def = &self.states[state];
            if def.selection != Selection::SelfOnly {
                for &child in &def.children {
                    mark(&mut reached, &mut stack, child);
                }
            }
            for &slot in &def.transitions {
                match self.transitions[slot].target {
                    Target::State(target) => mark(&mut reached, &mut stack, target),
                    Target::Parent => {
                        if let Some(parent) = def.parent {
                            mark(&mut reached, &mut stack, parent);
                        }
                    }
                    Target::NextSibling => {
                        let siblings = match def.parent {
                            Some(parent) => &self.states[parent].children,
                            None => &self.top,
                        };
                        let from = siblings
                            .iter()
                            .position(|s| *s == state)
                            .map_or(0, |p| p + 1);
                        for &sibling in &siblings[from..] {
                            mark(&mut reached, &mut stack, sibling);
                        }
                    }
                    Target::Succeeded | Target::Failed => {}
                }
            }
        }
        reached
    }
}
