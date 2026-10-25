//! Shared test harness: a document builder and a recording host.
#![allow(dead_code)]
use engine_ai::{
    BbValue, Blackboard,
    behavior_tree::{
        BehaviorTree, Host, MoveStatus, NativeCall, Status, TickReport, TreeInstance, compile,
    },
};
use engine_graph::GraphDocument;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

/// Fluent builder for behavior tree documents.
pub struct B {
    nodes: Vec<Value>,
    edges: Vec<Value>,
    variables: Vec<Value>,
    types: BTreeMap<String, String>,
}

impl B {
    pub fn new() -> Self {
        let mut b = Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            variables: Vec::new(),
            types: BTreeMap::new(),
        };
        b.nodes.push(json!({"id":"root","node_type":"root"}));
        b.types.insert("root".into(), "root".into());
        b
    }
    pub fn node(mut self, id: &str, ty: &str, params: Value) -> Self {
        self.nodes
            .push(json!({"id": id, "node_type": ty, "params": params}));
        self.types.insert(id.into(), ty.into());
        self
    }
    /// A native task node running the test behavior `name`.
    pub fn leaf(self, id: &str, name: &str) -> Self {
        self.node(id, "native_task", json!({"name": name}))
    }
    pub fn var(mut self, name: &str, ty: &str, default: Value) -> Self {
        let mut v = json!({"name": name, "type": ty});
        if !default.is_null() {
            v["default"] = default;
        }
        self.variables.push(v);
        self
    }
    /// Connects `parent` to `child` through the right ports.
    pub fn link(mut self, parent: &str, child: &str) -> Self {
        let port = match self.types[parent].as_str() {
            "sequence" | "selector" | "parallel" | "random_selector" => "children",
            _ => "child",
        };
        self.edges
            .push(json!({"from":{"node":parent,"port":port},"to":{"node":child,"port":"parent"}}));
        self
    }
    /// Attaches a service node to a composite.
    pub fn service(mut self, owner: &str, id: &str, ty: &str, params: Value) -> Self {
        self.nodes
            .push(json!({"id": id, "node_type": ty, "params": params}));
        self.types.insert(id.into(), ty.into());
        self.edges
            .push(json!({"from":{"node":owner,"port":"services"},"to":{"node":id,"port":"owner"}}));
        self
    }
    pub fn doc(self) -> GraphDocument {
        GraphDocument::from_value(json!({
            "version": 1, "kind": "behavior_tree",
            "nodes": self.nodes, "edges": self.edges, "variables": self.variables,
        }))
        .expect("document parses")
    }
    pub fn compile(self) -> Arc<BehaviorTree> {
        let doc = self.doc();
        Arc::new(compile(&doc).unwrap_or_else(|e| panic!("compile failed: {e:#?}")))
    }
}

/// Records every call and answers native tasks by name.
pub struct TestHost {
    pub log: Vec<String>,
    pub events: Vec<(u64, String, Value)>,
    pub moves: Vec<[f32; 3]>,
    pub cancels: usize,
    pub status: MoveStatus,
    pub tick: u64,
    ticks_of: BTreeMap<usize, u32>,
    flaky: u32,
}

impl TestHost {
    pub fn new() -> Self {
        Self {
            log: Vec::new(),
            events: Vec::new(),
            moves: Vec::new(),
            cancels: 0,
            status: MoveStatus::Moving,
            tick: 0,
            ticks_of: BTreeMap::new(),
            flaky: 0,
        }
    }
    fn behave(&mut self, name: &str, n: u32, bb: &mut Blackboard) -> Status {
        match name {
            "ok" => Status::Success,
            "fail" => Status::Failure,
            "run" | "svc" => Status::Running,
            "run_ok" => {
                if n >= 1 {
                    Status::Success
                } else {
                    Status::Running
                }
            }
            "run3_ok" => {
                if n >= 3 {
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
                bb.set("alarm", BbValue::Bool(true)).unwrap();
                Status::Success
            }
            "flaky" => {
                self.flaky += 1;
                if self.flaky <= 2 {
                    Status::Failure
                } else {
                    Status::Success
                }
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
    fn emit_event(&mut self, name: &str, payload: &Value) {
        self.log.push(format!("event:{name}"));
        self.events.push((self.tick, name.into(), payload.clone()));
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

pub fn instance(tree: &Arc<BehaviorTree>) -> TreeInstance {
    TreeInstance::new(Arc::clone(tree), &BTreeMap::new(), 0).expect("instance")
}

/// Ticks `n` times with `dt`, numbering host ticks from 1; returns the reports.
pub fn run(instance: &mut TreeInstance, host: &mut TestHost, n: usize, dt: f32) -> Vec<TickReport> {
    (0..n)
        .map(|_| {
            host.tick += 1;
            instance.tick(dt, host)
        })
        .collect()
}

pub fn finished(reports: &[TickReport]) -> Vec<Option<Status>> {
    reports.iter().map(|r| r.finished).collect()
}
