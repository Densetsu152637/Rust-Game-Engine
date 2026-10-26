//! Coded diagnostics. Codes are stable API; messages are for humans.
use serde::{Deserialize, Serialize};

/// Most diagnostics one validation returns before it reports truncation.
pub const MAX_DIAGNOSTICS: usize = 100;

/// One problem found in a document or rejected edit.
///
/// `path` locates the problem by identity, not by array index, so it stays valid
/// while edits reorder arrays: `/nodes/<id>`, `/nodes/<id>/params/<name>`,
/// `/nodes/<id>/ports/<name>`, `/edges/<node>.<port>-><node>.<port>`,
/// `/variables/<name>`, `/metadata/<key>`. The empty path is the document.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GraphDiagnostic {
    pub code: String,
    pub path: String,
    pub message: String,
}

impl GraphDiagnostic {
    pub fn new(code: &str, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            path: path.into(),
            message: message.into(),
        }
    }
}

/// Every diagnostic code this crate emits.
pub mod codes {
    pub const DOCUMENT_MALFORMED: &str = "graph.document.malformed";
    pub const DOCUMENT_TOO_LARGE: &str = "graph.document.too_large";
    pub const DIAGNOSTICS_TRUNCATED: &str = "graph.diagnostics.truncated";
    pub const VERSION_UNSUPPORTED: &str = "graph.version.unsupported";
    pub const KIND_MISMATCH: &str = "graph.kind.mismatch";
    pub const LIMIT_NODES: &str = "graph.limit.nodes";
    pub const LIMIT_EDGES: &str = "graph.limit.edges";
    pub const LIMIT_VARIABLES: &str = "graph.limit.variables";
    pub const LIMIT_PORTS: &str = "graph.limit.ports";
    pub const LIMIT_PARAMS: &str = "graph.limit.params";
    pub const LIMIT_VALUE: &str = "graph.limit.value";
    pub const LIMIT_METADATA: &str = "graph.limit.metadata";
    pub const LIMIT_EDITS: &str = "graph.limit.edits";
    pub const NODE_ID_INVALID: &str = "graph.node.id_invalid";
    pub const NODE_DUPLICATE: &str = "graph.node.duplicate";
    pub const NODE_UNKNOWN_TYPE: &str = "graph.node.unknown_type";
    pub const NODE_PORTS_MISMATCH: &str = "graph.node.ports_mismatch";
    pub const NODE_POSITION_INVALID: &str = "graph.node.position_invalid";
    pub const NODE_UNREACHABLE: &str = "graph.node.unreachable";
    pub const PORT_DUPLICATE: &str = "graph.port.duplicate";
    pub const PORT_UNCONNECTED: &str = "graph.port.unconnected";
    pub const PARAM_UNKNOWN: &str = "graph.param.unknown";
    pub const PARAM_TYPE: &str = "graph.param.type";
    pub const PARAM_RANGE: &str = "graph.param.range";
    pub const PARAM_MISSING: &str = "graph.param.missing";
    pub const PARAM_VARIABLE: &str = "graph.param.variable";
    pub const EDGE_UNKNOWN_NODE: &str = "graph.edge.unknown_node";
    pub const EDGE_UNKNOWN_PORT: &str = "graph.edge.unknown_port";
    pub const EDGE_DIRECTION: &str = "graph.edge.direction";
    pub const EDGE_TYPE_MISMATCH: &str = "graph.edge.type_mismatch";
    pub const EDGE_DUPLICATE: &str = "graph.edge.duplicate";
    pub const EDGE_MULTIPLE: &str = "graph.edge.multiple";
    pub const CYCLE: &str = "graph.cycle";
    pub const ROOT_MISSING: &str = "graph.root.missing";
    pub const ROOT_MULTIPLE: &str = "graph.root.multiple";
    pub const VARIABLE_NAME_INVALID: &str = "graph.variable.name_invalid";
    pub const VARIABLE_DUPLICATE: &str = "graph.variable.duplicate";
    pub const VARIABLE_TYPE: &str = "graph.variable.type";
    pub const VARIABLE_DEFAULT: &str = "graph.variable.default";
    pub const METADATA_KEY_INVALID: &str = "graph.metadata.key_invalid";
    pub const EDIT_NODE_MISSING: &str = "graph.edit.node_missing";
    pub const EDIT_EDGE_MISSING: &str = "graph.edit.edge_missing";
    pub const EDIT_VARIABLE_MISSING: &str = "graph.edit.variable_missing";
    pub const EDIT_INDEX: &str = "graph.edit.index";

    /// Every code above, for coverage checks and documentation.
    pub const ALL: &[&str] = &[
        DOCUMENT_MALFORMED,
        DOCUMENT_TOO_LARGE,
        DIAGNOSTICS_TRUNCATED,
        VERSION_UNSUPPORTED,
        KIND_MISMATCH,
        LIMIT_NODES,
        LIMIT_EDGES,
        LIMIT_VARIABLES,
        LIMIT_PORTS,
        LIMIT_PARAMS,
        LIMIT_VALUE,
        LIMIT_METADATA,
        LIMIT_EDITS,
        NODE_ID_INVALID,
        NODE_DUPLICATE,
        NODE_UNKNOWN_TYPE,
        NODE_PORTS_MISMATCH,
        NODE_POSITION_INVALID,
        NODE_UNREACHABLE,
        PORT_DUPLICATE,
        PORT_UNCONNECTED,
        PARAM_UNKNOWN,
        PARAM_TYPE,
        PARAM_RANGE,
        PARAM_MISSING,
        PARAM_VARIABLE,
        EDGE_UNKNOWN_NODE,
        EDGE_UNKNOWN_PORT,
        EDGE_DIRECTION,
        EDGE_TYPE_MISMATCH,
        EDGE_DUPLICATE,
        EDGE_MULTIPLE,
        CYCLE,
        ROOT_MISSING,
        ROOT_MULTIPLE,
        VARIABLE_NAME_INVALID,
        VARIABLE_DUPLICATE,
        VARIABLE_TYPE,
        VARIABLE_DEFAULT,
        METADATA_KEY_INVALID,
        EDIT_NODE_MISSING,
        EDIT_EDGE_MISSING,
        EDIT_VARIABLE_MISSING,
        EDIT_INDEX,
    ];
}

/// Collects diagnostics up to [`MAX_DIAGNOSTICS`], then marks truncation.
#[derive(Default)]
pub(crate) struct Sink {
    items: Vec<GraphDiagnostic>,
    truncated: bool,
}

impl Sink {
    pub fn push(&mut self, code: &str, path: impl Into<String>, message: impl Into<String>) {
        if self.items.len() + 1 < MAX_DIAGNOSTICS {
            self.items.push(GraphDiagnostic::new(code, path, message));
        } else {
            self.truncated = true;
        }
    }
    pub fn finish(mut self) -> Vec<GraphDiagnostic> {
        if self.truncated {
            self.items.push(GraphDiagnostic::new(
                codes::DIAGNOSTICS_TRUNCATED,
                "",
                format!(
                    "more than {} problems; fix these first",
                    MAX_DIAGNOSTICS - 1
                ),
            ));
        }
        self.items
    }
}
