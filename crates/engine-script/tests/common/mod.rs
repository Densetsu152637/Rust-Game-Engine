//! Shared helpers: a document builder and a recording host.
#![allow(dead_code)]
use engine_graph::GraphDocument;
use engine_script::{
    EntityRef, FnId, FnSig, ScriptHost, SigTable, Trap, Value, ValueType, graph::shared_kind,
};
use serde_json::{Value as Json, json};

#[derive(Default)]
pub struct Doc {
    nodes: Vec<Json>,
    edges: Vec<Json>,
    variables: Vec<Json>,
}

impl Doc {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn node(&mut self, id: &str, ty: &str, params: Json) -> &mut Self {
        self.nodes
            .push(json!({"id": id, "node_type": ty, "params": params}));
        self
    }
    /// A node with extra ports appended to the declared ones.
    pub fn node_with(&mut self, id: &str, ty: &str, params: Json, extra: Json) -> &mut Self {
        let declared = shared_kind()
            .registry
            .get(ty)
            .expect("known type")
            .declared_ports();
        let mut ports = serde_json::to_value(&declared).expect("ports serialize");
        ports
            .as_array_mut()
            .expect("array")
            .extend(extra.as_array().expect("array").iter().cloned());
        self.nodes
            .push(json!({"id": id, "node_type": ty, "params": params, "ports": ports}));
        self
    }
    /// `from.port -> to.exec`
    pub fn flow(&mut self, from: &str, port: &str, to: &str) -> &mut Self {
        self.data(from, port, to, "exec")
    }
    pub fn data(&mut self, from: &str, fp: &str, to: &str, tp: &str) -> &mut Self {
        self.edges.push(json!({
            "from": {"node": from, "port": fp},
            "to": {"node": to, "port": tp}
        }));
        self
    }
    pub fn var(&mut self, name: &str, ty: &str, default: Json) -> &mut Self {
        self.variables
            .push(json!({"name": name, "type": ty, "default": default}));
        self
    }
    pub fn build(&self) -> GraphDocument {
        GraphDocument::from_value(json!({
            "version": 1,
            "kind": "script_graph",
            "nodes": self.nodes,
            "edges": self.edges,
            "variables": self.variables,
        }))
        .expect("document parses")
    }
}

/// Signatures: id 0 `game.add` (pure), 1 `game.record` (impure), 2 `game.fail`.
pub fn sigs() -> SigTable {
    let mut table = SigTable::new();
    table
        .insert(
            "game.add",
            FnSig::new(
                &[("a", ValueType::Float), ("b", ValueType::Float)],
                Some(ValueType::Float),
            )
            .pure(),
        )
        .unwrap();
    table
        .insert(
            "game.record",
            FnSig::new(&[("n", ValueType::Int)], None).cost(3),
        )
        .unwrap();
    table.insert("game.fail", FnSig::new(&[], None)).unwrap();
    table
}

#[derive(Default)]
pub struct Rec {
    pub calls: Vec<(u32, Vec<Value>)>,
    pub emitted: Vec<(String, Value, Option<EntityRef>)>,
    pub logs: Vec<String>,
}

impl ScriptHost for Rec {
    fn call(&mut self, function: FnId, args: &[Value]) -> Result<Value, Trap> {
        self.calls.push((function.0, args.to_vec()));
        match function.0 {
            0 => match (&args[0], &args[1]) {
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
                _ => Err(Trap::NonFinite),
            },
            1 => Ok(Value::Unit),
            _ => Err(Trap::host("script.host.failed", "game.fail always fails")),
        }
    }
    fn emit(&mut self, name: &str, payload: Value, target: Option<EntityRef>) -> Result<(), Trap> {
        self.emitted.push((name.to_owned(), payload, target));
        Ok(())
    }
    fn log(&mut self, message: &str) {
        self.logs.push(message.to_owned());
    }
    fn subject(&self) -> EntityRef {
        EntityRef(7)
    }
}
