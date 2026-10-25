mod common;

use common::*;
use engine_net_replication::testing::Rng;
use engine_net_replication::*;
use std::collections::BTreeMap;

fn spawn_scene(pump: &mut Pump, ids: &Ids, n: usize, rng: &mut Rng) -> Vec<NetId> {
    (0..n)
        .map(|i| {
            let pos = [rng.range(-900.0, 900.0), rng.range(-900.0, 900.0), 0.0];
            pump.server
                .spawn(basic(ids, pos, 0, &format!("entity-{i}")))
                .unwrap()
        })
        .collect()
}

#[test]
fn budget_and_payload_are_hard_limits_and_every_entity_stays_fresh() {
    let (reg, ids) = registry();
    let mut pump = Pump::new(&reg, &[1]);
    pump.server.set_peer_view(1, [0.0; 3], 100_000.0).unwrap();
    let mut rng = Rng::new(3);
    let scene = spawn_scene(&mut pump, &ids, 500, &mut rng);
    let (budget, mtu) = (1500usize, 1100usize);

    let mut ticks_to_spawn = 0;
    while scene.iter().any(|id| !pump.server.is_live_for(1, *id)) {
        ticks_to_spawn += 1;
        assert!(ticks_to_spawn < 400, "spawns starved");
        for (_, packets) in pump.step(budget, mtu) {
            let total: usize = packets.iter().map(|p| p.bytes.len()).sum();
            assert!(total <= budget);
            assert!(packets.iter().all(|p| p.bytes.len() <= mtu));
        }
    }

    let mut max_age = 0u32;
    let mut sent_total = 0usize;
    for t in 0..120u32 {
        let value = (pump.tick + 1) as u16;
        for id in &scene {
            pump.server
                .set_property(*id, ids.health, ids.hp, &Value::U16(value))
                .unwrap();
            pump.server
                .set_property(
                    *id,
                    ids.transform,
                    ids.pos,
                    &Value::Vec3([f32::from(value), 0.0, 0.0]),
                )
                .unwrap();
        }
        for (_, packets) in pump.step(budget, mtu) {
            let total: usize = packets.iter().map(|p| p.bytes.len()).sum();
            assert!(total <= budget, "tick {t}: {total} > {budget}");
            assert!(packets.iter().all(|p| p.bytes.len() <= mtu));
            sent_total += total;
        }
        if t >= 40 {
            for id in &scene {
                let Some(Value::U16(hp)) = pump.clients[0]
                    .1
                    .entity(*id)
                    .and_then(|e| e.get(ids.health, ids.hp).cloned())
                else {
                    panic!("entity missing from mirror");
                };
                max_age = max_age.max(pump.tick - u32::from(hp));
            }
        }
    }
    let tel = pump.server.peer_telemetry(1).unwrap();
    eprintln!(
        "500 entities, {budget} B/tick budget: max staleness {max_age} ticks, {} B/tick used, {} deferred/tick",
        sent_total / 120,
        tel.entities_deferred
    );
    assert!(
        max_age <= 24,
        "an entity went {max_age} ticks without an update"
    );
    assert!(
        tel.entities_deferred > 0,
        "the budget really was the bottleneck"
    );
    assert_eq!(tel.oversize_blocks, 0);
}

#[test]
fn zero_budget_defers_everything_and_nothing_is_lost() {
    let (reg, ids) = registry();
    let mut pump = Pump::new(&reg, &[1]);
    pump.server.set_peer_view(1, [0.0; 3], 10_000.0).unwrap();
    let mut rng = Rng::new(5);
    let scene = spawn_scene(&mut pump, &ids, 20, &mut rng);
    for _ in 0..3 {
        let step = pump.step(0, 1100);
        assert!(step[0].1.is_empty());
    }
    assert_eq!(pump.server.peer_telemetry(1).unwrap().entities_deferred, 20);
    pump.run(8);
    assert!(scene.iter().all(|id| pump.client(1).entity(*id).is_some()));
}

#[test]
fn blocks_larger_than_max_payload_are_reported_not_sent() {
    let (reg, ids) = registry();
    let mut pump = Pump::new(&reg, &[1]);
    pump.server.set_peer_view(1, [0.0; 3], 100.0).unwrap();
    pump.server
        .spawn(basic(&ids, [0.0; 3], 0, "0123456789abcdef0123456789abcdef"))
        .unwrap();
    for (_, packets) in pump.step(10_000, 64) {
        assert!(packets.iter().all(|p| p.bytes.len() <= 64));
    }
    let tel = pump.server.peer_telemetry(1).unwrap();
    assert!(tel.oversize_blocks > 0);
    assert_eq!(tel.packets_total, 0);
}

#[test]
fn higher_priority_components_win_a_tight_budget_but_nothing_starves() {
    let (reg, ids) = registry();
    let mut pump = Pump::new(&reg, &[1]);
    pump.server.set_peer_view(1, [0.0; 3], 100_000.0).unwrap();
    let mut rng = Rng::new(11);
    let scene = spawn_scene(&mut pump, &ids, 40, &mut rng);
    pump.run(8);
    pump.client(1).drain_events();
    let (hp_group, pos_group) = scene.split_at(20);
    let budget = 150;
    for _ in 0..90u32 {
        let value = (pump.tick + 1) as u16;
        for id in hp_group {
            pump.server
                .set_property(*id, ids.health, ids.hp, &Value::U16(value))
                .unwrap();
        }
        for id in pos_group {
            pump.server
                .set_property(
                    *id,
                    ids.transform,
                    ids.pos,
                    &Value::Vec3([f32::from(value), 0.0, 0.0]),
                )
                .unwrap();
        }
        let step = pump.step(budget, 1100);
        assert!(step[0].1.iter().map(|p| p.bytes.len()).sum::<usize>() <= budget);
    }
    let mut hp_updates = 0;
    let mut pos_updates: BTreeMap<NetId, u32> = BTreeMap::new();
    for e in pump.client(1).drain_events() {
        if let ClientEvent::PropertyChanged(c) = e {
            if c.component == ids.health {
                hp_updates += 1;
            } else {
                *pos_updates.entry(c.entity).or_default() += 1;
            }
        }
    }
    let pos_total: u32 = pos_updates.values().sum();
    eprintln!("priority 4 updates {hp_updates}, priority 1 updates {pos_total}");
    assert!(hp_updates > pos_total, "{hp_updates} <= {pos_total}");
    for id in pos_group {
        assert!(
            pos_updates.get(id).copied().unwrap_or(0) >= 3,
            "low priority entity {id} starved"
        );
    }
}
