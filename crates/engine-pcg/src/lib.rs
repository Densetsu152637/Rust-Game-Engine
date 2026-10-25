//! Deterministic procedural content generation.
//!
//! * [`noise`]: seeded Perlin, Simplex and Worley noise with fBm/ridged fractals.
//! * [`rng`]: platform-independent random numbers and per-node seed derivation.
//! * [`graph`]: the serde graph model (nodes with typed ports, edges, parameters).
//! * [`Registry`]: node kinds, including the built-in library ([`nodes`]) and game-defined
//!   kinds registered with [`Registry::register`].
//! * [`run`]: the bounded topological executor, run against a [`Surface`].
//!
//! The crate is pure Rust with no engine dependencies; `engine-project` wires it to scenes
//! through the `engine.PcgVolume` component.
pub mod data;
pub mod edit;
mod exec;
pub mod graph;
mod nodes;
pub mod noise;
mod registry;
pub mod rng;
pub mod surface;

pub use data::{Data, Field, FieldContext, Instance, InstanceKind, MathOp, Point};
pub use edit::{EditOp, GraphBuilder, apply as apply_edit};
pub use exec::{NodeReport, RunError, RunResult, run, validate};
pub use graph::{Diagnostic, Edge, GRAPH_VERSION, Graph, MAX_EDGES, MAX_NODES, Node, PortRef};
pub use noise::{Distance, Fractal, Noise, NoiseKind, NoiseParams};
pub use registry::{
    Limits, NodeContext, NodeDefinition, NodeError, NodeFn, Outputs, ParamSpec, ParamType, Params,
    PortSpec, PortType, Registry, RunSettings,
};
pub use rng::{Rng, derive_seed, hash_cell, hash_str, mix64};
pub use surface::{Aabb, FlatSurface, FnSurface, Surface};
