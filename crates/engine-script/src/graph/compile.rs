//! Analysis and code generation for `script_graph` documents.
//!
//! [`check_document`] is the kind check (no project, no signatures);
//! [`compile`] additionally resolves `call` nodes against a [`Signatures`] table and
//! returns a runnable [`Program`]. Neither executes anything.
use super::{
    kind::{node, port, shared_kind, valid_event_name},
    program::{
        BinOp, EntryKey, FuncInfo, MAX_OPS, MAX_REGS, Op, Program, Reg, Region, UnOp, VarDecl,
    },
};
use crate::{
    codes::{self, graph as gc},
    host::{FnId, FnSig, Signatures},
    value::{Value, ValueType},
};
use engine_graph::{
    GraphDiagnostic, GraphDocument, Port, PortDirection, PortType, Strictness, valid_name, validate,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// Longest literal `for_range` the kind accepts (fuel bounds the rest at run time).
pub const MAX_STATIC_LOOP: i64 = 100_000;

/// The kind check: structure and data flow of the document, without a project.
pub fn check_document(doc: &GraphDocument) -> Vec<GraphDiagnostic> {
    match build(doc, None) {
        Ok(_) => Vec::new(),
        Err(diagnostics) => diagnostics,
    }
}

/// Validates and compiles `doc`, resolving host calls with `sigs`.
///
/// # Errors
/// Coded diagnostics: the generic graph rules, the kind rules (`script.graph.*`)
/// and, for host calls, `script.call.unknown` and `script.call.ports`.
pub fn compile(
    doc: &GraphDocument,
    sigs: &dyn Signatures,
) -> Result<Program, Vec<GraphDiagnostic>> {
    let generic = validate(doc, shared_kind(), Strictness::Complete);
    if !generic.is_empty() {
        return Err(generic);
    }
    build(doc, Some(sigs))
}

type R<T> = Result<T, ()>;

struct N {
    id: String,
    ty: String,
    ports: Vec<Port>,
    extras: Vec<Port>,
    params: BTreeMap<String, serde_json::Value>,
}

impl N {
    fn str(&self, key: &str) -> &str {
        self.params.get(key).and_then(|v| v.as_str()).unwrap_or("")
    }
    fn int(&self, key: &str) -> i64 {
        self.params.get(key).and_then(|v| v.as_i64()).unwrap_or(0)
    }
    fn float(&self, key: &str) -> f64 {
        self.params.get(key).and_then(|v| v.as_f64()).unwrap_or(0.0)
    }
    fn bool(&self, key: &str) -> bool {
        self.params
            .get(key)
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }
    fn port(&self, name: &str) -> Option<&Port> {
        self.ports.iter().find(|p| p.name == name)
    }
}

struct G {
    n: Vec<N>,
    /// Per node: in port -> producers `(node, out port)` in edge order.
    inc: Vec<BTreeMap<String, Vec<(usize, String)>>>,
    /// Per node: out port -> consumer nodes in edge order.
    out: Vec<BTreeMap<String, Vec<usize>>>,
    flow_call: Vec<bool>,
    vars: Vec<VarDecl>,
    diags: Vec<GraphDiagnostic>,
}

fn is_entry_type(ty: &str) -> bool {
    matches!(
        ty,
        node::ON_START | node::ON_UPDATE | node::ON_EVENT | node::ON_STOP | node::FUNCTION_ENTRY
    )
}

fn flow_outs(ty: &str) -> &'static [&'static str] {
    match ty {
        node::BRANCH => &[port::TRUE, port::FALSE],
        node::FOR_RANGE | node::WHILE => &[port::BODY, port::COMPLETED],
        node::GATE => &[port::THEN, port::CLOSED],
        node::RETURN => &[],
        _ => &[port::THEN],
    }
}

impl G {
    fn new(doc: &GraphDocument) -> Self {
        let kind = shared_kind();
        let mut index = BTreeMap::new();
        let mut n = Vec::new();
        for (i, node) in doc.nodes.iter().enumerate() {
            index.insert(node.id.as_str().to_owned(), i);
            let def = kind.registry.get(&node.node_type);
            let declared = def.map(|d| d.declared_ports()).unwrap_or_default();
            let ports = if node.ports.is_empty() {
                declared.clone()
            } else {
                node.ports.clone()
            };
            let extras = ports
                .iter()
                .filter(|p| !declared.iter().any(|d| d.name == p.name))
                .cloned()
                .collect();
            n.push(N {
                id: node.id.as_str().to_owned(),
                ty: node.node_type.clone(),
                ports,
                extras,
                params: def.map_or_else(|| node.params.clone(), |d| d.effective_params(node)),
            });
        }
        let mut inc = vec![BTreeMap::new(); n.len()];
        let mut out = vec![BTreeMap::new(); n.len()];
        for edge in &doc.edges {
            let (Some(&a), Some(&b)) = (
                index.get(edge.from.node.as_str()),
                index.get(edge.to.node.as_str()),
            ) else {
                continue;
            };
            inc[b]
                .entry(edge.to.port.clone())
                .or_insert_with(Vec::new)
                .push((a, edge.from.port.clone()));
            out[a]
                .entry(edge.from.port.clone())
                .or_insert_with(Vec::new)
                .push(b);
        }
        let flow_call = (0..n.len())
            .map(|i| {
                n[i].ty == node::CALL
                    && (inc[i].contains_key(port::EXEC) || out[i].contains_key(port::THEN))
            })
            .collect();
        let vars = doc
            .variables
            .iter()
            .filter_map(|v| {
                let ty = ValueType::from_port_type(&v.ty)?;
                let default = v
                    .default
                    .as_ref()
                    .and_then(|d| Value::from_json(ty, d))
                    .unwrap_or_else(|| ty.default_value());
                Some(VarDecl {
                    name: v.name.clone(),
                    ty,
                    default,
                })
            })
            .collect();
        Self {
            n,
            inc,
            out,
            flow_call,
            vars,
            diags: Vec::new(),
        }
    }

    fn is_entry(&self, i: usize) -> bool {
        is_entry_type(&self.n[i].ty)
    }
    fn is_flow(&self, i: usize) -> bool {
        match self.n[i].ty.as_str() {
            node::BRANCH
            | node::SEQUENCE
            | node::FOR_RANGE
            | node::WHILE
            | node::DO_ONCE
            | node::GATE
            | node::WAIT
            | node::RETURN
            | node::SET_VAR
            | node::CALL_FUNCTION
            | node::LOG
            | node::EMIT_EVENT => true,
            node::CALL => self.flow_call[i],
            _ => false,
        }
    }
    fn is_pure(&self, i: usize) -> bool {
        !self.is_entry(i) && !self.is_flow(i)
    }
    fn targets(&self, i: usize, port: &str) -> &[usize] {
        self.out[i].get(port).map_or(&[], Vec::as_slice)
    }
    fn producer(&self, i: usize, port: &str) -> Option<(usize, &str)> {
        self.inc[i]
            .get(port)
            .and_then(|v| v.first())
            .map(|(p, pp)| (*p, pp.as_str()))
    }
    fn diag(&mut self, code: &str, path: String, message: impl Into<String>) {
        self.diags.push(GraphDiagnostic::new(code, path, message));
    }
    fn node_path(&self, i: usize) -> String {
        format!("/nodes/{}", self.n[i].id)
    }
    fn var_type(&self, name: &str) -> Option<ValueType> {
        self.vars.iter().find(|v| v.name == name).map(|v| v.ty)
    }
}

/// Flow-tree facts shared by the checks and the code generator.
struct Flow {
    /// Entry node owning each flow node.
    region: Vec<Option<usize>>,
    parent: Vec<Option<(usize, &'static str)>>,
    /// Flow nodes of each entry region in visit order.
    members: BTreeMap<usize, Vec<usize>>,
    /// Function signature by entry node: parameters and results.
    fsigs: BTreeMap<usize, FnShape>,
    /// Function name -> entry node.
    funcs: BTreeMap<String, usize>,
}

#[derive(Clone, Default, PartialEq)]
struct FnShape {
    params: Vec<(String, ValueType)>,
    results: Vec<(String, ValueType)>,
}

fn typed_ports(ports: &[Port], direction: PortDirection) -> Option<Vec<(String, ValueType)>> {
    ports
        .iter()
        .filter(|p| p.direction == direction)
        .map(|p| ValueType::from_port_type(&p.ty).map(|t| (p.name.clone(), t)))
        .collect()
}

fn build(
    doc: &GraphDocument,
    sigs: Option<&dyn Signatures>,
) -> Result<Program, Vec<GraphDiagnostic>> {
    let mut g = G::new(doc);
    let flow = analyze(&mut g);
    if !g.diags.is_empty() {
        return Err(g.diags);
    }
    let mut cg = Cg::new(&g, &flow, sigs);
    match cg.generate() {
        Ok(program) => Ok(program),
        Err(()) => Err(cg.diags),
    }
}

// ---------------------------------------------------------------------------------
// Analysis
// ---------------------------------------------------------------------------------

fn analyze(g: &mut G) -> Flow {
    let count = g.n.len();
    let mut flow = Flow {
        region: vec![None; count],
        parent: vec![None; count],
        members: BTreeMap::new(),
        fsigs: BTreeMap::new(),
        funcs: BTreeMap::new(),
    };
    check_node_params(g);

    // Flow tree: one parent per flow node, one edge per flow out port (except
    // `sequence.then`).
    for i in 0..count {
        for port_name in flow_outs(&g.n[i].ty) {
            let fanout = g.targets(i, port_name).len();
            if fanout > 1 && g.n[i].ty != node::SEQUENCE {
                let path = format!("/nodes/{}/ports/{port_name}", g.n[i].id);
                g.diag(
                    gc::FLOW_FANOUT,
                    path,
                    format!("the flow port {port_name:?} has {fanout} edges; use a sequence node"),
                );
            }
        }
        if g.is_flow(i) && g.inc[i].get(port::EXEC).is_some_and(|v| v.len() > 1) {
            let path = format!("/nodes/{}/ports/exec", g.n[i].id);
            g.diag(
                gc::FLOW_FANOUT,
                path,
                "a node has one flow parent; share code with a function instead of merging flows",
            );
        }
    }
    let mut entries: Vec<usize> = (0..count).filter(|i| g.is_entry(*i)).collect();
    entries.sort_by(|a, b| g.n[*a].id.cmp(&g.n[*b].id));
    for &entry in &entries {
        flow.region[entry] = Some(entry);
        let mut members = vec![entry];
        let mut stack = vec![entry];
        while let Some(u) = stack.pop() {
            let ty = g.n[u].ty.clone();
            for &port_name in flow_outs(&ty).iter().rev() {
                for &t in g.targets(u, port_name).iter().rev() {
                    if flow.region[t].is_some() || !g.is_flow(t) {
                        continue;
                    }
                    flow.region[t] = Some(entry);
                    flow.parent[t] = Some((u, port_name));
                    members.push(t);
                    stack.push(t);
                }
            }
        }
        flow.members.insert(entry, members);
    }
    for i in 0..count {
        if g.is_flow(i) && flow.region[i].is_none() {
            let path = g.node_path(i);
            g.diag(
                gc::UNREACHABLE,
                path,
                "no entry node reaches this flow node",
            );
        }
    }
    if !g.n.iter().any(|n| {
        matches!(
            n.ty.as_str(),
            node::ON_START | node::ON_UPDATE | node::ON_EVENT | node::ON_STOP
        )
    }) {
        g.diag(
            gc::ENTRY_MISSING,
            String::new(),
            "the graph has no on_start, on_update, on_event or on_stop entry",
        );
    }
    check_functions(g, &mut flow);
    check_recursion(g, &flow);
    check_reads(g, &flow);
    flow
}

fn check_node_params(g: &mut G) {
    for i in 0..g.n.len() {
        let id = g.n[i].id.clone();
        match g.n[i].ty.as_str() {
            node::ON_EVENT | node::EMIT_EVENT => {
                if !valid_event_name(g.n[i].str("event")) {
                    g.diag(
                        gc::EVENT_NAME,
                        format!("/nodes/{id}/params/event"),
                        "event names are lower-case dotted segments of [a-z0-9_], at most 128 bytes",
                    );
                }
            }
            node::CONST => {
                let ty = ValueType::parse(g.n[i].str("type")).unwrap_or(ValueType::Float);
                let value = g.n[i].params.get("value").cloned().unwrap_or_default();
                if Value::from_json(ty, &value).is_none() {
                    g.diag(
                        gc::CONST_TYPE,
                        format!("/nodes/{id}/params/value"),
                        format!("the value is not a finite, in-limit {}", ty.name()),
                    );
                }
            }
            node::FOR_RANGE => {
                let literal = !g.inc[i].contains_key("start") && !g.inc[i].contains_key("end");
                let span = g.n[i].int("end").saturating_sub(g.n[i].int("start"));
                if literal && span > MAX_STATIC_LOOP {
                    g.diag(
                        gc::LOOP_BOUND,
                        format!("/nodes/{id}"),
                        format!("a literal loop of {span} iterations exceeds {MAX_STATIC_LOOP}"),
                    );
                }
            }
            _ => {}
        }
    }
}

fn check_functions(g: &mut G, flow: &mut Flow) {
    let mut entries: Vec<usize> = (0..g.n.len())
        .filter(|i| g.n[*i].ty == node::FUNCTION_ENTRY)
        .collect();
    entries.sort_by(|a, b| g.n[*a].id.cmp(&g.n[*b].id));
    for &f in &entries {
        let name = g.n[f].str("name").to_owned();
        let path = format!("/nodes/{}/params/name", g.n[f].id);
        if !valid_name(&name) {
            g.diag(
                gc::FUNCTION_UNKNOWN,
                path,
                "a function name is 1-64 characters of letters, digits, '_', '-' or '.'",
            );
            continue;
        }
        if flow.funcs.contains_key(&name) {
            g.diag(
                gc::FUNCTION_DUPLICATE,
                path,
                format!("function {name:?} is defined twice"),
            );
            continue;
        }
        flow.funcs.insert(name.clone(), f);
        // Parameters: extra data out ports of the entry.
        let mut shape = FnShape::default();
        match typed_ports(&g.n[f].extras, PortDirection::Out).filter(|_| {
            g.n[f]
                .extras
                .iter()
                .all(|p| p.direction == PortDirection::Out)
        }) {
            Some(params) => shape.params = params,
            None => g.diag(
                gc::FUNCTION_PORTS,
                format!("/nodes/{}", g.n[f].id),
                "function_entry extra ports must be typed data out ports",
            ),
        }
        // Results: the extra in ports shared by every return node of the function.
        let mut first: Option<Vec<(String, ValueType)>> = None;
        let members = flow.members.get(&f).cloned().unwrap_or_default();
        let returns: Vec<usize> = members
            .iter()
            .copied()
            .filter(|r| g.n[*r].ty == node::RETURN)
            .collect();
        for r in returns {
            let Some(results) = typed_ports(&g.n[r].extras, PortDirection::In).filter(|_| {
                g.n[r]
                    .extras
                    .iter()
                    .all(|p| p.direction == PortDirection::In)
            }) else {
                let path = g.node_path(r);
                g.diag(
                    gc::FUNCTION_PORTS,
                    path,
                    "return extra ports must be typed data in ports",
                );
                continue;
            };
            match &first {
                None => first = Some(results),
                Some(expected) if *expected != results => {
                    let path = g.node_path(r);
                    g.diag(
                        gc::FUNCTION_PORTS,
                        path,
                        "every return node of a function has the same extra ports",
                    );
                }
                Some(_) => {}
            }
        }
        shape.results = first.unwrap_or_default();
        flow.fsigs.insert(f, shape);
    }
    for i in 0..g.n.len() {
        match g.n[i].ty.as_str() {
            node::RETURN => {
                let in_function = flow.region[i].is_some_and(|e| g.n[e].ty == node::FUNCTION_ENTRY);
                if !in_function && !g.n[i].extras.is_empty() {
                    let path = g.node_path(i);
                    g.diag(
                        gc::FUNCTION_PORTS,
                        path,
                        "return outside a function takes no extra ports",
                    );
                }
            }
            node::WAIT => {
                if flow.region[i].is_some_and(|e| g.n[e].ty == node::FUNCTION_ENTRY) {
                    let path = g.node_path(i);
                    g.diag(
                        gc::LATENT_IN_FUNCTION,
                        path,
                        "wait is latent and may not appear inside a function",
                    );
                }
            }
            node::CALL_FUNCTION => {
                let name = g.n[i].str("function").to_owned();
                let path = format!("/nodes/{}", g.n[i].id);
                let Some(&f) = flow.funcs.get(&name) else {
                    g.diag(
                        gc::FUNCTION_UNKNOWN,
                        format!("{path}/params/function"),
                        format!("the document defines no function {name:?}"),
                    );
                    continue;
                };
                let shape = flow.fsigs.get(&f).cloned().unwrap_or_default();
                let side = |direction: PortDirection| -> Vec<(String, Option<ValueType>)> {
                    g.n[i]
                        .extras
                        .iter()
                        .filter(|p| p.direction == direction)
                        .map(|p| (p.name.clone(), ValueType::from_port_type(&p.ty)))
                        .collect()
                };
                let typed = |list: &[(String, ValueType)]| -> Vec<(String, Option<ValueType>)> {
                    list.iter().map(|(n, t)| (n.clone(), Some(*t))).collect()
                };
                let ok = side(PortDirection::In) == typed(&shape.params)
                    && side(PortDirection::Out) == typed(&shape.results);
                if !ok {
                    g.diag(
                        gc::FUNCTION_PORTS,
                        path,
                        format!(
                            "ports do not match function {name:?}: in ports are its parameters, out ports its results"
                        ),
                    );
                }
            }
            _ => {}
        }
    }
}

fn check_recursion(g: &mut G, flow: &Flow) {
    let mut callees: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..g.n.len() {
        if g.n[i].ty != node::CALL_FUNCTION {
            continue;
        }
        let (Some(region), Some(&target)) =
            (flow.region[i], flow.funcs.get(g.n[i].str("function")))
        else {
            continue;
        };
        if g.n[region].ty == node::FUNCTION_ENTRY {
            callees.entry(region).or_default().push(target);
        }
    }
    let mut color: BTreeMap<usize, u8> = BTreeMap::new();
    let mut reported = BTreeSet::new();
    for &start in flow.funcs.values() {
        if color.contains_key(&start) {
            continue;
        }
        color.insert(start, 1);
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        while let Some(&(node_idx, next)) = stack.last() {
            let children = callees.get(&node_idx).map_or(&[][..], Vec::as_slice);
            if let Some(&child) = children.get(next) {
                stack.last_mut().expect("non-empty").1 += 1;
                match color.get(&child) {
                    None => {
                        color.insert(child, 1);
                        stack.push((child, 0));
                    }
                    Some(1) => {
                        if reported.insert(child) {
                            let path = format!("/nodes/{}", g.n[child].id);
                            g.diag(
                                gc::FUNCTION_RECURSIVE,
                                path,
                                "functions may not call themselves, directly or indirectly",
                            );
                        }
                    }
                    Some(_) => {}
                }
            } else {
                color.insert(node_idx, 2);
                stack.pop();
            }
        }
    }
}

/// Whether `anc` is a strict flow ancestor of `node`; the port of `anc` leading
/// toward `node` when it is.
fn ancestor_via(flow: &Flow, anc: usize, node: usize) -> Option<&'static str> {
    let mut cursor = flow.parent[node];
    while let Some((p, via)) = cursor {
        if p == anc {
            return Some(via);
        }
        cursor = flow.parent[p];
    }
    None
}

fn data_inputs(g: &G, i: usize) -> Vec<(String, usize, String)> {
    let mut found = Vec::new();
    for port_def in &g.n[i].ports {
        if port_def.direction != PortDirection::In || port_def.ty == PortType::Flow {
            continue;
        }
        for (p, pp) in g.inc[i].get(&port_def.name).into_iter().flatten() {
            found.push((port_def.name.clone(), *p, pp.clone()));
        }
    }
    found
}

fn check_reads(g: &mut G, flow: &Flow) {
    let mut problems = Vec::new();
    for (&entry, members) in &flow.members {
        for &reader in members.iter().filter(|m| !g.is_entry(**m)) {
            let mut stack: Vec<(usize, String, usize, String)> = data_inputs(g, reader)
                .into_iter()
                .map(|(port_name, p, pp)| (reader, port_name, p, pp))
                .collect();
            let mut seen = BTreeSet::new();
            while let Some((consumer, in_port, prod, prod_port)) = stack.pop() {
                let ok = if g.is_entry(prod) {
                    prod == entry
                } else if g.is_flow(prod) {
                    match ancestor_via(flow, prod, reader) {
                        Some(via) => {
                            !(g.n[prod].ty == node::FOR_RANGE
                                && prod_port == port::INDEX
                                && via != port::BODY)
                        }
                        None => false,
                    }
                } else {
                    if seen.insert(prod) {
                        for (port_name, p, pp) in data_inputs(g, prod) {
                            stack.push((prod, port_name, p, pp));
                        }
                    }
                    true
                };
                if !ok {
                    problems.push((consumer, in_port, prod, prod_port));
                }
            }
        }
    }
    problems.sort();
    problems.dedup();
    for (consumer, in_port, prod, prod_port) in problems {
        let path = format!("/nodes/{}/ports/{in_port}", g.n[consumer].id);
        let message = format!(
            "{}.{prod_port} is not available here: a flow output is readable only below its node \
             (a loop index only inside its body) and an entry output only in its own entry",
            g.n[prod].id
        );
        g.diag(gc::VALUE_UNAVAILABLE, path, message);
    }
}

// ---------------------------------------------------------------------------------
// Code generation
// ---------------------------------------------------------------------------------

enum Task {
    Chain(Option<usize>, u32),
    Place(u32),
    Emit(Op),
}

struct Cg<'a> {
    g: &'a G,
    flow: &'a Flow,
    sigs: Option<&'a dyn Signatures>,
    diags: Vec<GraphDiagnostic>,
    code: Vec<Op>,
    sites: Vec<u32>,
    consts: Vec<Value>,
    labels: Vec<Option<usize>>,
    flags: usize,
    hosts: BTreeSet<String>,
    func_ids: BTreeMap<String, u32>,
    // Per region.
    persist: BTreeMap<(usize, String), Reg>,
    next_reg: usize,
    temp_base: usize,
    temp_top: usize,
    max_regs: usize,
    memo: BTreeMap<usize, Reg>,
    cur_site: u32,
}

impl<'a> Cg<'a> {
    fn new(g: &'a G, flow: &'a Flow, sigs: Option<&'a dyn Signatures>) -> Self {
        let func_ids = flow
            .funcs
            .keys()
            .enumerate()
            .map(|(i, name)| (name.clone(), i as u32))
            .collect();
        Self {
            g,
            flow,
            sigs,
            diags: Vec::new(),
            code: Vec::new(),
            sites: Vec::new(),
            consts: Vec::new(),
            labels: Vec::new(),
            flags: 0,
            hosts: BTreeSet::new(),
            func_ids,
            persist: BTreeMap::new(),
            next_reg: 0,
            temp_base: 0,
            temp_top: 0,
            max_regs: 0,
            memo: BTreeMap::new(),
            cur_site: 0,
        }
    }

    fn fail<T>(&mut self, code: &str, path: String, message: impl Into<String>) -> R<T> {
        self.diags.push(GraphDiagnostic::new(code, path, message));
        Err(())
    }

    fn push(&mut self, op: Op) -> R<()> {
        if self.code.len() >= MAX_OPS {
            return self.fail(
                gc::CODE_SIZE,
                String::new(),
                format!("the compiled script exceeds {MAX_OPS} ops"),
            );
        }
        self.code.push(op);
        self.sites.push(self.cur_site);
        Ok(())
    }

    fn label(&mut self) -> u32 {
        self.labels.push(None);
        (self.labels.len() - 1) as u32
    }
    fn place(&mut self, label: u32) {
        self.labels[label as usize] = Some(self.code.len());
    }
    fn konst(&mut self, value: Value) -> u32 {
        self.consts.push(value);
        (self.consts.len() - 1) as u32
    }

    fn temp(&mut self) -> R<Reg> {
        self.temps(1)
    }
    fn temps(&mut self, count: usize) -> R<Reg> {
        let reg = self.temp_top;
        self.temp_top += count;
        self.max_regs = self.max_regs.max(self.temp_top);
        if self.temp_top > MAX_REGS {
            return self.fail(
                gc::CODE_SIZE,
                String::new(),
                format!("a frame needs more than {MAX_REGS} registers"),
            );
        }
        Ok(reg as Reg)
    }
    fn alloc_persist(&mut self, node_idx: usize, port_name: &str) -> R<Reg> {
        let reg = self.next_reg;
        self.next_reg += 1;
        if self.next_reg > MAX_REGS {
            return self.fail(
                gc::CODE_SIZE,
                String::new(),
                format!("a frame needs more than {MAX_REGS} registers"),
            );
        }
        self.persist
            .insert((node_idx, port_name.to_owned()), reg as Reg);
        Ok(reg as Reg)
    }

    fn zero(&mut self, ty: ValueType) -> R<Reg> {
        let dst = self.temp()?;
        let k = self.konst(ty.default_value());
        self.push(Op::Const { dst, k })?;
        Ok(dst)
    }

    // --- types ---------------------------------------------------------------------

    fn param_type(&self, i: usize) -> Option<ValueType> {
        ValueType::parse(self.g.n[i].str("type"))
    }

    fn out_type(&self, i: usize, port_name: &str) -> Option<ValueType> {
        let n = &self.g.n[i];
        match n.ty.as_str() {
            node::ON_UPDATE => Some(ValueType::Float),
            node::ON_EVENT => match port_name {
                "payload" => Some(ValueType::Json),
                _ => Some(ValueType::Entity),
            },
            node::FOR_RANGE => Some(ValueType::Int),
            node::GET_VAR => self.g.var_type(n.str("variable")),
            node::SELF => Some(ValueType::Entity),
            node::CONST
            | node::ADD
            | node::SUB
            | node::MUL
            | node::DIV
            | node::MIN
            | node::MAX
            | node::ABS
            | node::NEG
            | node::JSON_TO => self.param_type(i),
            node::TO_FLOAT | node::VEC3_BREAK | node::VEC3_DOT | node::VEC3_LENGTH => {
                Some(ValueType::Float)
            }
            node::COMPARE | node::AND | node::OR | node::NOT => Some(ValueType::Bool),
            node::VEC3_MAKE | node::VEC3_ADD | node::VEC3_SUB | node::VEC3_SCALE => {
                Some(ValueType::Vec3)
            }
            node::CONCAT => Some(ValueType::Str),
            node::TO_JSON => Some(ValueType::Json),
            // function_entry parameters, call and call_function results.
            _ => n
                .port(port_name)
                .and_then(|p| ValueType::from_port_type(&p.ty)),
        }
    }

    fn in_type(&self, i: usize, port_name: &str) -> Option<ValueType> {
        let n = &self.g.n[i];
        match (n.ty.as_str(), port_name) {
            (
                node::ADD
                | node::SUB
                | node::MUL
                | node::DIV
                | node::MIN
                | node::MAX
                | node::ABS
                | node::NEG
                | node::COMPARE
                | node::TO_JSON,
                _,
            ) => self.param_type(i),
            (node::SET_VAR, "value") => self.g.var_type(n.str("variable")),
            _ => n
                .port(port_name)
                .and_then(|p| ValueType::from_port_type(&p.ty)),
        }
    }

    fn reg_of(&mut self, p: usize, port_name: &str) -> R<Reg> {
        if self.g.is_pure(p) {
            let base = self.memo.get(&p).copied();
            let offset = match (self.g.n[p].ty.as_str(), port_name) {
                (node::VEC3_BREAK, "y") => 1,
                (node::VEC3_BREAK, "z") => 2,
                _ => 0,
            };
            match base {
                Some(base) => Ok(base + offset),
                None => self.fail(
                    gc::VALUE_UNAVAILABLE,
                    format!("/nodes/{}", self.g.n[p].id),
                    "internal: a value node was not evaluated",
                ),
            }
        } else {
            match self.persist.get(&(p, port_name.to_owned())) {
                Some(reg) => Ok(*reg),
                None => self.fail(
                    gc::VALUE_UNAVAILABLE,
                    format!("/nodes/{}/ports/{port_name}", self.g.n[p].id),
                    "internal: an output has no register in this frame",
                ),
            }
        }
    }

    /// Evaluates the value node `root` and everything it reads, children first.
    fn eval(&mut self, root: usize) -> R<()> {
        let mut stack = vec![(root, false)];
        while let Some((i, expanded)) = stack.pop() {
            if self.memo.contains_key(&i) || !self.g.is_pure(i) {
                continue;
            }
            if !expanded {
                stack.push((i, true));
                for (_, p, _) in data_inputs(self.g, i) {
                    stack.push((p, false));
                }
            } else {
                self.emit_pure(i)?;
            }
        }
        Ok(())
    }

    /// The register holding the value feeding `i.port`, converted to the port's
    /// type; `None` when nothing is connected.
    fn input(&mut self, i: usize, port_name: &str) -> R<Option<Reg>> {
        let Some((p, pp)) = self.g.producer(i, port_name) else {
            return Ok(None);
        };
        let pp = pp.to_owned();
        self.eval(p)?;
        let reg = self.reg_of(p, &pp)?;
        let actual = self.out_type(p, &pp);
        let expected = self.in_type(i, port_name);
        match (actual, expected) {
            (Some(a), Some(e)) if a == e => Ok(Some(reg)),
            (Some(ValueType::Int), Some(ValueType::Float)) => {
                let dst = self.temp()?;
                self.push(Op::Un {
                    dst,
                    op: UnOp::ToFloat,
                    a: reg,
                })?;
                Ok(Some(dst))
            }
            (Some(a), Some(e)) => self.fail(
                gc::TYPE_MISMATCH,
                format!("/nodes/{}/ports/{port_name}", self.g.n[i].id),
                format!("{} cannot feed {}", a.name(), e.name()),
            ),
            _ => Ok(Some(reg)),
        }
    }

    fn require(&mut self, i: usize, port_name: &str) -> R<Reg> {
        match self.input(i, port_name)? {
            Some(reg) => Ok(reg),
            None => self.fail(
                gc::VALUE_UNAVAILABLE,
                format!("/nodes/{}/ports/{port_name}", self.g.n[i].id),
                "a required input is not connected",
            ),
        }
    }

    /// An input, or the zero value of the port's type when unconnected.
    fn input_or_zero(&mut self, i: usize, port_name: &str) -> R<Reg> {
        match self.input(i, port_name)? {
            Some(reg) => Ok(reg),
            None => {
                let ty = self.in_type(i, port_name).unwrap_or(ValueType::Int);
                self.zero(ty)
            }
        }
    }

    fn emit_pure(&mut self, i: usize) -> R<()> {
        self.cur_site = i as u32;
        let n = &self.g.n[i];
        let ty = n.ty.clone();
        let bin = |op| Some(op);
        let op2 = match ty.as_str() {
            node::ADD => bin(BinOp::Add),
            node::SUB => bin(BinOp::Sub),
            node::MUL => bin(BinOp::Mul),
            node::DIV => bin(BinOp::Div),
            node::MIN => bin(BinOp::Min),
            node::MAX => bin(BinOp::Max),
            node::AND => bin(BinOp::And),
            node::OR => bin(BinOp::Or),
            node::CONCAT => bin(BinOp::Concat),
            node::VEC3_ADD => bin(BinOp::V3Add),
            node::VEC3_SUB => bin(BinOp::V3Sub),
            node::VEC3_DOT => bin(BinOp::V3Dot),
            _ => None,
        };
        let dst;
        if let Some(op) = op2 {
            let a = self.require(i, "a")?;
            let b = self.require(i, "b")?;
            dst = self.temp()?;
            self.push(Op::Bin { dst, op, a, b })?;
        } else {
            match ty.as_str() {
                node::GET_VAR => {
                    dst = self.temp()?;
                    let name = self.g.n[i].str("variable").to_owned();
                    let var = self.g.vars.iter().position(|v| v.name == name).unwrap_or(0) as u16;
                    self.push(Op::GetVar { dst, var })?;
                }
                node::SELF => {
                    dst = self.temp()?;
                    self.push(Op::SelfEntity { dst })?;
                }
                node::CONST => {
                    dst = self.temp()?;
                    let ty = self.param_type(i).unwrap_or(ValueType::Float);
                    let raw = self.g.n[i].params.get("value").cloned().unwrap_or_default();
                    let value = Value::from_json(ty, &raw).unwrap_or_else(|| ty.default_value());
                    let k = self.konst(value);
                    self.push(Op::Const { dst, k })?;
                }
                node::ABS | node::NEG | node::TO_FLOAT | node::NOT | node::VEC3_LENGTH => {
                    let port_name = if ty == node::VEC3_LENGTH { "v" } else { "a" };
                    let a = self.require(i, port_name)?;
                    dst = self.temp()?;
                    let op = match ty.as_str() {
                        node::ABS => UnOp::Abs,
                        node::NEG => UnOp::Neg,
                        node::TO_FLOAT => UnOp::ToFloat,
                        node::NOT => UnOp::Not,
                        _ => UnOp::V3Length,
                    };
                    self.push(Op::Un { dst, op, a })?;
                }
                node::COMPARE => {
                    let kind = self.param_type(i).unwrap_or(ValueType::Float);
                    let name = self.g.n[i].str("op").to_owned();
                    let op = match name.as_str() {
                        "eq" => BinOp::Eq,
                        "ne" => BinOp::Ne,
                        "lt" => BinOp::Lt,
                        "le" => BinOp::Le,
                        "gt" => BinOp::Gt,
                        _ => BinOp::Ge,
                    };
                    if !matches!(op, BinOp::Eq | BinOp::Ne)
                        && !matches!(kind, ValueType::Int | ValueType::Float | ValueType::Str)
                    {
                        return self.fail(
                            gc::TYPE_MISMATCH,
                            format!("/nodes/{}/params/op", self.g.n[i].id),
                            format!("{} values can only be compared with eq and ne", kind.name()),
                        );
                    }
                    let a = self.require(i, "a")?;
                    let b = self.require(i, "b")?;
                    dst = self.temp()?;
                    self.push(Op::Bin { dst, op, a, b })?;
                }
                node::VEC3_MAKE => {
                    let x = self.input_or_zero(i, "x")?;
                    let y = self.input_or_zero(i, "y")?;
                    let z = self.input_or_zero(i, "z")?;
                    dst = self.temp()?;
                    self.push(Op::Vec3Make { dst, x, y, z })?;
                }
                node::VEC3_BREAK => {
                    let src = self.require(i, "v")?;
                    dst = self.temps(3)?;
                    self.push(Op::Vec3Break { dst, src })?;
                }
                node::VEC3_SCALE => {
                    let a = self.require(i, "v")?;
                    let b = self.require(i, "s")?;
                    dst = self.temp()?;
                    self.push(Op::Bin {
                        dst,
                        op: BinOp::V3Scale,
                        a,
                        b,
                    })?;
                }
                node::TO_JSON => {
                    let src = self.require(i, "value")?;
                    dst = self.temp()?;
                    self.push(Op::ToJson { dst, src })?;
                }
                node::JSON_TO => {
                    let src = self.require(i, "value")?;
                    dst = self.temp()?;
                    let ty = self.param_type(i).unwrap_or(ValueType::Float);
                    self.push(Op::JsonTo { dst, src, ty })?;
                }
                node::CALL => {
                    let (args, _) = self.host_args(i)?;
                    dst = self.temp()?;
                    let (function, cost) = self.host_target(i)?;
                    self.push(Op::Host {
                        function,
                        args,
                        dst: Some(dst),
                        cost,
                    })?;
                }
                other => {
                    return self.fail(
                        gc::VALUE_UNAVAILABLE,
                        format!("/nodes/{}", self.g.n[i].id),
                        format!("internal: {other} is not a value node"),
                    );
                }
            }
        }
        self.memo.insert(i, dst);
        Ok(())
    }

    // --- host calls ----------------------------------------------------------------

    /// The signature a `call` node is compiled against: the registered one, or one
    /// synthesized from the node's own ports when compiling without signatures.
    fn call_sig(&mut self, i: usize) -> R<(FnId, FnSig)> {
        let name = self.g.n[i].str("function").to_owned();
        let path = format!("/nodes/{}", self.g.n[i].id);
        match self.sigs {
            Some(sigs) => match sigs.lookup(&name) {
                Some((id, sig)) => Ok((id, sig.clone())),
                None => self.fail(
                    codes::CALL_UNKNOWN,
                    format!("{path}/params/function"),
                    format!("no host function {name:?} is declared"),
                ),
            },
            None => {
                let params: Vec<(String, ValueType)> =
                    typed_ports(&self.g.n[i].extras, PortDirection::In).unwrap_or_default();
                let returns = self.g.n[i]
                    .port(port::RESULT)
                    .and_then(|p| ValueType::from_port_type(&p.ty));
                let params: Vec<(&str, ValueType)> =
                    params.iter().map(|(n, t)| (n.as_str(), *t)).collect();
                Ok((FnId(u32::MAX), FnSig::new(&params, returns)))
            }
        }
    }

    fn host_target(&mut self, i: usize) -> R<(FnId, u32)> {
        let (id, sig) = self.call_sig(i)?;
        let name = self.g.n[i].str("function").to_owned();
        self.hosts.insert(name);
        Ok((id, sig.cost))
    }

    fn host_args(&mut self, i: usize) -> R<(Box<[Reg]>, Option<ValueType>)> {
        let (_, sig) = self.call_sig(i)?;
        let mut args = Vec::with_capacity(sig.params.len());
        for param in &sig.params {
            let reg = match self.input(i, &param.name)? {
                Some(reg) => reg,
                None => self.zero(param.ty)?,
            };
            args.push(reg);
        }
        Ok((args.into_boxed_slice(), sig.returns))
    }

    /// Checks every `call` node against its signature (needs signatures).
    fn check_calls(&mut self) -> R<()> {
        let Some(sigs) = self.sigs else {
            return Ok(());
        };
        let mut failed = false;
        for i in 0..self.g.n.len() {
            if self.g.n[i].ty != node::CALL {
                continue;
            }
            let name = self.g.n[i].str("function").to_owned();
            let path = format!("/nodes/{}", self.g.n[i].id);
            let Some((_, sig)) = sigs.lookup(&name) else {
                self.diags.push(GraphDiagnostic::new(
                    codes::CALL_UNKNOWN,
                    format!("{path}/params/function"),
                    format!("no host function {name:?} is declared"),
                ));
                failed = true;
                continue;
            };
            let mut expected: Vec<(String, PortDirection, ValueType)> = sig
                .params
                .iter()
                .map(|p| (p.name.clone(), PortDirection::In, p.ty))
                .collect();
            if let Some(ret) = sig.returns {
                expected.push((port::RESULT.to_owned(), PortDirection::Out, ret));
            }
            let mut actual: Vec<(String, PortDirection, Option<ValueType>)> = self.g.n[i]
                .extras
                .iter()
                .map(|p| {
                    (
                        p.name.clone(),
                        p.direction,
                        ValueType::from_port_type(&p.ty),
                    )
                })
                .collect();
            let mut want: Vec<(String, PortDirection, Option<ValueType>)> = expected
                .into_iter()
                .map(|(n, d, t)| (n, d, Some(t)))
                .collect();
            actual.sort();
            want.sort();
            let mut problem = None;
            if actual != want {
                problem = Some(format!(
                    "extra ports must match {name}: parameters as in ports, the return value as out port `result`"
                ));
            } else if sig.pure && self.g.flow_call[i] {
                problem = Some(format!("{name} is pure: it takes no flow edges"));
            } else if !sig.pure && !self.g.flow_call[i] {
                problem = Some(format!(
                    "{name} has side effects or is not pure: connect it to the flow"
                ));
            }
            if let Some(message) = problem {
                self.diags
                    .push(GraphDiagnostic::new(codes::CALL_PORTS, path, message));
                failed = true;
            }
        }
        if failed { Err(()) } else { Ok(()) }
    }

    // --- program -------------------------------------------------------------------

    fn generate(&mut self) -> Result<Program, ()> {
        self.check_calls()?;
        let g = self.g;
        let flow = self.flow;
        let mut functions: Vec<Option<FuncInfo>> = vec![None; flow.funcs.len()];
        for (name, &entry) in &flow.funcs {
            let (pc, n_regs) = self.compile_region(entry)?;
            let shape = flow.fsigs.get(&entry).cloned().unwrap_or_default();
            let params = shape
                .params
                .iter()
                .map(|(port_name, ty)| (self.persist_reg(entry, port_name), *ty))
                .collect();
            let index = self.func_ids[name] as usize;
            functions[index] = Some(FuncInfo {
                pc,
                n_regs,
                params,
                results: shape.results.iter().map(|(_, t)| *t).collect(),
            });
        }
        let mut entries: BTreeMap<EntryKey, Vec<Region>> = BTreeMap::new();
        let mut order: Vec<usize> = flow
            .members
            .keys()
            .copied()
            .filter(|e| g.n[*e].ty != node::FUNCTION_ENTRY)
            .collect();
        order.sort_by(|a, b| g.n[*a].id.cmp(&g.n[*b].id));
        let mut subscriptions = BTreeSet::new();
        for entry in order {
            let key = match g.n[entry].ty.as_str() {
                node::ON_START => EntryKey::Start,
                node::ON_UPDATE => EntryKey::Update,
                node::ON_STOP => EntryKey::Stop,
                _ => {
                    let event = g.n[entry].str("event").to_owned();
                    subscriptions.insert(event.clone());
                    EntryKey::Event(event)
                }
            };
            let (pc, n_regs) = self.compile_region(entry)?;
            let region = Region {
                pc,
                n_regs,
                dt: self.persist.get(&(entry, "dt".to_owned())).copied(),
                payload: self.persist.get(&(entry, "payload".to_owned())).copied(),
                source: self.persist.get(&(entry, "source".to_owned())).copied(),
            };
            entries.entry(key).or_default().push(region);
        }
        // Resolve labels to op indices.
        let labels = std::mem::take(&mut self.labels);
        let resolve = |l: u32| labels[l as usize].unwrap_or(0) as u32;
        for op in &mut self.code {
            match op {
                Op::Jump(l) => *l = resolve(*l),
                Op::JumpIfNot { to, .. } | Op::DoOnce { to, .. } => *to = resolve(*to),
                _ => {}
            }
        }
        Ok(Program {
            code: std::mem::take(&mut self.code),
            sites: std::mem::take(&mut self.sites),
            node_names: g.n.iter().map(|n| n.id.clone()).collect(),
            consts: std::mem::take(&mut self.consts),
            vars: g.vars.clone(),
            entries,
            functions: functions.into_iter().flatten().collect(),
            flags: self.flags,
            subscriptions: subscriptions.into_iter().collect(),
            host_functions: self.hosts.iter().cloned().collect(),
            resolved: self.sigs.is_some(),
        })
    }

    fn persist_reg(&self, node_idx: usize, port_name: &str) -> Reg {
        self.persist
            .get(&(node_idx, port_name.to_owned()))
            .copied()
            .unwrap_or(0)
    }

    /// Compiles one entry or function region; returns `(pc, frame size)`.
    fn compile_region(&mut self, entry: usize) -> R<(usize, usize)> {
        self.persist.clear();
        self.memo.clear();
        self.next_reg = 0;
        self.max_regs = 0;
        let members = self.flow.members.get(&entry).cloned().unwrap_or_default();
        // Persistent registers: entry outputs, loop state and call results.
        for &m in &members {
            let n = &self.g.n[m];
            let ty = n.ty.clone();
            let outs: Vec<String> = match ty.as_str() {
                node::ON_UPDATE => vec!["dt".to_owned()],
                node::ON_EVENT => vec!["payload".to_owned(), "source".to_owned()],
                node::FUNCTION_ENTRY | node::CALL_FUNCTION | node::CALL => n
                    .extras
                    .iter()
                    .filter(|p| p.direction == PortDirection::Out)
                    .map(|p| p.name.clone())
                    .collect(),
                node::FOR_RANGE => vec![port::INDEX.to_owned(), "$end".to_owned()],
                node::WHILE => vec!["$count".to_owned()],
                _ => Vec::new(),
            };
            for name in outs {
                self.alloc_persist(m, &name)?;
            }
        }
        self.temp_base = self.next_reg;
        self.temp_top = self.temp_base;
        self.max_regs = self.temp_base;

        let pc = self.code.len();
        let end = self.label();
        self.cur_site = entry as u32;
        let first = self.g.targets(entry, port::THEN).first().copied();
        let mut tasks = vec![
            Task::Emit(Op::End),
            Task::Place(end),
            Task::Chain(first, end),
        ];
        while let Some(task) = tasks.pop() {
            match task {
                Task::Place(label) => self.place(label),
                Task::Emit(op) => self.push(op)?,
                Task::Chain(None, k) => self.push(Op::Jump(k))?,
                Task::Chain(Some(i), k) => {
                    self.temp_top = self.temp_base;
                    self.memo.clear();
                    self.cur_site = i as u32;
                    self.emit_flow(i, k, &mut tasks)?;
                }
            }
        }
        Ok((pc, self.max_regs))
    }

    fn follow(&self, i: usize, port_name: &str, k: u32, tasks: &mut Vec<Task>) {
        tasks.push(Task::Chain(
            self.g.targets(i, port_name).first().copied(),
            k,
        ));
    }

    fn emit_flow(&mut self, i: usize, k: u32, tasks: &mut Vec<Task>) -> R<()> {
        let ty = self.g.n[i].ty.clone();
        match ty.as_str() {
            node::SET_VAR => {
                let name = self.g.n[i].str("variable").to_owned();
                let var = self.g.vars.iter().position(|v| v.name == name).unwrap_or(0) as u16;
                let src = self.require(i, "value")?;
                self.push(Op::SetVar { var, src })?;
                self.follow(i, port::THEN, k, tasks);
            }
            node::LOG => {
                let src = self.require(i, "message")?;
                self.push(Op::Log { src })?;
                self.follow(i, port::THEN, k, tasks);
            }
            node::EMIT_EVENT => {
                let target = self.input(i, "target")?;
                let payload = self.g.n[i]
                    .params
                    .get("payload")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let payload = self.konst(Value::json(payload));
                let event: Arc<str> = Arc::from(self.g.n[i].str("event"));
                self.push(Op::Emit {
                    event,
                    payload,
                    target,
                })?;
                self.follow(i, port::THEN, k, tasks);
            }
            node::WAIT => {
                let secs = match self.input(i, "seconds")? {
                    Some(reg) => reg,
                    None => {
                        let dst = self.temp()?;
                        let value = Value::Float(self.g.n[i].float("seconds"));
                        let k = self.konst(value);
                        self.push(Op::Const { dst, k })?;
                        dst
                    }
                };
                self.push(Op::Wait { secs })?;
                self.follow(i, port::THEN, k, tasks);
            }
            node::CALL_FUNCTION => {
                let name = self.g.n[i].str("function").to_owned();
                let func = self.func_ids[&name];
                let mut args = Vec::new();
                let ins: Vec<String> = self.g.n[i]
                    .extras
                    .iter()
                    .filter(|p| p.direction == PortDirection::In)
                    .map(|p| p.name.clone())
                    .collect();
                for port_name in ins {
                    args.push(self.input_or_zero(i, &port_name)?);
                }
                let rets: Vec<Reg> = self.g.n[i]
                    .extras
                    .iter()
                    .filter(|p| p.direction == PortDirection::Out)
                    .map(|p| self.persist_reg(i, &p.name))
                    .collect();
                self.push(Op::CallFn {
                    func,
                    args: args.into_boxed_slice(),
                    rets: rets.into_boxed_slice(),
                })?;
                self.follow(i, port::THEN, k, tasks);
            }
            node::CALL => {
                let (args, returns) = self.host_args(i)?;
                let (function, cost) = self.host_target(i)?;
                let dst = returns.map(|_| self.persist_reg(i, port::RESULT));
                self.push(Op::Host {
                    function,
                    args,
                    dst,
                    cost,
                })?;
                self.follow(i, port::THEN, k, tasks);
            }
            node::RETURN => {
                let ins: Vec<String> = self.g.n[i]
                    .extras
                    .iter()
                    .filter(|p| p.direction == PortDirection::In)
                    .map(|p| p.name.clone())
                    .collect();
                let mut srcs = Vec::new();
                for port_name in ins {
                    srcs.push(self.input_or_zero(i, &port_name)?);
                }
                self.push(Op::Return {
                    srcs: srcs.into_boxed_slice(),
                })?;
            }
            node::BRANCH => {
                let cond = self.require(i, "condition")?;
                let otherwise = self.label();
                self.push(Op::JumpIfNot {
                    cond,
                    to: otherwise,
                })?;
                let t = self.g.targets(i, port::TRUE).first().copied();
                let f = self.g.targets(i, port::FALSE).first().copied();
                tasks.push(Task::Chain(f, k));
                tasks.push(Task::Place(otherwise));
                tasks.push(Task::Chain(t, k));
            }
            node::SEQUENCE => {
                let children: Vec<usize> = self.g.targets(i, port::THEN).to_vec();
                if children.is_empty() {
                    tasks.push(Task::Chain(None, k));
                } else {
                    let labels: Vec<u32> = (1..children.len()).map(|_| self.label()).collect();
                    for (index, &child) in children.iter().enumerate().rev() {
                        let next = labels.get(index).copied().unwrap_or(k);
                        tasks.push(Task::Chain(Some(child), next));
                        if index > 0 {
                            tasks.push(Task::Place(labels[index - 1]));
                        }
                    }
                }
            }
            node::DO_ONCE => {
                let flag = self.flags as u16;
                self.flags += 1;
                let skip = self.label();
                self.push(Op::DoOnce { flag, to: skip })?;
                tasks.push(Task::Chain(None, k));
                tasks.push(Task::Place(skip));
                self.follow(i, port::THEN, k, tasks);
            }
            node::GATE => {
                let open = match self.input(i, "open")? {
                    Some(reg) => reg,
                    None => {
                        let dst = self.temp()?;
                        let value = Value::Bool(self.g.n[i].bool("open"));
                        let kk = self.konst(value);
                        self.push(Op::Const { dst, k: kk })?;
                        dst
                    }
                };
                let flag = self.flags as u16;
                self.flags += 1;
                let dst = self.temp()?;
                let latch = self.g.n[i].bool("latch");
                self.push(Op::Gate {
                    flag,
                    latch,
                    open,
                    dst,
                })?;
                let closed = self.label();
                self.push(Op::JumpIfNot {
                    cond: dst,
                    to: closed,
                })?;
                let then = self.g.targets(i, port::THEN).first().copied();
                let shut = self.g.targets(i, port::CLOSED).first().copied();
                tasks.push(Task::Chain(shut, k));
                tasks.push(Task::Place(closed));
                tasks.push(Task::Chain(then, k));
            }
            node::FOR_RANGE => {
                let counter = self.persist_reg(i, port::INDEX);
                let end_reg = self.persist_reg(i, "$end");
                for (port_name, dst) in [("start", counter), ("end", end_reg)] {
                    let src = match self.input(i, port_name)? {
                        Some(reg) => reg,
                        None => {
                            let tmp = self.temp()?;
                            let value = Value::Int(self.g.n[i].int(port_name));
                            let kk = self.konst(value);
                            self.push(Op::Const { dst: tmp, k: kk })?;
                            tmp
                        }
                    };
                    self.push(Op::Move { dst, src })?;
                }
                let (check, done, next) = (self.label(), self.label(), self.label());
                self.place(check);
                let t = self.temp()?;
                self.push(Op::Bin {
                    dst: t,
                    op: BinOp::Lt,
                    a: counter,
                    b: end_reg,
                })?;
                self.push(Op::JumpIfNot { cond: t, to: done })?;
                let body = self.g.targets(i, port::BODY).first().copied();
                let completed = self.g.targets(i, port::COMPLETED).first().copied();
                tasks.push(Task::Chain(completed, k));
                tasks.push(Task::Place(done));
                tasks.push(Task::Emit(Op::Jump(check)));
                tasks.push(Task::Emit(Op::Inc { reg: counter }));
                tasks.push(Task::Place(next));
                tasks.push(Task::Chain(body, next));
            }
            node::WHILE => {
                let counter = self.persist_reg(i, "$count");
                let dst = counter;
                let kk = self.konst(Value::Int(0));
                self.push(Op::Const { dst, k: kk })?;
                let (check, done, back) = (self.label(), self.label(), self.label());
                self.place(check);
                let cond = self.require(i, "condition")?;
                self.push(Op::JumpIfNot { cond, to: done })?;
                self.push(Op::Inc { reg: counter })?;
                self.push(Op::LoopLimit {
                    reg: counter,
                    max: self.g.n[i].int("max_iterations").clamp(1, 10_000) as u32,
                })?;
                let body = self.g.targets(i, port::BODY).first().copied();
                let completed = self.g.targets(i, port::COMPLETED).first().copied();
                tasks.push(Task::Chain(completed, k));
                tasks.push(Task::Place(done));
                tasks.push(Task::Emit(Op::Jump(check)));
                tasks.push(Task::Place(back));
                tasks.push(Task::Chain(body, back));
            }
            other => {
                return self.fail(
                    gc::UNREACHABLE,
                    format!("/nodes/{}", self.g.n[i].id),
                    format!("internal: {other} is not a flow node"),
                );
            }
        }
        Ok(())
    }
}
