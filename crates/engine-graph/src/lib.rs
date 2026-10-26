//! Common node-graph data model shared by every graph domain.
//!
//! One serde JSON document shape ([`GraphDocument`]) carries behavior trees, state
//! trees, visual scripts and the animation, audio and material graphs of other
//! domains, so a single editor widget and a single set of tools can handle them.
//!
//! * [`model`]: documents, nodes, ports, edges, variables and a lookup index.
//! * [`kind`]: [`GraphKind`] plus the [`NodeTypeRegistry`] a domain fills with its
//!   node types (ports, parameter schemas with defaults and bounds, palette data).
//! * [`validate`](mod@validate): bounded, deterministic validation with stable
//!   diagnostic [`codes`].
//! * [`edit`]: pure atomic edit operations that validate before applying.
//!
//! The crate has no engine, ECS or game dependency. A domain compiles a document
//! that passed full validation into its own executable form. See
//! `docs/ai-graphs.md`.
pub mod diagnostic;
pub mod edit;
pub mod kind;
pub mod model;
pub mod validate;

pub use diagnostic::{GraphDiagnostic, MAX_DIAGNOSTICS, codes};
pub use edit::{EditFailure, GraphEdit, MAX_EDITS, apply_edit, apply_edits};
pub use kind::{
    GraphKind, GraphKindInfo, GraphLimits, KindCheck, NodeTypeDef, NodeTypeRegistry, ParamDef,
    ParamType, PortDef,
};
pub use model::{
    GRAPH_VERSION, GraphDocument, GraphEdge, GraphIndex, GraphNode, GraphVariable,
    MAX_DOCUMENT_BYTES, MAX_EDGES, MAX_METADATA, MAX_NAME_LEN, MAX_NODES, MAX_PARAMS_PER_NODE,
    MAX_PORTS_PER_NODE, MAX_VARIABLES, NodeId, Port, PortDirection, PortRef, PortType,
};
pub use validate::{Strictness, valid_name, validate, value_conforms, value_within_limits};
