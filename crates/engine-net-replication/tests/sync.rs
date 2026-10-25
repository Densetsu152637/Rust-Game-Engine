mod common;

use common::*;
use engine_net_replication::testing::{LinkConfig, Rng, Simulation};
use engine_net_replication::*;
use engine_network::{Delivery, Lane};
use std::sync::{Arc, Mutex};

fn deliver_all(client: &mut ReplicationClient, packets: &[OutPacket]) {
    for p in packets {
        client.receive(&p.bytes).unwrap();
    }
}

fn ack(server: &mut ReplicationServer, peer: u64, client: &mut ReplicationClient) {
    if let Some(a) = client.build_ack() {
        server.receive(peer, &a.bytes).unwrap();
    }
}

#[test]
fn converges_under_loss_reorder_and_duplication() {
    let mut worst = 0;
    for seed in 1..=6u64 {
        let (reg, ids) = registry();
        let mut sim = Simulation::new(
            reg.clone(),
            ReplicationLimits::default(),
            ServerConfig::default(),
            &LinkConfig::hostile(),
            seed,
            3,
        )
        .unwrap();
        for c in 1..=3 {
            sim.server.set_peer_view(c, [0.0; 3], 10_000.0).unwrap();
        }
        let mut rng = Rng::new(seed * 31);
        let mut scene: Vec<(NetId, Vec<ComponentId>)> = Vec::new();
        let spawn = |sim: &mut Simulation, rng: &mut Rng, i: usize| {
            let pos = [rng.range(-500.0, 500.0), rng.range(-500.0, 500.0), 0.0];
            let mut init = basic(&ids, pos, 100, &format!("e{i}"));
            let mut comps = vec![ids.transform, ids.health, ids.name];
            if i.is_multiple_of(4) {
                init = init.component(ids.observer, vec![Value::U8(i as u8)]);
                comps.push(ids.observer);
            }
            if i.is_multiple_of(5) {
                init = init.component(ids.slow, vec![Value::F32(1.0)]);
                comps.push(ids.slow);
            }
            if i < 3 {
                init = with_inventory(&ids, init.owner(i as u64 + 1), 30, "bag");
                comps.push(ids.inventory);
            }
            (sim.server.spawn(init).unwrap(), comps)
        };
        for i in 0..60 {
            scene.push(spawn(&mut sim, &mut rng, i));
        }
        for t in 0..200u32 {
            for _ in 0..10 {
                let (id, comps) = scene[rng.below(scene.len() as u64) as usize].clone();
                let pos = [rng.range(-500.0, 500.0), rng.range(-500.0, 500.0), 0.0];
                sim.server
                    .set_property(id, ids.transform, ids.pos, &Value::Vec3(pos))
                    .unwrap();
                sim.server
                    .set_property(
                        id,
                        ids.transform,
                        ids.rot,
                        &Value::Quat(quat_z(rng.range(0.0, 6.0))),
                    )
                    .unwrap();
                sim.server
                    .set_property(id, ids.health, ids.hp, &Value::U16(rng.below(1000) as u16))
                    .unwrap();
                if comps.contains(&ids.slow) {
                    sim.server
                        .set_property(id, ids.slow, ids.temp, &Value::F32(rng.range(0.0, 50.0)))
                        .unwrap();
                }
            }
            if t % 20 == 10 {
                let victim = 3 + rng.below(scene.len() as u64 - 3) as usize;
                sim.server.despawn(scene[victim].0).unwrap();
                scene[victim] = spawn(&mut sim, &mut rng, 1000 + t as usize);
            }
            if t % 50 == 25 {
                // Owner change on an inventory-less entity (flag update) and on an inventory
                // entity (despawn + respawn resync).
                sim.server.set_owner(scene[10].0, Some(2)).unwrap();
                let current = sim.server.owner(scene[0].0).unwrap();
                sim.server
                    .set_owner(scene[0].0, Some(current % 3 + 1))
                    .unwrap();
            }
            let stats = sim.step(3000, 1100);
            assert!(stats.bytes.iter().all(|b| *b <= 3000), "budget exceeded");
        }
        let mut converged_at = None;
        for extra in 0..120u32 {
            let stats = sim.step(3000, 1100);
            assert!(stats.bytes.iter().all(|b| *b <= 3000), "budget exceeded");
            let clean = sim.clients.iter().all(|c| {
                mirror_mismatches(&reg, &sim.server, &c.client, c.peer, &scene).is_empty()
            });
            if clean {
                converged_at = Some(extra);
                break;
            }
        }
        let Some(ticks) = converged_at else {
            for c in &sim.clients {
                eprintln!(
                    "seed {seed} peer {}: {:?}",
                    c.peer,
                    mirror_mismatches(&reg, &sim.server, &c.client, c.peer, &scene)
                );
            }
            panic!("seed {seed} did not converge");
        };
        assert!(sim.clients.iter().all(|c| c.rejected == 0));
        worst = worst.max(ticks);
        let lost: u64 = sim.clients.iter().map(|c| c.down.lost + c.up.lost).sum();
        assert!(lost > 0, "the link must actually drop packets");
        eprintln!("seed {seed}: converged {ticks} ticks after changes stopped, {lost} lost");
    }
    assert!(worst <= 60, "slowest convergence {worst} ticks");
}

#[test]
fn delta_compression_ratio_is_measured_for_a_mostly_static_scene() {
    let (reg, ids) = registry();
    let mut pump = Pump::new(&reg, &[1]);
    pump.server.set_peer_view(1, [0.0; 3], 10_000.0).unwrap();
    let mut rng = Rng::new(7);
    let mut scene = Vec::new();
    for i in 0..200 {
        let pos = [rng.range(-900.0, 900.0), rng.range(-900.0, 900.0), 0.0];
        scene.push(
            pump.server
                .spawn(basic(&ids, pos, 100, &format!("entity-{i}")))
                .unwrap(),
        );
    }
    pump.run(10);
    assert!(scene.iter().all(|id| pump.server.is_live_for(1, *id)));
    let before = pump.server.peer_telemetry(1).unwrap().clone();
    let ticks = 300;
    for _ in 0..ticks {
        for _ in 0..20 {
            // 10% of the entities move each tick.
            let id = scene[rng.below(200) as usize];
            let pos = [rng.range(-900.0, 900.0), rng.range(-900.0, 900.0), 0.0];
            pump.server
                .set_property(id, ids.transform, ids.pos, &Value::Vec3(pos))
                .unwrap();
            pump.server
                .set_property(
                    id,
                    ids.transform,
                    ids.rot,
                    &Value::Quat(quat_z(rng.range(0.0, 6.0))),
                )
                .unwrap();
        }
        pump.step(100_000, 1100);
    }
    let after = pump.server.peer_telemetry(1).unwrap().clone();
    let delta = after.bytes_total - before.bytes_total;
    let full = after.snapshot_equivalent_bytes_total - before.snapshot_equivalent_bytes_total;
    let ratio = full as f64 / delta as f64;
    eprintln!(
        "200 entities, 10% moving: {} B/tick delta vs {} B/tick full snapshot, ratio {ratio:.1}",
        delta / ticks,
        full / ticks
    );
    assert!(ratio > 5.0, "compression ratio {ratio}");
    assert!(after.compression_ratio > 1.0);
    // The estimate is honest: a real full spawn costs about the same per entity.
    let spawn_avg = pump
        .server
        .peer_telemetry(1)
        .unwrap()
        .full_state_bytes_total
        / 200;
    let estimate = full / ticks / 200;
    assert!(
        estimate.abs_diff(spawn_avg) <= 8,
        "{estimate} vs {spawn_avg}"
    );
    assert!(
        mirror_mismatches(
            &reg,
            &pump.server,
            &pump.clients[0].1,
            1,
            &scene
                .iter()
                .map(|id| (*id, vec![ids.transform, ids.health, ids.name]))
                .collect::<Vec<_>>()
        )
        .is_empty()
    );
}

#[test]
fn unchanged_and_sub_step_changes_cost_nothing() {
    let (reg, ids) = registry();
    let mut pump = Pump::new(&reg, &[1]);
    pump.server.set_peer_view(1, [0.0; 3], 100.0).unwrap();
    let init = basic(&ids, [1.0, 2.0, 3.0], 50, "a").component(ids.slow, vec![Value::F32(1.0)]);
    let id = pump.server.spawn(init).unwrap();
    pump.run(8);
    assert!(
        !pump
            .server
            .set_property(id, ids.health, ids.hp, &Value::U16(50))
            .unwrap()
    );
    // Step quantization of 0.5: 1.2 rounds to 1.0, so it is not a change.
    assert!(
        !pump
            .server
            .set_property(id, ids.slow, ids.temp, &Value::F32(1.2))
            .unwrap()
    );
    assert!(
        pump.server
            .set_property(id, ids.slow, ids.temp, &Value::F32(1.6))
            .unwrap()
    );
    assert_eq!(
        pump.server.get_property(id, ids.slow, ids.temp).unwrap(),
        Value::F32(1.5)
    );
    pump.run(10);
    let quiet = pump.step(100_000, 1100);
    assert!(quiet[0].1.is_empty(), "converged scene sends nothing");
    assert_eq!(pump.server.peer_telemetry(1).unwrap().bytes_tick, 0);
}

#[test]
fn lost_packets_are_resent_and_late_packets_never_overwrite_newer_state() {
    let (reg, ids) = registry();
    let mut server = ReplicationServer::new(
        reg.clone(),
        ReplicationLimits::default(),
        ServerConfig::default(),
    )
    .unwrap();
    let mut client = ReplicationClient::new(reg, ReplicationLimits::default()).unwrap();
    server.add_peer(1).unwrap();
    server.set_peer_view(1, [0.0; 3], 100.0).unwrap();
    let id = server.spawn(basic(&ids, [0.0, 0.0, 0.0], 1, "x")).unwrap();
    let mut tick = 0;
    for _ in 0..4 {
        tick += 1;
        let p = server.build(1, tick, 5000, 1100).unwrap();
        deliver_all(&mut client, &p);
        ack(&mut server, 1, &mut client);
    }
    assert!(server.is_live_for(1, id));

    let pos_a = Value::Vec3([10.0, 0.0, 0.0]);
    server
        .set_property(id, ids.transform, ids.pos, &pos_a)
        .unwrap();
    tick += 1;
    let lost_then_late = server.build(1, tick, 5000, 1100).unwrap();
    assert_eq!(lost_then_late.len(), 1);
    server
        .set_property(id, ids.transform, ids.pos, &Value::Vec3([20.0, 0.0, 0.0]))
        .unwrap();
    // The authoritative value is the quantized one; clients converge to exactly it.
    let pos_b = server.get_property(id, ids.transform, ids.pos).unwrap();
    tick += 1;
    let newer = server.build(1, tick, 5000, 1100).unwrap();
    assert_eq!(newer.len(), 1);
    deliver_all(&mut client, &newer);
    ack(&mut server, 1, &mut client);
    assert_eq!(
        client.entity(id).unwrap().get(ids.transform, ids.pos),
        Some(&pos_b)
    );
    // The older packet shows up after the newer one: it must not roll the value back.
    deliver_all(&mut client, &lost_then_late);
    assert_eq!(
        client.entity(id).unwrap().get(ids.transform, ids.pos),
        Some(&pos_b)
    );
    assert!(client.telemetry().stale_blocks >= 1);

    // A packet that is really lost is repaired by resending the current value.
    server
        .set_property(id, ids.transform, ids.pos, &Value::Vec3([30.0, 0.0, 0.0]))
        .unwrap();
    let pos_c = server.get_property(id, ids.transform, ids.pos).unwrap();
    tick += 1;
    let dropped = server.build(1, tick, 5000, 1100).unwrap();
    assert_eq!(dropped.len(), 1);
    let mut resent = false;
    for _ in 0..80 {
        tick += 1;
        let p = server.build(1, tick, 5000, 1100).unwrap();
        resent |= !p.is_empty();
        deliver_all(&mut client, &p);
        ack(&mut server, 1, &mut client);
    }
    assert!(resent);
    assert_eq!(
        client.entity(id).unwrap().get(ids.transform, ids.pos),
        Some(&pos_c)
    );
}

#[test]
fn transport_refusal_releases_state_for_immediate_resend() {
    let (reg, ids) = registry();
    let mut pump = Pump::new(&reg, &[1]);
    pump.server.set_peer_view(1, [0.0; 3], 100.0).unwrap();
    let id = pump.server.spawn(basic(&ids, [0.0; 3], 9, "x")).unwrap();
    pump.run(5);
    pump.server
        .set_property(id, ids.health, ids.hp, &Value::U16(77))
        .unwrap();
    pump.tick += 1;
    let packets = pump.server.build(1, pump.tick, 5000, 1100).unwrap();
    assert_eq!(packets.len(), 1);
    // The host could not queue it.
    assert!(pump.server.mark_unsent(1, packets[0].sequence.unwrap()));
    pump.tick += 1;
    let retry = pump.server.build(1, pump.tick, 5000, 1100).unwrap();
    assert_eq!(retry.len(), 1, "state is resent on the very next tick");
    deliver_all(&mut pump.clients[0].1, &retry);
    assert_eq!(
        pump.clients[0]
            .1
            .entity(id)
            .unwrap()
            .get(ids.health, ids.hp),
        Some(&Value::U16(77))
    );
}

#[test]
fn delta_for_unknown_entity_and_late_spawn_after_despawn_are_harmless() {
    let (reg, ids) = registry();
    let mut server = ReplicationServer::new(
        reg.clone(),
        ReplicationLimits::default(),
        ServerConfig::default(),
    )
    .unwrap();
    let mut client = ReplicationClient::new(reg.clone(), ReplicationLimits::default()).unwrap();
    server.add_peer(1).unwrap();
    let id = server
        .spawn(basic(&ids, [0.0; 3], 5, "ghost").relevance(EntityRelevance::Always))
        .unwrap();
    let spawn_packets = server.build(1, 1, 5000, 1100).unwrap();
    assert!(!spawn_packets.is_empty());
    // The spawn is delayed; the entity is destroyed first.
    server.despawn(id).unwrap();
    let despawn_packets = server.build(1, 2, 5000, 1100).unwrap();
    assert!(!despawn_packets.is_empty());
    deliver_all(&mut client, &despawn_packets);
    deliver_all(&mut client, &spawn_packets);
    assert_eq!(client.entity_count(), 0, "a late spawn must not resurrect");
    assert!(client.drain_events().is_empty());

    // A hand-built update for an entity the client has never seen is ignored, not fatal.
    let mut body = vec![OP_UPDATE_TEST];
    NetIdBytes::put(&mut body, NetId::new(3, 0));
    body.extend([1, 1, 1]); // one component (Health), mask 0b1
    body.extend(5u16.to_le_bytes());
    let bytes = packet_with_blocks(&reg, 77, 5, 1, &body);
    client.receive(&bytes).unwrap();
    assert_eq!(client.entity_count(), 0);
    assert!(client.telemetry().stale_blocks >= 1);
}

const OP_UPDATE_TEST: u8 = 2;

struct NetIdBytes;
impl NetIdBytes {
    fn put(out: &mut Vec<u8>, id: NetId) {
        // index < 128 keeps the varint to one byte
        out.push(id.index() as u8);
        out.extend(id.generation().to_le_bytes());
    }
}

fn packet_with_blocks(
    reg: &ReplicationRegistry,
    seq: u32,
    tick: u32,
    count: u8,
    blocks: &[u8],
) -> Vec<u8> {
    let mut out = vec![PROTOCOL_VERSION, 1];
    out.extend(reg.fingerprint().to_le_bytes());
    out.extend(seq.to_le_bytes());
    out.extend(tick.to_le_bytes());
    out.push(count);
    out.extend(blocks);
    out
}

#[test]
fn net_id_reuse_changes_generation_and_stale_packets_cannot_alias() {
    let (reg, ids) = registry();
    let limits = ReplicationLimits {
        max_entities: 2,
        max_entities_per_peer: 2,
        ..ReplicationLimits::default()
    };
    let mut server =
        ReplicationServer::new(reg.clone(), limits.clone(), ServerConfig::default()).unwrap();
    let mut client = ReplicationClient::new(reg, limits).unwrap();
    server.add_peer(1).unwrap();
    let a = server
        .spawn(basic(&ids, [0.0; 3], 10, "a").relevance(EntityRelevance::Always))
        .unwrap();
    let _filler = server
        .spawn(basic(&ids, [0.0; 3], 10, "f").relevance(EntityRelevance::Always))
        .unwrap();
    let mut tick = 0;
    fn step(server: &mut ReplicationServer, client: &mut ReplicationClient, tick: &mut u32) {
        *tick += 1;
        let p = server.build(1, *tick, 5000, 1100).unwrap();
        deliver_all(client, &p);
        ack(server, 1, client);
    }
    for _ in 0..4 {
        step(&mut server, &mut client, &mut tick);
    }
    // An update for `a` is built but never delivered.
    server
        .set_property(a, ids.health, ids.hp, &Value::U16(99))
        .unwrap();
    tick += 1;
    let stale_update = server.build(1, tick, 5000, 1100).unwrap();
    assert_eq!(stale_update.len(), 1);

    server.despawn(a).unwrap();
    for _ in 0..4 {
        step(&mut server, &mut client, &mut tick);
    }
    assert!(client.entity(a).is_none());
    let b = server
        .spawn(basic(&ids, [0.0; 3], 10, "b").relevance(EntityRelevance::Always))
        .unwrap();
    assert_eq!(
        b.index(),
        a.index(),
        "the exhausted index space reuses the slot"
    );
    assert_ne!(b.generation(), a.generation());
    for _ in 0..4 {
        step(&mut server, &mut client, &mut tick);
    }
    assert_eq!(
        client.entity(b).unwrap().get(ids.health, ids.hp),
        Some(&Value::U16(10))
    );
    assert!(client.entity(a).is_none(), "the old ID no longer resolves");

    // The stale update for the previous incarnation finally arrives.
    deliver_all(&mut client, &stale_update);
    assert_eq!(
        client.entity(b).unwrap().get(ids.health, ids.hp),
        Some(&Value::U16(10)),
        "stale generation must not touch the new entity"
    );
    assert!(
        server
            .set_property(a, ids.health, ids.hp, &Value::U16(1))
            .is_err()
    );
    assert!(!server.contains(a));
}

#[test]
fn callbacks_and_events_report_old_and_new_values() {
    let (reg, ids) = registry();
    let mut pump = Pump::new(&reg, &[1]);
    pump.server.set_peer_view(1, [0.0; 3], 100.0).unwrap();
    let seen: Arc<Mutex<Vec<(NetId, Value, Value)>>> = Arc::default();
    let sink = seen.clone();
    pump.client(1)
        .on_property_changed(ids.health, ids.hp, move |c| {
            sink.lock()
                .unwrap()
                .push((c.entity, c.old.clone(), c.new.clone()));
        })
        .unwrap();
    assert!(
        pump.client(1)
            .on_property_changed(ids.health, PropertyId(9), |_| {})
            .is_err()
    );
    let id = pump.server.spawn(basic(&ids, [0.0; 3], 10, "n")).unwrap();
    pump.run(5);
    assert!(
        seen.lock().unwrap().is_empty(),
        "spawn state is not a change"
    );
    for hp in [11u16, 12, 13] {
        pump.server
            .set_property(id, ids.health, ids.hp, &Value::U16(hp))
            .unwrap();
        pump.run(3);
    }
    let seen = seen.lock().unwrap().clone();
    let expect: Vec<_> = [(10u16, 11u16), (11, 12), (12, 13)]
        .into_iter()
        .map(|(o, n)| (id, Value::U16(o), Value::U16(n)))
        .collect();
    assert_eq!(seen, expect);
    let events = pump.client(1).drain_events();
    let changes: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            ClientEvent::PropertyChanged(c) => Some((c.old.clone(), c.new.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(changes.len(), 3);
    assert!(matches!(events[0], ClientEvent::Spawned { entity, owned: false } if entity == id));
}

#[test]
fn lifecycle_and_ownership_events() {
    let (reg, ids) = registry();
    let mut pump = Pump::new(&reg, &[1, 2]);
    for p in [1, 2] {
        pump.server.set_peer_view(p, [0.0; 3], 100.0).unwrap();
    }
    // `plain` has no ownership-dependent component: ownership is a small flag update.
    let plain = pump.server.spawn(basic(&ids, [0.0; 3], 1, "p")).unwrap();
    // `bag` has an owner-only component: ownership changes resync the affected peers.
    let bag = pump
        .server
        .spawn(with_inventory(
            &ids,
            basic(&ids, [1.0; 3], 1, "b").owner(1),
            5,
            "n",
        ))
        .unwrap();
    pump.run(6);
    assert!(pump.client(1).entity(bag).unwrap().owned);
    assert!(
        pump.client(1)
            .entity(bag)
            .unwrap()
            .component(ids.inventory)
            .is_some()
    );
    assert!(
        pump.client(2)
            .entity(bag)
            .unwrap()
            .component(ids.inventory)
            .is_none()
    );
    pump.client(1).drain_events();
    pump.client(2).drain_events();

    pump.server.set_owner(plain, Some(2)).unwrap();
    pump.run(5);
    assert!(pump.client(2).entity(plain).unwrap().owned);
    assert_eq!(
        pump.client(2).drain_events(),
        vec![ClientEvent::OwnerChanged {
            entity: plain,
            owned: true
        }]
    );

    pump.server.set_owner(bag, Some(2)).unwrap();
    pump.run(8);
    let e1 = pump.client(1).drain_events();
    assert!(e1.contains(&ClientEvent::Despawned { entity: bag }));
    assert!(e1.contains(&ClientEvent::Spawned {
        entity: bag,
        owned: false
    }));
    assert!(
        pump.client(1)
            .entity(bag)
            .unwrap()
            .component(ids.inventory)
            .is_none()
    );
    assert!(pump.client(2).entity(bag).unwrap().owned);
    assert!(
        pump.client(2)
            .entity(bag)
            .unwrap()
            .component(ids.inventory)
            .is_some()
    );

    pump.server.despawn(plain).unwrap();
    pump.run(5);
    assert!(pump.client(1).entity(plain).is_none());
    assert!(
        pump.client(2)
            .drain_events()
            .contains(&ClientEvent::Despawned { entity: plain })
    );
    assert!(
        !pump.server.is_tracked_for(1, plain),
        "baseline dropped after the despawn ack"
    );
}

#[test]
fn delivery_classes_follow_the_component_rules() {
    let (reg, ids) = registry();
    let mut pump = Pump::new(&reg, &[1]);
    pump.server.set_peer_view(1, [0.0; 3], 100.0).unwrap();
    let id = pump.server.spawn(basic(&ids, [0.0; 3], 1, "d")).unwrap();
    let first = pump.step(5000, 1100);
    assert!(
        first[0]
            .1
            .iter()
            .all(|p| p.delivery == Delivery::ReliableOrdered && p.lane == Lane::REPLICATION)
    );
    pump.run(5);
    pump.server
        .set_property(id, ids.transform, ids.pos, &Value::Vec3([5.0, 0.0, 0.0]))
        .unwrap();
    pump.server
        .set_property(id, ids.health, ids.hp, &Value::U16(2))
        .unwrap();
    let step = pump.step(5000, 1100);
    let kinds: Vec<_> = step[0].1.iter().map(|p| p.delivery).collect();
    assert!(kinds.contains(&Delivery::ReliableOrdered));
    assert!(kinds.contains(&Delivery::Unreliable));
    assert!(step[0].1.iter().all(|p| p.lane == Lane::REPLICATION));
}

#[test]
fn max_update_rate_caps_property_frequency() {
    let (reg, ids) = registry();
    let mut pump = Pump::new(&reg, &[1]);
    pump.server.set_peer_view(1, [0.0; 3], 100.0).unwrap();
    let init = basic(&ids, [0.0; 3], 1, "r").component(ids.slow, vec![Value::F32(0.0)]);
    let id = pump.server.spawn(init).unwrap();
    pump.run(8);
    pump.client(1).drain_events();
    for t in 0..60 {
        pump.server
            .set_property(id, ids.slow, ids.temp, &Value::F32((t + 1) as f32))
            .unwrap();
        pump.step(5000, 1100);
    }
    let updates = pump
        .client(1)
        .drain_events()
        .iter()
        .filter(|e| matches!(e, ClientEvent::PropertyChanged(_)))
        .count();
    // 10 Hz at a 60 Hz tick rate: one update per 6 ticks.
    assert!((9..=11).contains(&updates), "{updates} updates in 60 ticks");
}

#[test]
fn schema_mismatch_is_rejected_on_both_ends() {
    let (reg, ids) = registry();
    let other = ReplicationRegistry::builder()
        .component(ComponentDef::new("Transform").property("position", ValueSpec::vec3()))
        .build()
        .unwrap();
    assert_ne!(reg.schema_hash(), other.schema_hash());
    let mut server = ReplicationServer::new(
        reg.clone(),
        ReplicationLimits::default(),
        ServerConfig::default(),
    )
    .unwrap();
    server.add_peer(1).unwrap();
    server
        .spawn(basic(&ids, [0.0; 3], 1, "s").relevance(EntityRelevance::Always))
        .unwrap();
    let packets = server.build(1, 1, 5000, 1100).unwrap();
    let mut foreign =
        ReplicationClient::new(Arc::new(other), ReplicationLimits::default()).unwrap();
    assert_eq!(
        foreign.receive(&packets[0].bytes),
        Err(ReplicationError::SchemaMismatch)
    );
    assert_eq!(foreign.entity_count(), 0);
    let mut client = ReplicationClient::new(reg, ReplicationLimits::default()).unwrap();
    client.receive(&packets[0].bytes).unwrap();
    let mut ack = client.build_ack().unwrap().bytes;
    ack[2] ^= 1; // corrupt the fingerprint
    assert_eq!(
        server.receive(1, &ack),
        Err(ReplicationError::SchemaMismatch)
    );
}

#[test]
fn server_state_validation_rejects_misuse() {
    let (reg, ids) = registry();
    let mut server =
        ReplicationServer::new(reg, ReplicationLimits::default(), ServerConfig::default()).unwrap();
    server.add_peer(1).unwrap();
    assert!(server.add_peer(1).is_err());
    assert!(
        server
            .spawn(basic(&ids, [f32::NAN, 0.0, 0.0], 1, "n"))
            .is_err()
    );
    assert!(server.spawn(EntityInit::new().owner(9)).is_err());
    assert!(
        server
            .spawn(EntityInit::new().relevance(EntityRelevance::Spatial))
            .is_err()
    );
    assert!(
        server
            .spawn(
                EntityInit::new()
                    .component(ids.health, vec![Value::U16(1)])
                    .component(ids.health, vec![Value::U16(2)])
            )
            .is_err()
    );
    assert!(
        server
            .spawn(EntityInit::new().component(ids.health, vec![Value::U8(1)]))
            .is_err()
    );
    let id = server.spawn(basic(&ids, [0.0; 3], 1, "ok")).unwrap();
    assert!(
        server
            .set_property(id, ids.inventory, ids.ammo, &Value::U16(1))
            .is_err()
    );
    assert!(
        server
            .set_property(id, ids.health, ids.hp, &Value::F32(1.0))
            .is_err()
    );
    assert!(server.set_peer_view(1, [0.0; 3], f32::INFINITY).is_err());
    assert!(server.set_owner(id, Some(5)).is_err());
    assert!(server.build(1, 1, 100, 10).is_err());
    assert!(server.build(2, 1, 100, 1100).is_err());
    assert!(server.remove_peer(1));
    assert!(!server.remove_peer(1));
}

#[test]
fn rpc_lane_is_independent_and_follows_its_delivery_class() {
    let (reg, ids) = registry();
    let mut sim = Simulation::new(
        reg,
        ReplicationLimits::default(),
        ServerConfig::default(),
        &LinkConfig::hostile(),
        42,
        1,
    )
    .unwrap();
    sim.server.set_peer_view(1, [0.0; 3], 100.0).unwrap();
    let id = sim
        .server
        .spawn(basic(&ids, [0.0; 3], 1, "target"))
        .unwrap();
    for _ in 0..60 {
        sim.step(3000, 1100);
    }
    assert!(sim.server.is_live_for(1, id));
    sim.clients[0].client.drain_events();
    for n in 0..50u16 {
        let hit = sim
            .server
            .call_rpc(ids.hit, RpcTarget::Peer(1), Some(id), &[Value::U16(n)])
            .unwrap();
        sim.server_send(0, hit.into_iter().next().unwrap().1);
        let note = sim
            .server
            .call_rpc(
                ids.announce,
                RpcTarget::Peer(1),
                None,
                &[Value::String("n".into())],
            )
            .unwrap();
        sim.server_send(0, note.into_iter().next().unwrap().1);
        sim.step(3000, 1100);
    }
    for _ in 0..30 {
        sim.step(3000, 1100);
    }
    let events = sim.clients[0].client.drain_events();
    let reliable: Vec<u16> = events
        .iter()
        .filter_map(|e| match e {
            ClientEvent::RpcReceived { rpc, args, .. } if *rpc == ids.hit => match args[0] {
                Value::U16(n) => Some(n),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let unreliable = events
        .iter()
        .filter(|e| matches!(e, ClientEvent::RpcReceived { rpc, .. } if *rpc == ids.announce))
        .count();
    assert_eq!(
        reliable,
        (0..50).collect::<Vec<u16>>(),
        "reliable RPCs arrive once, in order"
    );
    assert!(
        (20..=70).contains(&unreliable),
        "{unreliable} unreliable RPCs arrived"
    );
}
