//! Packet framing shared by server and client.
//!
//! ```text
//! common   : u8 version | u8 kind | u32 schema fingerprint
//! state    : common | u32 sequence | u32 tick | varint blocks | block*
//! ack      : common | u32 largest | u64 bits (bit i = largest-1-i received)
//! rpc      : common | varint calls | (varint rpc | u8 flags | [NetId] | varint len | args)*
//! block    : u8 op | NetId | body
//!   spawn  : u8 flags(bit0 owned) | varint components | (varint component | all property values)*
//!   update : varint components | (varint component | varint property mask | present values)*
//!   despawn: -
//!   owner  : u8 owned
//! ```
use crate::error::{ReplicationError, Result};
use crate::ids::NetId;
use crate::limits::ReplicationLimits;
use crate::registry::{PROTOCOL_VERSION, RpcDef};
use crate::value::Value;
use crate::wire::{Reader, put_varint};
use engine_network::{Delivery, Lane};

pub(crate) const KIND_STATE: u8 = 1;
pub(crate) const KIND_ACK: u8 = 2;
pub(crate) const KIND_RPC: u8 = 3;
pub(crate) const OP_SPAWN: u8 = 1;
pub(crate) const OP_UPDATE: u8 = 2;
pub(crate) const OP_DESPAWN: u8 = 3;
pub(crate) const OP_OWNER: u8 = 4;
/// Version, kind, fingerprint, sequence and tick (the block-count varint follows).
pub(crate) const STATE_FIXED_HEADER: usize = 14;
/// Smallest `max_payload` accepted by `ReplicationServer::build`.
pub const MIN_PAYLOAD: usize = 64;

/// One packet ready for the transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutPacket {
    pub lane: Lane,
    pub delivery: Delivery,
    pub bytes: Vec<u8>,
    /// Replication sequence number of a server state packet. Pass it to
    /// `ReplicationServer::mark_unsent` when the transport refused the payload.
    pub sequence: Option<u32>,
}

pub(crate) fn put_common(out: &mut Vec<u8>, kind: u8, fingerprint: u32) {
    out.push(PROTOCOL_VERSION);
    out.push(kind);
    out.extend_from_slice(&fingerprint.to_le_bytes());
}

/// Parses version, kind and fingerprint; rejects unknown versions and schema mismatch.
pub(crate) fn read_common(r: &mut Reader<'_>, fingerprint: u32) -> Result<u8> {
    if r.u8()? != PROTOCOL_VERSION {
        return Err(ReplicationError::Malformed("unsupported protocol version"));
    }
    let kind = r.u8()?;
    if r.u32()? != fingerprint {
        return Err(ReplicationError::SchemaMismatch);
    }
    if !matches!(kind, KIND_STATE | KIND_ACK | KIND_RPC) {
        return Err(ReplicationError::Malformed("unknown packet kind"));
    }
    Ok(kind)
}

pub(crate) fn encode_ack(fingerprint: u32, largest: u32, bits: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(18);
    put_common(&mut out, KIND_ACK, fingerprint);
    out.extend_from_slice(&largest.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out
}

pub(crate) fn encode_args(def: &RpcDef, args: &[Value]) -> Result<Vec<u8>> {
    if args.len() != def.args.len() {
        return Err(ReplicationError::Invalid("rpc argument count mismatch"));
    }
    let mut out = Vec::new();
    for (arg, value) in def.args.iter().zip(args) {
        arg.spec.encode(value, &mut out)?;
    }
    Ok(out)
}

pub(crate) fn decode_args(def: &RpcDef, bytes: &[u8]) -> Result<Vec<Value>> {
    let mut r = Reader::new(bytes);
    let mut out = Vec::with_capacity(def.args.len());
    for arg in &def.args {
        out.push(arg.spec.decode(&mut r)?);
    }
    r.end()?;
    Ok(out)
}

/// Encodes one RPC packet containing a single call.
pub(crate) fn encode_rpc_packet(
    fingerprint: u32,
    rpc: u16,
    entity: Option<NetId>,
    args: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + args.len());
    put_common(&mut out, KIND_RPC, fingerprint);
    put_varint(&mut out, 1);
    put_varint(&mut out, u64::from(rpc));
    out.push(u8::from(entity.is_some()));
    if let Some(id) = entity {
        id.put(&mut out);
    }
    put_varint(&mut out, args.len() as u64);
    out.extend_from_slice(args);
    out
}

/// One call as framed on the wire; its arguments are still opaque bytes.
pub(crate) struct RawCall<'a> {
    pub rpc: u32,
    pub entity: Option<NetId>,
    pub args: &'a [u8],
}

/// Parses the call frames of an RPC packet body (after the common header). Framing errors are
/// hard failures; per-call validation is left to the caller so bad calls can be skipped.
pub(crate) fn read_calls<'a>(
    r: &mut Reader<'a>,
    limits: &ReplicationLimits,
) -> Result<Vec<RawCall<'a>>> {
    let n = r.varint()?;
    if n > limits.max_rpcs_per_packet as u64 {
        return Err(ReplicationError::Limit("too many rpc calls in one packet"));
    }
    let mut calls = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let rpc = r.varint_u32()?;
        let flags = r.u8()?;
        if flags > 1 {
            return Err(ReplicationError::Malformed("unknown rpc flags"));
        }
        let entity = if flags & 1 == 1 {
            Some(NetId::read(r, limits.max_entities)?)
        } else {
            None
        };
        let len = r.varint()?;
        if len > limits.max_rpc_payload_bytes as u64 {
            return Err(ReplicationError::Limit("rpc payload above limit"));
        }
        let args = r.take(len as usize)?;
        calls.push(RawCall { rpc, entity, args });
    }
    r.end()?;
    Ok(calls)
}

pub(crate) fn rpc_delivery(def: &RpcDef) -> Delivery {
    match def.delivery {
        crate::registry::RpcDelivery::Reliable => Delivery::ReliableOrdered,
        crate::registry::RpcDelivery::Unreliable => Delivery::Unreliable,
    }
}

pub(crate) fn rpc_packet(def: &RpcDef, bytes: Vec<u8>) -> OutPacket {
    OutPacket {
        lane: Lane::RPC,
        delivery: rpc_delivery(def),
        bytes,
        sequence: None,
    }
}
