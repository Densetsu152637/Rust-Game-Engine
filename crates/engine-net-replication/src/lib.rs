//! Scalable, sans-IO state replication for engine sessions.
//!
//! The crate turns authoritative entity state into bounded byte packets per peer and applies
//! those packets to a client mirror. It never owns sockets: packets are addressed by
//! `(Lane, Delivery)` exactly as the `engine_network::MessageEndpoint` contract expects, and
//! lanes and deliveries are assumed to be independent, lossy, duplicating and reordering.
//!
//! * [`ReplicationRegistry`] describes components, typed/quantized properties, delivery class,
//!   priority, visibility condition and update-rate caps, plus RPCs; both ends build it
//!   identically and compare [`ReplicationRegistry::schema_hash`].
//! * [`ReplicationServer`] holds authoritative state, tracks per-peer acknowledged baselines,
//!   sends only what differs, applies interest management and fills packets by priority within a
//!   byte budget.
//! * [`ReplicationClient`] validates and applies packets to a mirror, emits [`ClientEvent`]s and
//!   property callbacks and produces acknowledgements.
//!
//! See `docs/networking-replication.md` for the algorithm, wire format and threat model.
mod client;
mod error;
mod grid;
mod ids;
mod limits;
mod packet;
mod registry;
mod server;
pub mod telemetry;
pub mod testing;
mod value;
mod wire;

pub use client::{ClientComponent, ClientEntity, ClientEvent, PropertyChange, ReplicationClient};
pub use error::{ReplicationError, Result};
pub use ids::NetId;
pub use limits::{BandwidthBudget, ReplicationLimits, ServerConfig};
pub use packet::{MIN_PAYLOAD, OutPacket};
pub use registry::{
    ComponentDef, ComponentId, Condition, PROTOCOL_VERSION, PropertyDef, PropertyId,
    RegistryBuilder, ReplicationRegistry, RpcArg, RpcDef, RpcDelivery, RpcDirection, RpcId,
    StateDelivery,
};
pub use server::{
    EntityInit, EntityRelevance, EntityView, Relevance, RelevanceFn, ReplicationServer, RpcTarget,
    ServerEvent,
};
pub use telemetry::{ClientTelemetry, PeerTelemetry, RpcRejectReason, ServerTelemetry};
pub use value::{Quantization, Value, ValueKind, ValueSpec};
