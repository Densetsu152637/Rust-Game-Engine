mod common;

use common::*;
use engine_net_replication::*;

#[test]
fn out_of_radius_entities_are_never_sent_and_enter_leave_is_per_peer() {
    let (reg, ids) = registry();
    let mut pump = Pump::new(&reg, &[1, 2]);
    pump.server.set_peer_view(1, [0.0; 3], 50.0).unwrap();
    pump.server
        .set_peer_view(2, [1000.0, 0.0, 0.0], 50.0)
        .unwrap();
    let near: Vec<NetId> = (0..5)
        .map(|i| {
            pump.server
                .spawn(basic(
                    &ids,
                    [i as f32 * 3.0, 0.0, 0.0],
                    10,
                    &format!("NEAR-{i}"),
                ))
                .unwrap()
        })
        .collect();
    let far = pump
        .server
        .spawn(basic(&ids, [1000.0, 0.0, 0.0], 10, "FAR-AWAY-ENTITY"))
        .unwrap();
    pump.run(20);
    // Peer 1 sees only the near group, peer 2 only the far entity.
    assert_eq!(pump.client(1).entity_count(), 5);
    assert!(near.iter().all(|id| pump.client(1).entity(*id).is_some()));
    assert!(pump.client(1).entity(far).is_none());
    assert_eq!(pump.client(2).entity_count(), 1);
    assert!(pump.client(2).entity(far).is_some());
    for (peer, packet) in &pump.log {
        if *peer == 1 {
            assert!(
                !contains(&packet.bytes, b"FAR-AWAY"),
                "far name leaked to peer 1"
            );
        } else {
            assert!(
                !contains(&packet.bytes, b"NEAR-"),
                "near names leaked to peer 2"
            );
        }
    }
    let t = pump.server.peer_telemetry(1).unwrap();
    assert_eq!(t.entities_relevant, 5);
    assert!(
        !pump.server.is_tracked_for(1, far),
        "no state is even tracked for hidden entities"
    );

    // Walk the far entity into peer 1's radius, then out beyond the hysteresis band.
    pump.server.set_position(far, [40.0, 0.0, 0.0]).unwrap();
    pump.run(6);
    assert!(pump.client(1).entity(far).is_some());
    pump.server.set_position(far, [54.0, 0.0, 0.0]).unwrap();
    pump.run(6);
    assert!(
        pump.client(1).entity(far).is_some(),
        "inside radius * (1 + hysteresis) the entity stays visible"
    );
    pump.server.set_position(far, [60.0, 0.0, 0.0]).unwrap();
    pump.run(6);
    assert!(pump.client(1).entity(far).is_none());
    assert!(!pump.server.is_tracked_for(1, far));
}

#[test]
fn reentering_relevance_sends_the_full_current_state() {
    let (reg, ids) = registry();
    let mut pump = Pump::new(&reg, &[1]);
    pump.server.set_peer_view(1, [0.0; 3], 50.0).unwrap();
    let id = pump
        .server
        .spawn(basic(&ids, [10.0, 0.0, 0.0], 10, "wanderer"))
        .unwrap();
    pump.run(6);
    pump.client(1).drain_events();
    let before = pump
        .server
        .peer_telemetry(1)
        .unwrap()
        .full_state_bytes_total;

    pump.server.set_position(id, [500.0, 0.0, 0.0]).unwrap();
    pump.run(6);
    assert!(pump.client(1).entity(id).is_none());
    // State changes while the peer cannot see the entity.
    let sent_before = pump.log.len();
    pump.server
        .set_property(id, ids.health, ids.hp, &Value::U16(321))
        .unwrap();
    pump.server
        .set_property(id, ids.transform, ids.pos, &Value::Vec3([500.0, 1.0, 2.0]))
        .unwrap();
    pump.run(3);
    assert_eq!(
        pump.log.len(),
        sent_before,
        "hidden changes cost no bandwidth at all"
    );

    pump.server.set_position(id, [20.0, 0.0, 0.0]).unwrap();
    pump.run(6);
    let ce = pump.client(1).entity(id).expect("re-spawned");
    assert_eq!(ce.get(ids.health, ids.hp), Some(&Value::U16(321)));
    let events = pump.client(1).drain_events();
    assert!(events.contains(&ClientEvent::Despawned { entity: id }));
    assert!(events.contains(&ClientEvent::Spawned {
        entity: id,
        owned: false
    }));
    let after = pump
        .server
        .peer_telemetry(1)
        .unwrap()
        .full_state_bytes_total;
    assert!(
        after > before,
        "re-entry is a full-state spawn, not a delta"
    );
}

#[test]
fn owner_only_and_skip_owner_data_is_never_encoded_for_other_peers() {
    let (reg, ids) = registry();
    let mut pump = Pump::new(&reg, &[1, 2, 3]);
    for p in [1, 2, 3] {
        pump.server.set_peer_view(p, [0.0; 3], 100.0).unwrap();
    }
    let init = with_inventory(
        &ids,
        basic(&ids, [0.0; 3], 100, "hero").owner(1),
        77,
        "SECRET-NOTE-ALPHA",
    )
    .component(ids.observer, vec![Value::U8(0xEE)]);
    let id = pump.server.spawn(init).unwrap();
    pump.run(8);
    for note in ["SECRET-NOTE-BRAVO", "SECRET-NOTE-CHARLIE"] {
        pump.server
            .set_property(id, ids.inventory, ids.note, &Value::String(note.into()))
            .unwrap();
        pump.server
            .set_property(id, ids.inventory, ids.ammo, &Value::U16(1234))
            .unwrap();
        pump.run(5);
    }
    let mut owner_saw_secret = false;
    for (peer, packet) in &pump.log {
        let has_secret = contains(&packet.bytes, b"SECRET-NOTE");
        if *peer == 1 {
            owner_saw_secret |= has_secret;
        } else {
            assert!(!has_secret, "peer {peer} received owner-only bytes");
        }
    }
    assert!(owner_saw_secret);
    let owner = pump.client(1).entity(id).unwrap();
    assert_eq!(
        owner.get(ids.inventory, ids.note),
        Some(&Value::String("SECRET-NOTE-CHARLIE".into()))
    );
    for other in [2, 3] {
        let e = pump.client(other).entity(id).unwrap();
        assert!(e.component(ids.inventory).is_none());
        assert!(e.component(ids.observer).is_some());
    }
    // The skip-owner component is never encoded for the owner.
    assert!(
        pump.client(1)
            .entity(id)
            .unwrap()
            .component(ids.observer)
            .is_none()
    );
    // Reassigning ownership moves the secret: peer 1 loses it, peer 2 gains it.
    pump.server.set_owner(id, Some(2)).unwrap();
    pump.run(10);
    assert!(
        pump.client(1)
            .entity(id)
            .unwrap()
            .component(ids.inventory)
            .is_none()
    );
    assert!(
        pump.client(2)
            .entity(id)
            .unwrap()
            .component(ids.inventory)
            .is_some()
    );
    let leaked_to_3 = pump
        .log
        .iter()
        .filter(|(p, _)| *p == 3)
        .any(|(_, pk)| contains(&pk.bytes, b"SECRET"));
    assert!(!leaked_to_3);
}

#[test]
fn initial_only_components_are_sent_once_per_incarnation() {
    let (reg, ids) = registry();
    let mut pump = Pump::new(&reg, &[1]);
    pump.server.set_peer_view(1, [0.0; 3], 50.0).unwrap();
    let id = pump
        .server
        .spawn(basic(&ids, [0.0; 3], 1, "first-name"))
        .unwrap();
    pump.run(6);
    pump.server
        .set_property(
            id,
            ids.name,
            ids.name_v,
            &Value::String("second-name".into()),
        )
        .unwrap();
    pump.run(10);
    assert_eq!(
        pump.client(1).entity(id).unwrap().get(ids.name, ids.name_v),
        Some(&Value::String("first-name".into()))
    );
    assert!(
        !pump
            .log
            .iter()
            .any(|(_, p)| contains(&p.bytes, b"second-name"))
    );
    // Re-entering relevance is a new spawn and carries the current initial state.
    pump.server.set_position(id, [900.0, 0.0, 0.0]).unwrap();
    pump.run(6);
    pump.server.set_position(id, [0.0; 3]).unwrap();
    pump.run(6);
    assert_eq!(
        pump.client(1).entity(id).unwrap().get(ids.name, ids.name_v),
        Some(&Value::String("second-name".into()))
    );
}

#[test]
fn custom_callback_can_hide_and_force_entities() {
    let (reg, ids) = registry();
    let mut pump = Pump::new(&reg, &[1, 2]);
    for p in [1, 2] {
        pump.server.set_peer_view(p, [0.0; 3], 50.0).unwrap();
    }
    let team_a = pump
        .server
        .spawn(basic(&ids, [1.0; 3], 1, "team-a").tag(1))
        .unwrap();
    let hidden_from_2 = pump
        .server
        .spawn(basic(&ids, [2.0; 3], 1, "stealth").tag(9))
        .unwrap();
    let far_but_forced = pump
        .server
        .spawn(basic(&ids, [5000.0, 0.0, 0.0], 1, "beacon").tag(7))
        .unwrap();
    pump.server
        .set_relevance_callback(Some(Box::new(|peer, view| match (view.tag, peer) {
            (9, 2) => Relevance::Hidden,
            (7, 1) => Relevance::Visible(2.0),
            _ => Relevance::Default,
        })));
    pump.run(10);
    assert!(pump.client(1).entity(team_a).is_some());
    assert!(pump.client(2).entity(team_a).is_some());
    assert!(pump.client(1).entity(hidden_from_2).is_some());
    assert!(pump.client(2).entity(hidden_from_2).is_none());
    assert!(pump.client(1).entity(far_but_forced).is_some());
    assert!(pump.client(2).entity(far_but_forced).is_none());
    assert!(
        !pump
            .log
            .iter()
            .any(|(p, pk)| *p == 2 && contains(&pk.bytes, b"stealth"))
    );

    // Clearing the callback reverts to the built-in rules.
    pump.server.set_relevance_callback(None);
    pump.run(10);
    assert!(pump.client(1).entity(far_but_forced).is_none());
    assert!(pump.client(2).entity(hidden_from_2).is_some());
}

#[test]
fn relevance_modes_always_and_owner_only() {
    let (reg, ids) = registry();
    let mut pump = Pump::new(&reg, &[1, 2]);
    for p in [1, 2] {
        pump.server.set_peer_view(p, [0.0; 3], 10.0).unwrap();
    }
    let world = pump
        .server
        .spawn(basic(&ids, [9000.0, 0.0, 0.0], 1, "world").relevance(EntityRelevance::Always))
        .unwrap();
    let private = pump
        .server
        .spawn(
            basic(&ids, [0.0; 3], 1, "private")
                .owner(2)
                .relevance(EntityRelevance::OwnerOnly),
        )
        .unwrap();
    pump.run(8);
    assert!(pump.client(1).entity(world).is_some());
    assert!(pump.client(2).entity(world).is_some());
    assert!(pump.client(1).entity(private).is_none());
    assert!(pump.client(2).entity(private).is_some());
    assert!(
        !pump
            .log
            .iter()
            .any(|(p, pk)| *p == 1 && contains(&pk.bytes, b"private"))
    );
}

#[test]
fn a_peer_without_a_view_sees_no_spatial_entities() {
    let (reg, ids) = registry();
    let mut pump = Pump::new(&reg, &[1]);
    pump.server
        .spawn(basic(&ids, [0.0; 3], 1, "spatial"))
        .unwrap();
    pump.run(5);
    assert_eq!(pump.client(1).entity_count(), 0);
}
