//! Pure, atomic edit operations over a [`GraphDocument`].
//!
//! Every operation is applied to a copy. The copy is checked with
//! [`Strictness::Structure`] and the edit is rejected when it introduces a
//! structural diagnostic that the document did not already have; only then does
//! the copy replace the original. A rejected edit therefore leaves the document
//! byte-for-byte unchanged. [`apply_edits`] applies a list the same way and, by
//! default, also requires the final document to pass full validation.
use crate::{
    diagnostic::{GraphDiagnostic, codes},
    kind::GraphKind,
    model::{GraphDocument, GraphEdge, GraphNode, GraphVariable, NodeId, Port, PortRef},
    validate::{Strictness, validate},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Most operations accepted in one batch.
pub const MAX_EDITS: usize = 1024;

/// One edit operation. JSON form: `{"op":"add_node","id":"a","node_type":"wait"}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum GraphEdit {
    /// Adds a node. Omitted ports are copied from the registry declaration.
    AddNode {
        id: NodeId,
        node_type: String,
        #[serde(default)]
        params: BTreeMap<String, Value>,
        #[serde(default)]
        position: Option<[f32; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ports: Option<Vec<Port>>,
    },
    /// Removes a node and every edge attached to it.
    RemoveNode {
        id: NodeId,
    },
    /// Adds an edge. `index` is the position among the edges already leaving
    /// `from` (its child or execution order); omitted means last.
    Connect {
        from: PortRef,
        to: PortRef,
        #[serde(default)]
        index: Option<usize>,
    },
    Disconnect {
        from: PortRef,
        to: PortRef,
    },
    /// Sets one parameter; a `null` value removes it (the default applies).
    SetParam {
        node: NodeId,
        name: String,
        value: Value,
    },
    SetPosition {
        node: NodeId,
        position: Option<[f32; 2]>,
    },
    /// Replaces the ports of an extensible node, including its declared ports.
    SetPorts {
        node: NodeId,
        ports: Vec<Port>,
    },
    AddVariable {
        variable: GraphVariable,
    },
    RemoveVariable {
        name: String,
    },
    /// Replaces the variable of the same name.
    SetVariable {
        variable: GraphVariable,
    },
    /// Sets one metadata entry; a `null` value removes it.
    SetMetadata {
        key: String,
        value: Value,
    },
}

/// Why an edit or batch was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditFailure {
    /// Zero-based index of the offending operation in a batch; `None` when the
    /// batch is fine operation by operation but the final document is incomplete.
    pub op_index: Option<usize>,
    pub diagnostics: Vec<GraphDiagnostic>,
}

/// Applies one operation atomically, checking structure only.
///
/// # Errors
/// Returns the diagnostics that make the edit invalid; `doc` is then unchanged.
pub fn apply_edit(
    doc: &mut GraphDocument,
    edit: &GraphEdit,
    kind: &GraphKind,
) -> Result<(), Vec<GraphDiagnostic>> {
    let baseline: BTreeSet<_> = validate(doc, kind, Strictness::Structure)
        .into_iter()
        .map(|d| (d.code, d.path))
        .collect();
    let mut candidate = doc.clone();
    mutate(&mut candidate, edit, kind)?;
    let introduced: Vec<_> = validate(&candidate, kind, Strictness::Structure)
        .into_iter()
        .filter(|d| !baseline.contains(&(d.code.clone(), d.path.clone())))
        .collect();
    if introduced.is_empty() {
        *doc = candidate;
        Ok(())
    } else {
        Err(introduced)
    }
}

/// Applies `edits` in order as one atomic batch.
///
/// With `Strictness::Complete` the final document must also pass full validation,
/// which is the right setting before saving an asset; `Strictness::Structure`
/// allows saving work in progress. On any failure `doc` is unchanged.
///
/// # Errors
/// Returns [`EditFailure`] naming the first failing operation (or the final
/// completeness diagnostics).
pub fn apply_edits(
    doc: &mut GraphDocument,
    edits: &[GraphEdit],
    kind: &GraphKind,
    level: Strictness,
) -> Result<(), EditFailure> {
    if edits.len() > MAX_EDITS {
        return Err(EditFailure {
            op_index: None,
            diagnostics: vec![GraphDiagnostic::new(
                codes::LIMIT_EDITS,
                "",
                format!(
                    "{} edits exceed the batch limit of {MAX_EDITS}",
                    edits.len()
                ),
            )],
        });
    }
    let mut working = doc.clone();
    for (index, edit) in edits.iter().enumerate() {
        apply_edit(&mut working, edit, kind).map_err(|diagnostics| EditFailure {
            op_index: Some(index),
            diagnostics,
        })?;
    }
    if level == Strictness::Complete {
        let diagnostics = validate(&working, kind, Strictness::Complete);
        if !diagnostics.is_empty() {
            return Err(EditFailure {
                op_index: None,
                diagnostics,
            });
        }
    }
    *doc = working;
    Ok(())
}

fn fail(code: &str, path: String, message: impl Into<String>) -> Vec<GraphDiagnostic> {
    vec![GraphDiagnostic::new(code, path, message)]
}

fn node_index(doc: &GraphDocument, id: &NodeId) -> Result<usize, Vec<GraphDiagnostic>> {
    doc.nodes.iter().position(|n| n.id == *id).ok_or_else(|| {
        fail(
            codes::EDIT_NODE_MISSING,
            format!("/nodes/{id}"),
            "no such node",
        )
    })
}

fn mutate(
    doc: &mut GraphDocument,
    edit: &GraphEdit,
    kind: &GraphKind,
) -> Result<(), Vec<GraphDiagnostic>> {
    match edit {
        GraphEdit::AddNode {
            id,
            node_type,
            params,
            position,
            ports,
        } => {
            let def = kind.registry.get(node_type).ok_or_else(|| {
                fail(
                    codes::NODE_UNKNOWN_TYPE,
                    format!("/nodes/{id}"),
                    format!("kind {} has no node type {node_type:?}", kind.name),
                )
            })?;
            let ports = authored_ports(def, ports.as_deref(), id)?;
            doc.nodes.push(GraphNode {
                id: id.clone(),
                node_type: node_type.clone(),
                ports,
                params: params.clone(),
                position: *position,
            });
        }
        GraphEdit::RemoveNode { id } => {
            let index = node_index(doc, id)?;
            doc.nodes.remove(index);
            doc.edges.retain(|e| e.from.node != *id && e.to.node != *id);
        }
        GraphEdit::Connect { from, to, index } => {
            let leaving: Vec<usize> = doc
                .edges
                .iter()
                .enumerate()
                .filter(|(_, e)| e.from == *from)
                .map(|(i, _)| i)
                .collect();
            let edge = GraphEdge {
                from: from.clone(),
                to: to.clone(),
            };
            match index {
                None => doc.edges.push(edge),
                Some(i) if *i == leaving.len() => doc.edges.push(edge),
                Some(i) if *i < leaving.len() => doc.edges.insert(leaving[*i], edge),
                Some(i) => {
                    return Err(fail(
                        codes::EDIT_INDEX,
                        crate::validate::edge_path(&edge),
                        format!(
                            "index {i} exceeds the {} edges leaving the port",
                            leaving.len()
                        ),
                    ));
                }
            }
        }
        GraphEdit::Disconnect { from, to } => {
            let position = doc
                .edges
                .iter()
                .position(|e| e.from == *from && e.to == *to)
                .ok_or_else(|| {
                    fail(
                        codes::EDIT_EDGE_MISSING,
                        crate::validate::edge_path(&GraphEdge {
                            from: from.clone(),
                            to: to.clone(),
                        }),
                        "no such edge",
                    )
                })?;
            doc.edges.remove(position);
        }
        GraphEdit::SetParam { node, name, value } => {
            let index = node_index(doc, node)?;
            if value.is_null() {
                doc.nodes[index].params.remove(name);
            } else {
                doc.nodes[index].params.insert(name.clone(), value.clone());
            }
        }
        GraphEdit::SetPosition { node, position } => {
            let index = node_index(doc, node)?;
            doc.nodes[index].position = *position;
        }
        GraphEdit::SetPorts { node, ports } => {
            let index = node_index(doc, node)?;
            let def = kind
                .registry
                .get(&doc.nodes[index].node_type)
                .ok_or_else(|| {
                    fail(
                        codes::NODE_UNKNOWN_TYPE,
                        format!("/nodes/{node}"),
                        "node type is not registered",
                    )
                })?;
            doc.nodes[index].ports = authored_ports(def, Some(ports), node)?;
        }
        GraphEdit::AddVariable { variable } => doc.variables.push(variable.clone()),
        GraphEdit::RemoveVariable { name } => {
            let index = variable_index(doc, name)?;
            doc.variables.remove(index);
        }
        GraphEdit::SetVariable { variable } => {
            let index = variable_index(doc, &variable.name)?;
            doc.variables[index] = variable.clone();
        }
        GraphEdit::SetMetadata { key, value } => {
            if value.is_null() {
                doc.metadata.remove(key);
            } else {
                doc.metadata.insert(key.clone(), value.clone());
            }
        }
    }
    Ok(())
}

fn authored_ports(
    def: &crate::kind::NodeTypeDef,
    ports: Option<&[Port]>,
    node: &NodeId,
) -> Result<Vec<Port>, Vec<GraphDiagnostic>> {
    match ports {
        Some(ports) if def.extensible => {
            for declared in def.declared_ports() {
                if !ports.contains(&declared) {
                    return Err(fail(
                        codes::NODE_PORTS_MISMATCH,
                        format!("/nodes/{node}/ports/{}", declared.name),
                        format!("node type {} declares this port as {declared:?}", def.name),
                    ));
                }
            }
            Ok(ports.to_vec())
        }
        Some(_) => Err(fail(
            codes::NODE_PORTS_MISMATCH,
            format!("/nodes/{node}/ports"),
            format!("node type {} does not allow authored ports", def.name),
        )),
        None => Ok(def.declared_ports()),
    }
}

fn variable_index(doc: &GraphDocument, name: &str) -> Result<usize, Vec<GraphDiagnostic>> {
    doc.variables
        .iter()
        .position(|v| v.name == name)
        .ok_or_else(|| {
            fail(
                codes::EDIT_VARIABLE_MISSING,
                format!("/variables/{name}"),
                "no such variable",
            )
        })
}
