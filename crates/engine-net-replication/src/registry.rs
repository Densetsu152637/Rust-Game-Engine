//! Replicated component and RPC schema shared by server and client.
//!
//! Both ends must build the registry from identical definitions in identical order; the
//! [`ReplicationRegistry::schema_hash`] covers every rule that influences the wire format and
//! packets carry its 32-bit [`fingerprint`](ReplicationRegistry::fingerprint).
use crate::error::{ReplicationError, Result};
use crate::value::ValueSpec;
use crate::wire::put_varint;
use serde::Serialize;
use std::collections::BTreeMap;

/// Wire-format revision folded into the schema hash and the first byte of every packet.
pub const PROTOCOL_VERSION: u8 = 1;
pub const MAX_COMPONENTS: usize = 1024;
pub const MAX_PROPS_PER_COMPONENT: usize = 32;
pub const MAX_RPCS: usize = 1024;
pub const MAX_RPC_ARGS: usize = 16;
pub const MAX_NAME_LEN: usize = 64;

macro_rules! id_type {
    ($(#[$m:meta])* $name:ident($inner:ty)) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
        pub struct $name(pub $inner);
    };
}
id_type!(
    /// Index of a component in registration order.
    ComponentId(u16)
);
id_type!(
    /// Index of a property within its component.
    PropertyId(u8)
);
id_type!(
    /// Index of an RPC in registration order.
    RpcId(u16)
);

/// How a component's property changes are delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum StateDelivery {
    /// Sent in `Delivery::ReliableOrdered` packets; the transport retransmits, and the acked
    /// baseline still drives deltas.
    Reliable,
    /// Sent in `Delivery::Unreliable` packets; losses are repaired by resending against the
    /// acknowledged baseline.
    Unreliable,
}

/// Which peers may receive a component, and when.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Condition {
    All,
    /// Only the entity owner receives it; it is never encoded for anyone else.
    OwnerOnly,
    /// Everyone except the owner.
    SkipOwner,
    /// Sent with the spawn state only (to all relevant peers), never as an update.
    InitialOnly,
}

impl Condition {
    pub(crate) fn visible_to(self, owned: bool) -> bool {
        match self {
            Self::All | Self::InitialOnly => true,
            Self::OwnerOnly => owned,
            Self::SkipOwner => !owned,
        }
    }
    pub(crate) fn owner_dependent(self) -> bool {
        matches!(self, Self::OwnerOnly | Self::SkipOwner)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PropertyDef {
    pub name: String,
    pub spec: ValueSpec,
}

/// Replication rules of one component.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ComponentDef {
    pub name: String,
    pub delivery: StateDelivery,
    /// Priority weight (> 0) added to a peer's per-entity accumulator each tick while changes
    /// are pending; larger values win bandwidth sooner.
    pub priority: f32,
    pub condition: Condition,
    /// Per-peer, per-property cap on update frequency; `None` means every tick.
    pub max_update_hz: Option<f32>,
    pub properties: Vec<PropertyDef>,
}

impl ComponentDef {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            delivery: StateDelivery::Unreliable,
            priority: 1.0,
            condition: Condition::All,
            max_update_hz: None,
            properties: Vec::new(),
        }
    }
    pub fn delivery(mut self, delivery: StateDelivery) -> Self {
        self.delivery = delivery;
        self
    }
    pub fn priority(mut self, priority: f32) -> Self {
        self.priority = priority;
        self
    }
    pub fn condition(mut self, condition: Condition) -> Self {
        self.condition = condition;
        self
    }
    pub fn max_update_hz(mut self, hz: f32) -> Self {
        self.max_update_hz = Some(hz);
        self
    }
    pub fn property(mut self, name: impl Into<String>, spec: ValueSpec) -> Self {
        self.properties.push(PropertyDef {
            name: name.into(),
            spec,
        });
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum RpcDirection {
    ServerToClient,
    ClientToServer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum RpcDelivery {
    Reliable,
    Unreliable,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RpcArg {
    pub name: String,
    pub spec: ValueSpec,
}

/// Rules of one remote procedure.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RpcDef {
    pub name: String,
    pub direction: RpcDirection,
    pub delivery: RpcDelivery,
    /// The call names an entity; the server verifies the caller can see it.
    pub targets_entity: bool,
    /// Client calls require the caller to own the target entity (implies `targets_entity`).
    pub requires_ownership: bool,
    pub args: Vec<RpcArg>,
    /// Sustained calls per second per peer (client-to-server calls only; burst equals this).
    pub max_calls_per_sec: u32,
}

impl RpcDef {
    pub fn new(name: impl Into<String>, direction: RpcDirection) -> Self {
        Self {
            name: name.into(),
            direction,
            delivery: RpcDelivery::Reliable,
            targets_entity: false,
            requires_ownership: false,
            args: Vec::new(),
            max_calls_per_sec: 10,
        }
    }
    pub fn delivery(mut self, delivery: RpcDelivery) -> Self {
        self.delivery = delivery;
        self
    }
    pub fn targets_entity(mut self) -> Self {
        self.targets_entity = true;
        self
    }
    pub fn requires_ownership(mut self) -> Self {
        self.targets_entity = true;
        self.requires_ownership = true;
        self
    }
    pub fn max_calls_per_sec(mut self, calls: u32) -> Self {
        self.max_calls_per_sec = calls;
        self
    }
    pub fn arg(mut self, name: impl Into<String>, spec: ValueSpec) -> Self {
        self.args.push(RpcArg {
            name: name.into(),
            spec,
        });
        self
    }
}

/// Immutable, validated component/RPC schema.
#[derive(Clone, Debug, Serialize)]
pub struct ReplicationRegistry {
    components: Vec<ComponentDef>,
    rpcs: Vec<RpcDef>,
    #[serde(skip)]
    component_names: BTreeMap<String, ComponentId>,
    #[serde(skip)]
    rpc_names: BTreeMap<String, RpcId>,
    schema_hash: u64,
}

/// Collects definitions and validates them in [`RegistryBuilder::build`].
#[derive(Default)]
pub struct RegistryBuilder {
    components: Vec<ComponentDef>,
    rpcs: Vec<RpcDef>,
}

impl RegistryBuilder {
    pub fn component(mut self, def: ComponentDef) -> Self {
        self.components.push(def);
        self
    }
    pub fn rpc(mut self, def: RpcDef) -> Self {
        self.rpcs.push(def);
        self
    }

    pub fn build(self) -> Result<ReplicationRegistry> {
        let reg = |m: String| Err(ReplicationError::Registry(m));
        if self.components.len() > MAX_COMPONENTS || self.rpcs.len() > MAX_RPCS {
            return reg("too many components or rpcs".into());
        }
        let mut component_names = BTreeMap::new();
        for (i, c) in self.components.iter().enumerate() {
            valid_name(&c.name)?;
            if component_names
                .insert(c.name.clone(), ComponentId(i as u16))
                .is_some()
            {
                return reg(format!("duplicate component `{}`", c.name));
            }
            if c.properties.is_empty() || c.properties.len() > MAX_PROPS_PER_COMPONENT {
                return reg(format!("component `{}` needs 1..=32 properties", c.name));
            }
            if !c.priority.is_finite() || c.priority <= 0.0 || c.priority > 1e6 {
                return reg(format!(
                    "component `{}` priority must be in (0, 1e6]",
                    c.name
                ));
            }
            if c.max_update_hz
                .is_some_and(|hz| !hz.is_finite() || hz <= 0.0 || hz > 1e6)
            {
                return reg(format!("component `{}` max_update_hz must be > 0", c.name));
            }
            let mut seen = BTreeMap::new();
            for p in &c.properties {
                valid_name(&p.name)?;
                if seen.insert(p.name.as_str(), ()).is_some() {
                    return reg(format!("duplicate property `{}.{}`", c.name, p.name));
                }
                p.spec.validate().map_err(|e| {
                    ReplicationError::Registry(format!("property `{}.{}`: {e}", c.name, p.name))
                })?;
            }
        }
        let mut rpc_names = BTreeMap::new();
        for (i, r) in self.rpcs.iter().enumerate() {
            valid_name(&r.name)?;
            if rpc_names.insert(r.name.clone(), RpcId(i as u16)).is_some() {
                return reg(format!("duplicate rpc `{}`", r.name));
            }
            if r.args.len() > MAX_RPC_ARGS {
                return reg(format!("rpc `{}` has too many arguments", r.name));
            }
            if r.requires_ownership
                && (!r.targets_entity || r.direction != RpcDirection::ClientToServer)
            {
                return reg(format!(
                    "rpc `{}`: ownership applies to client-to-server entity calls",
                    r.name
                ));
            }
            if r.direction == RpcDirection::ClientToServer && r.max_calls_per_sec == 0 {
                return reg(format!("rpc `{}` needs max_calls_per_sec >= 1", r.name));
            }
            for a in &r.args {
                valid_name(&a.name)?;
                a.spec.validate().map_err(|e| {
                    ReplicationError::Registry(format!("rpc `{}.{}`: {e}", r.name, a.name))
                })?;
            }
        }
        let schema_hash = fnv1a(&schema_bytes(&self.components, &self.rpcs));
        Ok(ReplicationRegistry {
            components: self.components,
            rpcs: self.rpcs,
            component_names,
            rpc_names,
            schema_hash,
        })
    }
}

fn valid_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'/' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(ReplicationError::Registry(format!(
            "name `{name}` must be 1..=64 of [A-Za-z0-9_.:/-]"
        )))
    }
}

fn put_name(out: &mut Vec<u8>, name: &str) {
    put_varint(out, name.len() as u64);
    out.extend_from_slice(name.as_bytes());
}

fn schema_bytes(components: &[ComponentDef], rpcs: &[RpcDef]) -> Vec<u8> {
    let mut out = vec![PROTOCOL_VERSION];
    put_varint(&mut out, components.len() as u64);
    for c in components {
        put_name(&mut out, &c.name);
        out.push(c.delivery as u8);
        out.push(c.condition as u8);
        out.extend_from_slice(&c.priority.to_bits().to_le_bytes());
        out.extend_from_slice(&c.max_update_hz.map_or(0, f32::to_bits).to_le_bytes());
        out.push(u8::from(c.max_update_hz.is_some()));
        put_varint(&mut out, c.properties.len() as u64);
        for p in &c.properties {
            put_name(&mut out, &p.name);
            p.spec.write_canonical(&mut out);
        }
    }
    put_varint(&mut out, rpcs.len() as u64);
    for r in rpcs {
        put_name(&mut out, &r.name);
        out.push(r.direction as u8);
        out.push(r.delivery as u8);
        out.push(u8::from(r.targets_entity));
        out.push(u8::from(r.requires_ownership));
        out.extend_from_slice(&r.max_calls_per_sec.to_le_bytes());
        put_varint(&mut out, r.args.len() as u64);
        for a in &r.args {
            put_name(&mut out, &a.name);
            a.spec.write_canonical(&mut out);
        }
    }
    out
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

impl ReplicationRegistry {
    pub fn builder() -> RegistryBuilder {
        RegistryBuilder::default()
    }
    pub fn components(&self) -> &[ComponentDef] {
        &self.components
    }
    pub fn rpcs(&self) -> &[RpcDef] {
        &self.rpcs
    }
    pub fn component(&self, id: ComponentId) -> Option<&ComponentDef> {
        self.components.get(usize::from(id.0))
    }
    pub fn rpc(&self, id: RpcId) -> Option<&RpcDef> {
        self.rpcs.get(usize::from(id.0))
    }
    pub fn component_id(&self, name: &str) -> Option<ComponentId> {
        self.component_names.get(name).copied()
    }
    pub fn property_id(&self, component: ComponentId, name: &str) -> Option<PropertyId> {
        self.component(component)?
            .properties
            .iter()
            .position(|p| p.name == name)
            .map(|i| PropertyId(i as u8))
    }
    pub fn rpc_id(&self, name: &str) -> Option<RpcId> {
        self.rpc_names.get(name).copied()
    }
    /// 64-bit FNV-1a hash of every wire-relevant rule. Compare it in the session handshake.
    pub fn schema_hash(&self) -> u64 {
        self.schema_hash
    }
    /// 32-bit fold of [`Self::schema_hash`] carried in every packet header.
    pub fn fingerprint(&self) -> u32 {
        (self.schema_hash ^ (self.schema_hash >> 32)) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{Quantization, ValueSpec};

    fn sample() -> RegistryBuilder {
        ReplicationRegistry::builder()
            .component(
                ComponentDef::new("Transform")
                    .property(
                        "position",
                        ValueSpec::vec3().quantized(Quantization::Step { step: 0.01 }),
                    )
                    .property("rotation", ValueSpec::quat()),
            )
            .rpc(RpcDef::new("Fire", RpcDirection::ClientToServer).requires_ownership())
    }

    #[test]
    fn lookup_by_name_and_stable_hash() {
        let a = sample().build().unwrap();
        let b = sample().build().unwrap();
        assert_eq!(a.schema_hash(), b.schema_hash());
        let t = a.component_id("Transform").unwrap();
        assert_eq!(a.property_id(t, "rotation"), Some(PropertyId(1)));
        assert_eq!(a.rpc_id("Fire"), Some(RpcId(0)));
        assert_eq!(a.rpc_id("Nope"), None);
    }

    #[test]
    fn any_rule_change_changes_the_hash() {
        let base = sample().build().unwrap().schema_hash();
        let changed = ReplicationRegistry::builder()
            .component(
                ComponentDef::new("Transform")
                    .priority(2.0)
                    .property(
                        "position",
                        ValueSpec::vec3().quantized(Quantization::Step { step: 0.01 }),
                    )
                    .property("rotation", ValueSpec::quat()),
            )
            .rpc(RpcDef::new("Fire", RpcDirection::ClientToServer).requires_ownership())
            .build()
            .unwrap()
            .schema_hash();
        assert_ne!(base, changed);
    }

    #[test]
    fn invalid_definitions_are_rejected() {
        let dup = ReplicationRegistry::builder()
            .component(ComponentDef::new("A").property("x", ValueSpec::u8()))
            .component(ComponentDef::new("A").property("x", ValueSpec::u8()))
            .build();
        assert!(dup.is_err());
        assert!(
            ReplicationRegistry::builder()
                .component(ComponentDef::new("Empty"))
                .build()
                .is_err()
        );
        assert!(
            ReplicationRegistry::builder()
                .component(ComponentDef::new("bad name").property("x", ValueSpec::u8()))
                .build()
                .is_err()
        );
        assert!(
            ReplicationRegistry::builder()
                .component(
                    ComponentDef::new("P")
                        .priority(0.0)
                        .property("x", ValueSpec::u8())
                )
                .build()
                .is_err()
        );
        assert!(
            ReplicationRegistry::builder()
                .rpc(RpcDef::new("R", RpcDirection::ServerToClient).requires_ownership())
                .build()
                .is_err()
        );
        assert!(
            ReplicationRegistry::builder()
                .rpc(RpcDef::new("R", RpcDirection::ClientToServer).max_calls_per_sec(0))
                .build()
                .is_err()
        );
    }
}
