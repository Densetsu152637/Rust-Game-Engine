//! Shared harness for the state tree tests: a document builder and a recording host.
#![allow(dead_code)]
use engine_ai::{
    BbValue, Blackboard,
    behavior_tree::{Host, MoveStatus, NativeCall, Status},
    state_tree::{StateTree, StateTreeHost, StateTreeInstance, TickReport, compile},
};
use engine_graph::GraphDocument;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

/// Fluent builder for state tree documents.
pub struct S {
    nodes: Vec<Value>,
    edges: Vec<Value>,
    variables: Vec<Value>,
}

impl S {
    pub fn new() -> Self {
        Self {
            nodes: vec![json!({"id":"root","node_type":"root"})],
            edges: Vec::new(),
            variables: Vec::new(),
        }
    }
    pub fn var(mut self, name: &str, ty: &str, default: Value) -> Self {
        let mut v = json!({"name": name, "type": ty});
        if !default.is_null() {
            v["default"] = default;
        }
        self.variables.push(v);
        self
    }
    fn add(mut self, id: &str, ty: &str, params: Value) -> Self {
        self.nodes
            .push(json!({"id": id, "node_type": ty, "params": params}));
        self
    }
    pub fn edge(mut self, from: &str, from_port: &str, to: &str, to_port: &str) -> Self {
        self.edges
            .push(json!({"from":{"node":from,"port":from_port},"to":{"node":to,"port":to_port}}));
        self
    }
    /// A state under `parent` (`root` or another state).
    pub fn state(self, id: &str, parent: &str, params: Value) -> Self {
        self.add(id, "state", params)
            .edge(parent, "children", id, "parent")
    }
    /// A task node of `ty` on `owner`.
    pub fn task(self, owner: &str, id: &str, ty: &str, params: Value) -> Self {
        self.add(id, ty, params).edge(owner, "tasks", id, "owner")
    }
    pub fn native(self, owner: &str, id: &str, name: &str) -> Self {
        self.task(owner, id, "native_task", json!({"name": name}))
    }
    pub fn wait(self, owner: &str, id: &str, seconds: f64) -> Self {
        self.task(owner, id, "wait", json!({"seconds": seconds}))
    }
    /// A blackboard condition on a state (enter condition) or a transition.
    pub fn cond(self, owner: &str, id: &str, key: &str, op: &str, value: Value) -> Self {
        let mut params = json!({"key": key, "operator": op});
        if !value.is_null() {
            params["value"] = value;
        }
        self.add(id, "blackboard_condition", params)
            .edge(owner, "conditions", id, "owner")
    }
    /// A transition on `owner`.
    pub fn go(self, owner: &str, id: &str, params: Value) -> Self {
        self.add(id, "transition", params)
            .edge(owner, "transitions", id, "owner")
    }
    /// An evaluator on the root.
    pub fn eval(self, id: &str, ty: &str, params: Value) -> Self {
        self.add(id, ty, params)
            .edge("root", "evaluators", id, "owner")
    }
    pub fn doc(self) -> GraphDocument {
        GraphDocument::from_value(json!({
            "version": 1, "kind": "state_tree",
            "nodes": self.nodes, "edges": self.edges, "variables": self.variables,
        }))
        .expect("document parses")
    }
    pub fn compile(self) -> Arc<StateTree> {
        let doc = self.doc();
        Arc::new(compile(&doc).unwrap_or_else(|e| panic!("compile failed: {e:#?}")))
    }
}

/// Records every call and answers native tasks by name.
pub struct TestHost {
    pub log: Vec<String>,
    pub events: Vec<String>,
    pub moves: Vec<[f32; 3]>,
    pub cancels: usize,
    pub status: MoveStatus,
    pub inputs: BTreeMap<String, BbValue>,
    ticks_of: BTreeMap<usize, u32>,
}

impl TestHost {
    pub fn new() -> Self {
        Self {
            log: Vec::new(),
            events: Vec::new(),
            moves: Vec::new(),
            cancels: 0,
            status: MoveStatus::Moving,
            inputs: BTreeMap::new(),
            ticks_of: BTreeMap::new(),
        }
    }
    fn behave(&mut self, name: &str, n: u32, bb: &mut Blackboard) -> Status {
        match name {
            "ok" => Status::Success,
            "fail" => Status::Failure,
            "run" | "eval" => Status::Running,
            "run_ok" => {
                if n >= 1 {
                    Status::Success
                } else {
                    Status::Running
                }
            }
            "run_fail" => {
                if n >= 1 {
                    Status::Failure
                } else {
                    Status::Running
                }
            }
            "flag_on" => {
                bb.set("flag", BbValue::Bool(true)).unwrap();
                Status::Running
            }
            other => panic!("unknown test behavior {other}"),
        }
    }
    /// Log entries starting with `prefix`, with the prefix removed.
    pub fn filter(&self, prefix: &str) -> Vec<String> {
        self.log
            .iter()
            .filter_map(|l| l.strip_prefix(prefix).map(str::to_owned))
            .collect()
    }
}

impl Host for TestHost {
    fn request_move(&mut self, target: [f32; 3]) {
        self.log.push(format!("move:{target:?}"));
        self.moves.push(target);
        self.status = MoveStatus::Moving;
    }
    fn move_status(&mut self) -> MoveStatus {
        self.status
    }
    fn cancel_move(&mut self) {
        self.cancels += 1;
        self.log.push("cancel_move".into());
    }
    fn emit_event(&mut self, name: &str, _payload: &Value) {
        self.log.push(format!("event:{name}"));
        self.events.push(name.into());
    }
    fn native_start(&mut self, call: &NativeCall<'_>, bb: &mut Blackboard) -> Status {
        self.log.push(format!("start:{}", call.node));
        self.ticks_of.insert(call.site, 0);
        self.behave(call.name, 0, bb)
    }
    fn native_tick(&mut self, call: &NativeCall<'_>, bb: &mut Blackboard, _dt: f32) -> Status {
        self.log.push(format!("tick:{}", call.node));
        let n = self.ticks_of.entry(call.site).or_default();
        *n += 1;
        let n = *n;
        self.behave(call.name, n, bb)
    }
    fn native_abort(&mut self, call: &NativeCall<'_>, _bb: &mut Blackboard) {
        self.log.push(format!("abort:{}", call.node));
    }
}

impl StateTreeHost for TestHost {
    fn input(&mut self, name: &str) -> Option<BbValue> {
        self.inputs.get(name).cloned()
    }
    fn evaluator_start(&mut self, call: &NativeCall<'_>, _bb: &mut Blackboard) {
        self.log.push(format!("eval_start:{}", call.node));
    }
    fn evaluator_tick(&mut self, call: &NativeCall<'_>, _bb: &mut Blackboard, _dt: f32) {
        self.log.push(format!("eval_tick:{}", call.node));
    }
    fn evaluator_stop(&mut self, call: &NativeCall<'_>, _bb: &mut Blackboard) {
        self.log.push(format!("eval_stop:{}", call.node));
    }
}

/// An instance with its host.
pub struct Rig {
    pub instance: StateTreeInstance,
    pub host: TestHost,
}

impl Rig {
    pub fn new(tree: &Arc<StateTree>) -> Self {
        Self::seeded(tree, 0)
    }
    pub fn seeded(tree: &Arc<StateTree>, seed: u64) -> Self {
        Self {
            instance: StateTreeInstance::new(Arc::clone(tree), &BTreeMap::new(), seed)
                .expect("instance"),
            host: TestHost::new(),
        }
    }
    pub fn tick(&mut self, dt: f32) -> TickReport {
        self.instance.tick(dt, &mut self.host)
    }
    pub fn ticks(&mut self, n: usize, dt: f32) -> Vec<TickReport> {
        (0..n).map(|_| self.tick(dt)).collect()
    }
    /// The active path joined with `/`.
    pub fn path(&self) -> String {
        self.instance.active_path().join("/")
    }
    pub fn set(&mut self, key: &str, value: BbValue) {
        self.instance.blackboard_mut().set(key, value).unwrap();
    }
}
