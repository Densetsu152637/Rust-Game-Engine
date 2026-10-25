#![allow(dead_code)]
//! Shared scene registry and a perfect-delivery pump for the integration tests.
use engine_net_replication::*;
use engine_network::PeerId;
use std::sync::Arc;

pub struct Ids {
    pub transform: ComponentId,
    pub pos: PropertyId,
    pub rot: PropertyId,
    pub health: ComponentId,
    pub hp: PropertyId,
    pub inventory: ComponentId,
    pub ammo: PropertyId,
    pub note: PropertyId,
    pub name: ComponentId,
    pub name_v: PropertyId,
    pub observer: ComponentId,
    pub level: PropertyId,
    pub slow: ComponentId,
    pub temp: PropertyId,
    pub fire: RpcId,
    pub chat: RpcId,
    pub hit: RpcId,
    pub announce: RpcId,
    pub poke: RpcId,
}

pub fn registry() -> (Arc<ReplicationRegistry>, Ids) {
    let reg = ReplicationRegistry::builder()
        .component(
            ComponentDef::new("Transform")
                .delivery(StateDelivery::Unreliable)
                .priority(1.0)
                .property(
                    "position",
                    ValueSpec::vec3().quantized(Quantization::Range {
                        min: -1024.0,
                        max: 1024.0,
                        bits: 20,
                    }),
                )
                .property(
                    "rotation",
                    ValueSpec::quat().quantized(Quantization::SmallestThree { bits: 10 }),
                ),
        )
        .component(
            ComponentDef::new("Health")
                .delivery(StateDelivery::Reliable)
                .priority(4.0)
                .property("hp", ValueSpec::u16()),
        )
        .component(
            ComponentDef::new("Inventory")
                .delivery(StateDelivery::Reliable)
                .condition(Condition::OwnerOnly)
                .property("ammo", ValueSpec::u16())
                .property("note", ValueSpec::string(32)),
        )
        .component(
            ComponentDef::new("Name")
                .delivery(StateDelivery::Reliable)
                .condition(Condition::InitialOnly)
                .property("name", ValueSpec::string(32)),
        )
        .component(
            ComponentDef::new("Observer")
                .condition(Condition::SkipOwner)
                .property("level", ValueSpec::u8()),
        )
        .component(ComponentDef::new("Slow").max_update_hz(10.0).property(
            "temp",
            ValueSpec::f32().quantized(Quantization::Step { step: 0.5 }),
        ))
        .rpc(
            RpcDef::new("Fire", RpcDirection::ClientToServer)
                .requires_ownership()
                .max_calls_per_sec(5)
                .arg("weapon", ValueSpec::u8())
                .arg(
                    "dir",
                    ValueSpec::vec3().quantized(Quantization::Step { step: 0.001 }),
                ),
        )
        .rpc(
            RpcDef::new("Chat", RpcDirection::ClientToServer)
                .max_calls_per_sec(3)
                .arg("text", ValueSpec::string(64)),
        )
        .rpc(
            RpcDef::new("Hit", RpcDirection::ServerToClient)
                .targets_entity()
                .arg("damage", ValueSpec::u16()),
        )
        .rpc(
            RpcDef::new("Announce", RpcDirection::ServerToClient)
                .delivery(RpcDelivery::Unreliable)
                .arg("text", ValueSpec::string(64)),
        )
        .rpc(
            RpcDef::new("Poke", RpcDirection::ClientToServer)
                .targets_entity()
                .max_calls_per_sec(100),
        )
        .build()
        .expect("registry");
    let transform = reg.component_id("Transform").unwrap();
    let health = reg.component_id("Health").unwrap();
    let inventory = reg.component_id("Inventory").unwrap();
    let name = reg.component_id("Name").unwrap();
    let observer = reg.component_id("Observer").unwrap();
    let slow = reg.component_id("Slow").unwrap();
    let ids = Ids {
        pos: reg.property_id(transform, "position").unwrap(),
        rot: reg.property_id(transform, "rotation").unwrap(),
        hp: reg.property_id(health, "hp").unwrap(),
        ammo: reg.property_id(inventory, "ammo").unwrap(),
        note: reg.property_id(inventory, "note").unwrap(),
        name_v: reg.property_id(name, "name").unwrap(),
        level: reg.property_id(observer, "level").unwrap(),
        temp: reg.property_id(slow, "temp").unwrap(),
        fire: reg.rpc_id("Fire").unwrap(),
        chat: reg.rpc_id("Chat").unwrap(),
        hit: reg.rpc_id("Hit").unwrap(),
        announce: reg.rpc_id("Announce").unwrap(),
        poke: reg.rpc_id("Poke").unwrap(),
        transform,
        health,
        inventory,
        name,
        observer,
        slow,
    };
    (Arc::new(reg), ids)
}

pub fn quat_z(angle: f32) -> [f32; 4] {
    [0.0, 0.0, (angle / 2.0).sin(), (angle / 2.0).cos()]
}

/// Transform + Health + Name entity.
pub fn basic(ids: &Ids, pos: [f32; 3], hp: u16, name: &str) -> EntityInit {
    EntityInit::new()
        .at(pos)
        .component(
            ids.transform,
            vec![Value::Vec3(pos), Value::Quat(quat_z(0.3))],
        )
        .component(ids.health, vec![Value::U16(hp)])
        .component(ids.name, vec![Value::String(name.to_owned())])
}

pub fn with_inventory(ids: &Ids, init: EntityInit, ammo: u16, note: &str) -> EntityInit {
    init.component(
        ids.inventory,
        vec![Value::U16(ammo), Value::String(note.to_owned())],
    )
}

pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

pub struct Pump {
    pub server: ReplicationServer,
    pub clients: Vec<(PeerId, ReplicationClient)>,
    pub tick: u32,
    /// Every state packet ever built, per peer.
    pub log: Vec<(PeerId, OutPacket)>,
}

impl Pump {
    pub fn new(reg: &Arc<ReplicationRegistry>, peers: &[PeerId]) -> Self {
        let limits = ReplicationLimits::default();
        let mut server =
            ReplicationServer::new(reg.clone(), limits.clone(), ServerConfig::default()).unwrap();
        let mut clients = Vec::new();
        for &p in peers {
            server.add_peer(p).unwrap();
            clients.push((
                p,
                ReplicationClient::new(reg.clone(), limits.clone()).unwrap(),
            ));
        }
        Self {
            server,
            clients,
            tick: 0,
            log: Vec::new(),
        }
    }

    pub fn client(&mut self, peer: PeerId) -> &mut ReplicationClient {
        &mut self.clients.iter_mut().find(|c| c.0 == peer).unwrap().1
    }

    /// One tick with lossless instant delivery. Returns the packets built per peer.
    pub fn step(&mut self, budget: usize, mtu: usize) -> Vec<(PeerId, Vec<OutPacket>)> {
        self.tick += 1;
        let mut result = Vec::new();
        for (peer, client) in &mut self.clients {
            let packets = self.server.build(*peer, self.tick, budget, mtu).unwrap();
            for p in &packets {
                client.receive(&p.bytes).expect("valid packet");
                self.log.push((*peer, p.clone()));
            }
            if let Some(ack) = client.build_ack() {
                self.server.receive(*peer, &ack.bytes).expect("valid ack");
            }
            result.push((*peer, packets));
        }
        result
    }

    pub fn run(&mut self, ticks: u32) {
        for _ in 0..ticks {
            self.step(100_000, 1100);
        }
    }
}

/// Compares a client mirror with the authoritative server state for the entities the peer may
/// see. Returns human-readable differences (empty when the mirror matches).
pub fn mirror_mismatches(
    reg: &ReplicationRegistry,
    server: &ReplicationServer,
    client: &ReplicationClient,
    peer: PeerId,
    expected: &[(NetId, Vec<ComponentId>)],
) -> Vec<String> {
    let mut out = Vec::new();
    if client.entity_count() != expected.len() {
        out.push(format!(
            "entity count {} != {}",
            client.entity_count(),
            expected.len()
        ));
    }
    for (id, comps) in expected {
        let Some(ce) = client.entity(*id) else {
            out.push(format!("{id} missing on client"));
            continue;
        };
        let owned = server.owner(*id) == Some(peer);
        if ce.owned != owned {
            out.push(format!("{id} owned {} != {owned}", ce.owned));
        }
        for comp in comps {
            let def = reg.component(*comp).unwrap();
            let visible = match def.condition {
                Condition::OwnerOnly => owned,
                Condition::SkipOwner => !owned,
                _ => true,
            };
            if !visible {
                if ce.component(*comp).is_some() {
                    out.push(format!("{id} leaked component {}", def.name));
                }
                continue;
            }
            for (p, _) in def.properties.iter().enumerate() {
                let pid = PropertyId(p as u8);
                let want = server.get_property(*id, *comp, pid).unwrap();
                match ce.get(*comp, pid) {
                    Some(have) if *have == want => {}
                    other => out.push(format!("{id} {}.{p}: {other:?} != {want:?}", def.name)),
                }
            }
        }
    }
    out
}
