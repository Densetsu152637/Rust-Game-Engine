//! Compiles a validated `behavior_tree` [`GraphDocument`] into an immutable tree.
//!
//! The compiled [`BehaviorTree`] is plain shared data; every agent runs its own
//! [`TreeInstance`](super::TreeInstance) over one `Arc<BehaviorTree>`.
use super::kind::{self, MAX_CHILDREN, MAX_DEPTH, node, port};
use crate::blackboard::BbValue;
use engine_graph::{
    GraphDiagnostic, GraphDocument, GraphIndex, GraphNode, GraphVariable, NodeTypeDef, PortType,
    Strictness, codes, validate,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Diagnostic codes added by the behavior tree compiler.
pub mod bt_codes {
    pub const COMPOSITE_EMPTY: &str = "bt.composite.empty";
    pub const CHILDREN_LIMIT: &str = "bt.composite.children";
    pub const DEPTH: &str = "bt.tree.depth";
    pub const CONDITION_VALUE: &str = "bt.condition.value";
    pub const CONDITION_OPERATOR: &str = "bt.condition.operator";
    pub const OBSERVER_PRIORITY: &str = "bt.observer.priority";
    pub const VALUE_TYPE: &str = "bt.value.type";
    pub const MOVE_TARGET: &str = "bt.move_to.target";
    pub const EVENT_NAME: &str = "bt.event.name";
    pub const EVENT_PAYLOAD: &str = "bt.event.payload";
    pub const NATIVE_NAME: &str = "bt.native.name";
    /// Every code above.
    pub const ALL: &[&str] = &[
        COMPOSITE_EMPTY,
        CHILDREN_LIMIT,
        DEPTH,
        CONDITION_VALUE,
        CONDITION_OPERATOR,
        OBSERVER_PRIORITY,
        VALUE_TYPE,
        MOVE_TARGET,
        EVENT_NAME,
        EVENT_PAYLOAD,
        NATIVE_NAME,
    ];
}

/// How a parallel node counts child outcomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    RequireOne,
    RequireAll,
}

/// Which child outcome starts a cooldown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CooldownLock {
    Any,
    Success,
    Failure,
}

/// Comparison of a blackboard condition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operator {
    IsSet,
    IsNotSet,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Unreal-style observer abort mode of a condition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbortMode {
    None,
    SelfAbort,
    LowerPriority,
    Both,
}

impl AbortMode {
    pub(crate) fn aborts_self(self) -> bool {
        matches!(self, Self::SelfAbort | Self::Both)
    }
    pub(crate) fn aborts_lower(self) -> bool {
        matches!(self, Self::LowerPriority | Self::Both)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Condition {
    pub key: String,
    pub op: Operator,
    pub value: Option<BbValue>,
    pub abort: AbortMode,
}

#[derive(Clone, Debug)]
pub(crate) enum MoveTarget {
    Point([f32; 3]),
    Key(String),
}

/// One native task or service call site; `site` is unique within the tree.
#[derive(Clone, Debug)]
pub(crate) struct NativeSite {
    pub site: usize,
    pub name: String,
    pub params: Value,
}

#[derive(Clone, Debug)]
pub(crate) enum NodeKind {
    Root,
    Sequence,
    Selector,
    Parallel {
        success: Policy,
        failure: Policy,
    },
    RandomSelector {
        seed: u64,
    },
    Inverter,
    Repeat {
        count: u32,
        stop_on_failure: bool,
    },
    Retry {
        attempts: u32,
    },
    Cooldown {
        duration: f64,
        lock_on: CooldownLock,
    },
    TimeLimit {
        seconds: f64,
    },
    ForceSuccess,
    ForceFailure,
    Condition(Condition),
    Wait {
        seconds: f64,
    },
    SetBlackboard {
        key: String,
        value: BbValue,
    },
    ClearBlackboard {
        key: String,
    },
    EmitEvent {
        event: String,
        payload: Value,
    },
    MoveTo {
        target: MoveTarget,
        timeout: f64,
    },
    Native(NativeSite),
}

#[derive(Clone, Debug)]
pub(crate) enum ServiceKind {
    Native(NativeSite),
    SetBlackboard { key: String, value: BbValue },
    EmitEvent { event: String, payload: Value },
}

#[derive(Clone, Debug)]
pub(crate) struct Service {
    pub id: String,
    pub interval: f64,
    pub kind: ServiceKind,
}

#[derive(Clone, Debug)]
pub(crate) struct TreeNode {
    pub id: String,
    pub node_type: String,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub services: Vec<Service>,
    pub kind: NodeKind,
}

/// A condition whose key changes can abort running branches.
#[derive(Clone, Debug)]
pub(crate) struct Observer {
    pub node: usize,
    pub key: String,
    pub abort: AbortMode,
    /// For lower-priority aborts: the priority composite and the slot of the
    /// child branch that contains the condition.
    pub priority: Option<(usize, usize)>,
}

/// An immutable compiled behavior tree.
#[derive(Clone, Debug)]
pub struct BehaviorTree {
    pub(crate) nodes: Vec<TreeNode>,
    pub(crate) variables: Vec<GraphVariable>,
    pub(crate) observers: Vec<Observer>,
    pub(crate) observers_by_key: BTreeMap<String, Vec<usize>>,
    native_sites: usize,
    native_names: BTreeSet<String>,
    uses_move_to: bool,
}

impl BehaviorTree {
    /// Nodes in depth-first order; index 0 is the root.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }
    /// Graph node id of the compiled node at `index`.
    pub fn node_id(&self, index: usize) -> Option<&str> {
        self.nodes.get(index).map(|n| n.id.as_str())
    }
    pub fn node_type(&self, index: usize) -> Option<&str> {
        self.nodes.get(index).map(|n| n.node_type.as_str())
    }
    /// The document variables, which declare the blackboard keys.
    pub fn variables(&self) -> &[GraphVariable] {
        &self.variables
    }
    /// Names of every native task and native service the tree calls.
    pub fn native_names(&self) -> &BTreeSet<String> {
        &self.native_names
    }
    /// Count of native call sites (tasks plus services).
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
/// Returns every graph and behavior tree diagnostic; nothing compiles on error.
pub fn compile(doc: &GraphDocument) -> Result<BehaviorTree, Vec<GraphDiagnostic>> {
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

fn build(doc: &GraphDocument) -> Result<BehaviorTree, Vec<GraphDiagnostic>> {
    let mut builder = Builder {
        doc,
        index: GraphIndex::new(doc),
        nodes: Vec::new(),
        diagnostics: Vec::new(),
        native_sites: 0,
        native_names: BTreeSet::new(),
        uses_move_to: false,
    };
    let root = doc
        .nodes
        .iter()
        .find(|n| n.node_type == node::ROOT)
        .map(|n| n.id.as_str().to_owned());
    match root {
        Some(root) => {
            builder.visit(&root, None, 0);
        }
        None => builder.error(codes::ROOT_MISSING, "", "the tree has no root node"),
    }
    builder.resolve_observers()
}

struct Builder<'a> {
    doc: &'a GraphDocument,
    index: GraphIndex<'a>,
    nodes: Vec<TreeNode>,
    diagnostics: Vec<GraphDiagnostic>,
    native_sites: usize,
    native_names: BTreeSet<String>,
    uses_move_to: bool,
}

/// Effective parameters of one node with typed accessors that report problems.
pub(crate) struct Params<'a> {
    pub(crate) values: BTreeMap<String, Value>,
    pub(crate) id: &'a str,
}

impl Params<'_> {
    pub(crate) fn missing(&self, name: &str) -> GraphDiagnostic {
        GraphDiagnostic::new(
            codes::PARAM_MISSING,
            format!("/nodes/{}/params/{name}", self.id),
            "required parameter is missing",
        )
    }
    pub(crate) fn f64(&self, name: &str) -> Result<f64, GraphDiagnostic> {
        self.values
            .get(name)
            .and_then(Value::as_f64)
            .ok_or_else(|| self.missing(name))
    }
    pub(crate) fn u64(&self, name: &str) -> Result<u64, GraphDiagnostic> {
        self.values
            .get(name)
            .and_then(Value::as_u64)
            .ok_or_else(|| self.missing(name))
    }
    pub(crate) fn bool(&self, name: &str) -> Result<bool, GraphDiagnostic> {
        self.values
            .get(name)
            .and_then(Value::as_bool)
            .ok_or_else(|| self.missing(name))
    }
    pub(crate) fn str(&self, name: &str) -> Result<&str, GraphDiagnostic> {
        self.values
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| self.missing(name))
    }
    pub(crate) fn json(&self, name: &str) -> Option<&Value> {
        self.values.get(name)
    }
    pub(crate) fn path(&self, name: &str) -> String {
        format!("/nodes/{}/params/{name}", self.id)
    }
}

fn policy(text: &str) -> Policy {
    if text == "require_one" {
        Policy::RequireOne
    } else {
        Policy::RequireAll
    }
}

pub(crate) fn valid_event_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
}

pub(crate) fn valid_native_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

impl<'a> Builder<'a> {
    fn error(&mut self, code: &str, path: impl Into<String>, message: impl Into<String>) {
        self.diagnostics
            .push(GraphDiagnostic::new(code, path, message));
    }

    fn key_type(&self, key: &str) -> Option<PortType> {
        self.doc.variable(key).map(|v| v.ty.clone())
    }

    /// Depth-first, pre-order, children in edge order. Returns the node index.
    fn visit(&mut self, id: &str, parent: Option<usize>, depth: usize) -> Option<usize> {
        if depth > MAX_DEPTH {
            self.error(
                bt_codes::DEPTH,
                format!("/nodes/{id}"),
                format!("tree nesting exceeds {MAX_DEPTH}"),
            );
            return None;
        }
        let node = self.index.node(id)?;
        let def = kind::shared_kind().registry.get(&node.node_type)?;
        let params = Params {
            values: def.effective_params(node),
            id: node.id.as_str(),
        };
        let index = self.nodes.len();
        let kind = self.node_kind(node, def, &params)?;
        let services = self.services(node);
        self.nodes.push(TreeNode {
            id: id.to_owned(),
            node_type: node.node_type.clone(),
            parent,
            children: Vec::new(),
            services,
            kind,
        });
        let port = match node.node_type.as_str() {
            node::ROOT => port::CHILD,
            _ if def.port_def(port::CHILDREN).is_some() => port::CHILDREN,
            _ if def.port_def(port::CHILD).is_some() => port::CHILD,
            _ => return Some(index),
        };
        let targets = self.index.targets(id, port);
        if port == port::CHILDREN {
            if targets.is_empty() {
                self.error(
                    bt_codes::COMPOSITE_EMPTY,
                    format!("/nodes/{id}"),
                    "a composite needs at least one child",
                );
            }
            if targets.len() > MAX_CHILDREN {
                self.error(
                    bt_codes::CHILDREN_LIMIT,
                    format!("/nodes/{id}"),
                    format!("more than {MAX_CHILDREN} children"),
                );
            }
        }
        let mut children = Vec::with_capacity(targets.len());
        for target in targets {
            if let Some(child) = self.visit(target, Some(index), depth + 1) {
                children.push(child);
            }
        }
        self.nodes[index].children = children;
        Some(index)
    }

    fn node_kind(
        &mut self,
        node: &GraphNode,
        def: &NodeTypeDef,
        p: &Params<'_>,
    ) -> Option<NodeKind> {
        let result = self.try_node_kind(node, def, p);
        match result {
            Ok(kind) => Some(kind),
            Err(diagnostic) => {
                self.diagnostics.push(diagnostic);
                None
            }
        }
    }

    fn try_node_kind(
        &mut self,
        node: &GraphNode,
        _def: &NodeTypeDef,
        p: &Params<'_>,
    ) -> Result<NodeKind, GraphDiagnostic> {
        Ok(match node.node_type.as_str() {
            node::ROOT => NodeKind::Root,
            node::SEQUENCE => NodeKind::Sequence,
            node::SELECTOR => NodeKind::Selector,
            node::PARALLEL => NodeKind::Parallel {
                success: policy(p.str("success_policy")?),
                failure: policy(p.str("failure_policy")?),
            },
            node::RANDOM_SELECTOR => NodeKind::RandomSelector {
                seed: p.u64("seed")?,
            },
            node::INVERTER => NodeKind::Inverter,
            node::REPEAT => NodeKind::Repeat {
                count: p.u64("count")? as u32,
                stop_on_failure: p.bool("stop_on_failure")?,
            },
            node::RETRY => NodeKind::Retry {
                attempts: p.u64("attempts")? as u32,
            },
            node::COOLDOWN => NodeKind::Cooldown {
                duration: p.f64("duration")?,
                lock_on: match p.str("lock_on")? {
                    "success" => CooldownLock::Success,
                    "failure" => CooldownLock::Failure,
                    _ => CooldownLock::Any,
                },
            },
            node::TIME_LIMIT => NodeKind::TimeLimit {
                seconds: p.f64("seconds")?,
            },
            node::FORCE_SUCCESS => NodeKind::ForceSuccess,
            node::FORCE_FAILURE => NodeKind::ForceFailure,
            node::BLACKBOARD_CONDITION => NodeKind::Condition(self.condition(p)?),
            node::WAIT => NodeKind::Wait {
                seconds: p.f64("seconds")?,
            },
            node::SET_BLACKBOARD => {
                let key = p.str("key")?.to_owned();
                let value = self.constant(p, &key)?;
                NodeKind::SetBlackboard { key, value }
            }
            node::CLEAR_BLACKBOARD => NodeKind::ClearBlackboard {
                key: p.str("key")?.to_owned(),
            },
            node::EMIT_EVENT => {
                let (event, payload) = self.event(p)?;
                NodeKind::EmitEvent { event, payload }
            }
            node::MOVE_TO => {
                self.uses_move_to = true;
                NodeKind::MoveTo {
                    target: self.move_target(p)?,
                    timeout: p.f64("timeout")?,
                }
            }
            node::NATIVE_TASK => NodeKind::Native(self.native(p)?),
            other => {
                return Err(GraphDiagnostic::new(
                    codes::NODE_UNKNOWN_TYPE,
                    format!("/nodes/{}", node.id),
                    format!("{other:?} is not a behavior tree node type"),
                ));
            }
        })
    }

    fn constant(&self, p: &Params<'_>, key: &str) -> Result<BbValue, GraphDiagnostic> {
        let ty = self.key_type(key).ok_or_else(|| {
            GraphDiagnostic::new(codes::PARAM_VARIABLE, p.path("key"), "unknown variable")
        })?;
        let value = p.json("value").ok_or_else(|| p.missing("value"))?;
        BbValue::from_json(&ty, value).ok_or_else(|| {
            GraphDiagnostic::new(
                bt_codes::VALUE_TYPE,
                p.path("value"),
                format!("value is not a {ty:?}"),
            )
        })
    }

    fn condition(&self, p: &Params<'_>) -> Result<Condition, GraphDiagnostic> {
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
        let abort = match p.str("abort")? {
            "self" => AbortMode::SelfAbort,
            "lower_priority" => AbortMode::LowerPriority,
            "both" => AbortMode::Both,
            _ => AbortMode::None,
        };
        let value = if matches!(op, Operator::IsSet | Operator::IsNotSet) {
            None
        } else {
            if matches!(
                op,
                Operator::Lt | Operator::Le | Operator::Gt | Operator::Ge
            ) && !matches!(ty, PortType::Int | PortType::Float)
            {
                return Err(GraphDiagnostic::new(
                    bt_codes::CONDITION_OPERATOR,
                    p.path("operator"),
                    format!("ordering needs an int or float key, {key:?} is {ty:?}"),
                ));
            }
            let raw = p.json("value").ok_or_else(|| {
                GraphDiagnostic::new(
                    bt_codes::CONDITION_VALUE,
                    p.path("value"),
                    "this operator compares against a value",
                )
            })?;
            Some(BbValue::from_json(&ty, raw).ok_or_else(|| {
                GraphDiagnostic::new(
                    bt_codes::CONDITION_VALUE,
                    p.path("value"),
                    format!("value is not a {ty:?}"),
                )
            })?)
        };
        Ok(Condition {
            key,
            op,
            value,
            abort,
        })
    }

    fn event(&self, p: &Params<'_>) -> Result<(String, Value), GraphDiagnostic> {
        let event = p.str("event")?;
        if !valid_event_name(event) {
            return Err(GraphDiagnostic::new(
                bt_codes::EVENT_NAME,
                p.path("event"),
                "event names are 1-128 characters without control characters",
            ));
        }
        let payload = p.json("payload").cloned().unwrap_or(Value::Null);
        if !(payload.is_object() || payload.is_null()) {
            return Err(GraphDiagnostic::new(
                bt_codes::EVENT_PAYLOAD,
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
            (Some(value), None) => {
                let point = BbValue::from_json(&PortType::Vec3, value).ok_or_else(|| {
                    GraphDiagnostic::new(
                        bt_codes::MOVE_TARGET,
                        p.path("target"),
                        "target must be [x, y, z]",
                    )
                })?;
                match point {
                    BbValue::Vec3(v) => Ok(MoveTarget::Point(v)),
                    _ => unreachable!("parsed as Vec3"),
                }
            }
            (None, Some(key)) => {
                if self.key_type(key) == Some(PortType::Vec3) {
                    Ok(MoveTarget::Key(key.to_owned()))
                } else {
                    Err(GraphDiagnostic::new(
                        bt_codes::MOVE_TARGET,
                        p.path("key"),
                        "the key must be a vec3 variable",
                    ))
                }
            }
            _ => Err(GraphDiagnostic::new(
                bt_codes::MOVE_TARGET,
                format!("/nodes/{}", p.id),
                "set exactly one of target and key",
            )),
        }
    }

    fn native(&mut self, p: &Params<'_>) -> Result<NativeSite, GraphDiagnostic> {
        let name = p.str("name")?;
        if !valid_native_name(name) {
            return Err(GraphDiagnostic::new(
                bt_codes::NATIVE_NAME,
                p.path("name"),
                "native names are 1-128 characters of letters, digits, '_', '-', '.' or ':'",
            ));
        }
        self.native_names.insert(name.to_owned());
        let site = self.native_sites;
        self.native_sites += 1;
        Ok(NativeSite {
            site,
            name: name.to_owned(),
            params: p.json("params").cloned().unwrap_or(Value::Null),
        })
    }

    fn services(&mut self, node: &GraphNode) -> Vec<Service> {
        let mut services = Vec::new();
        for id in self.index.targets(node.id.as_str(), port::SERVICES) {
            let Some(service) = self.index.node(id) else {
                continue;
            };
            let Some(def) = kind::shared_kind().registry.get(&service.node_type) else {
                continue;
            };
            let p = Params {
                values: def.effective_params(service),
                id: service.id.as_str(),
            };
            match self.service(service, &p) {
                Ok(service) => services.push(service),
                Err(diagnostic) => self.diagnostics.push(diagnostic),
            }
        }
        services
    }

    fn service(&mut self, node: &GraphNode, p: &Params<'_>) -> Result<Service, GraphDiagnostic> {
        let kind = match node.node_type.as_str() {
            node::NATIVE_SERVICE => ServiceKind::Native(self.native(p)?),
            node::SET_BLACKBOARD_SERVICE => {
                let key = p.str("key")?.to_owned();
                let value = self.constant(p, &key)?;
                ServiceKind::SetBlackboard { key, value }
            }
            node::EMIT_EVENT_SERVICE => {
                let (event, payload) = self.event(p)?;
                ServiceKind::EmitEvent { event, payload }
            }
            other => {
                return Err(GraphDiagnostic::new(
                    codes::NODE_UNKNOWN_TYPE,
                    format!("/nodes/{}", node.id),
                    format!("{other:?} is not a service"),
                ));
            }
        };
        Ok(Service {
            id: node.id.as_str().to_owned(),
            interval: p.f64("interval")?,
            kind,
        })
    }

    /// Links observer conditions to the composite whose priority they override.
    fn resolve_observers(mut self) -> Result<BehaviorTree, Vec<GraphDiagnostic>> {
        let mut observers = Vec::new();
        for index in 0..self.nodes.len() {
            let NodeKind::Condition(condition) = &self.nodes[index].kind else {
                continue;
            };
            if condition.abort == AbortMode::None {
                continue;
            }
            let (key, abort) = (condition.key.clone(), condition.abort);
            let priority = if abort.aborts_lower() {
                match self.priority_parent(index) {
                    Some(found) => Some(found),
                    None => {
                        let id = self.nodes[index].id.clone();
                        self.error(
                            bt_codes::OBSERVER_PRIORITY,
                            format!("/nodes/{id}/params/abort"),
                            "lower_priority and both need a sequence or selector parent (through decorators)",
                        );
                        continue;
                    }
                }
            } else {
                None
            };
            observers.push(Observer {
                node: index,
                key,
                abort,
                priority,
            });
        }
        if !self.diagnostics.is_empty() {
            return Err(self.diagnostics);
        }
        let mut observers_by_key: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (slot, observer) in observers.iter().enumerate() {
            observers_by_key
                .entry(observer.key.clone())
                .or_default()
                .push(slot);
        }
        Ok(BehaviorTree {
            nodes: self.nodes,
            variables: self.doc.variables.clone(),
            observers,
            observers_by_key,
            native_sites: self.native_sites,
            native_names: self.native_names,
            uses_move_to: self.uses_move_to,
        })
    }

    /// The nearest sequence or selector above `index` (looking through
    /// decorators) and the child slot the condition's branch occupies.
    fn priority_parent(&self, index: usize) -> Option<(usize, usize)> {
        let mut current = index;
        while let Some(parent) = self.nodes[current].parent {
            match self.nodes[parent].kind {
                NodeKind::Sequence | NodeKind::Selector => {
                    let slot = self.nodes[parent]
                        .children
                        .iter()
                        .position(|c| *c == current)?;
                    return Some((parent, slot));
                }
                NodeKind::Inverter
                | NodeKind::Repeat { .. }
                | NodeKind::Retry { .. }
                | NodeKind::Cooldown { .. }
                | NodeKind::TimeLimit { .. }
                | NodeKind::ForceSuccess
                | NodeKind::ForceFailure
                | NodeKind::Condition(_) => current = parent,
                _ => return None,
            }
        }
        None
    }
}
