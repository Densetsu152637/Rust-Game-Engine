mod common;

use common::*;
use engine_net_replication::*;
use engine_network::{Delivery, Lane};

struct Setup {
    pump: Pump,
    ids: Ids,
    mine: NetId,
    theirs: NetId,
}

fn setup() -> Setup {
    let (reg, ids) = registry();
    let mut pump = Pump::new(&reg, &[1, 2]);
    for p in [1, 2] {
        pump.server.set_peer_view(p, [0.0; 3], 100.0).unwrap();
    }
    let mine = pump
        .server
        .spawn(basic(&ids, [0.0; 3], 1, "mine").owner(1))
        .unwrap();
    let theirs = pump
        .server
        .spawn(basic(&ids, [1.0; 3], 1, "theirs").owner(2))
        .unwrap();
    pump.run(8);
    Setup {
        pump,
        ids,
        mine,
        theirs,
    }
}

fn fire_args() -> Vec<Value> {
    vec![Value::U8(2), Value::Vec3([0.0, 1.0, 0.0])]
}

fn rejected(events: &[ServerEvent]) -> Vec<RpcRejectReason> {
    events
        .iter()
        .filter_map(|e| match e {
            ServerEvent::RpcRejected { reason, .. } => Some(*reason),
            _ => None,
        })
        .collect()
}

#[test]
fn valid_owner_call_is_delivered_with_typed_args() {
    let mut s = setup();
    let packet = s
        .pump
        .client(1)
        .call_server(s.ids.fire, Some(s.mine), &fire_args())
        .unwrap();
    assert_eq!(packet.lane, Lane::RPC);
    assert_eq!(packet.delivery, Delivery::ReliableOrdered);
    s.pump.server.receive(1, &packet.bytes).unwrap();
    let events = s.pump.server.drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        ServerEvent::RpcReceived {
            peer,
            rpc,
            entity,
            args,
        } => {
            assert_eq!((*peer, *rpc, *entity), (1, s.ids.fire, Some(s.mine)));
            assert_eq!(args[0], Value::U8(2));
            assert_eq!(args[1], Value::Vec3([0.0, 1.0, 0.0]));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn ownership_visibility_and_target_rules_are_enforced() {
    let mut s = setup();
    // Peer 1 fires at an entity owned by peer 2: refused, connection untouched.
    let packet = s
        .pump
        .client(1)
        .call_server(s.ids.fire, Some(s.theirs), &fire_args())
        .unwrap();
    s.pump.server.receive(1, &packet.bytes).unwrap();
    assert_eq!(
        rejected(&s.pump.server.drain_events()),
        vec![RpcRejectReason::NotOwner]
    );

    // An entity the peer cannot see looks exactly like a missing one.
    let hidden = s
        .pump
        .server
        .spawn(basic(&s.ids, [5000.0, 0.0, 0.0], 1, "hidden"))
        .unwrap();
    s.pump.run(3);
    let forged = engine_net_replication_forge(&s.pump, s.ids.poke, Some(hidden), &[]);
    s.pump.server.receive(1, &forged).unwrap();
    let missing = engine_net_replication_forge(&s.pump, s.ids.poke, Some(NetId::new(100, 0)), &[]);
    s.pump.server.receive(1, &missing).unwrap();
    assert_eq!(
        rejected(&s.pump.server.drain_events()),
        vec![RpcRejectReason::UnknownEntity; 2]
    );

    // Missing / unexpected targets.
    let no_target = engine_net_replication_forge(&s.pump, s.ids.poke, None, &[]);
    s.pump.server.receive(1, &no_target).unwrap();
    let extra_target = engine_net_replication_forge(
        &s.pump,
        s.ids.chat,
        Some(s.mine),
        &[Value::String("hi".into())],
    );
    s.pump.server.receive(1, &extra_target).unwrap();
    assert_eq!(
        rejected(&s.pump.server.drain_events()),
        vec![
            RpcRejectReason::MissingTarget,
            RpcRejectReason::UnexpectedTarget
        ]
    );
}

/// Builds an RPC packet bypassing the client-side direction/target checks.
fn engine_net_replication_forge(
    pump: &Pump,
    rpc: RpcId,
    entity: Option<NetId>,
    args: &[Value],
) -> Vec<u8> {
    let reg = pump.server.registry().clone();
    let def = reg.rpc(rpc).unwrap();
    let mut payload = Vec::new();
    for (arg, v) in def.args.iter().zip(args) {
        arg.spec.encode(v, &mut payload).unwrap();
    }
    let mut out = vec![PROTOCOL_VERSION, 3];
    out.extend(reg.fingerprint().to_le_bytes());
    out.push(1); // one call
    out.push(rpc.0 as u8);
    out.push(u8::from(entity.is_some()));
    if let Some(id) = entity {
        assert!(id.index() < 128);
        if id.index() >= 128 {
            unreachable!()
        }
        out.push(id.index() as u8);
        out.extend(id.generation().to_le_bytes());
    }
    out.push(payload.len() as u8);
    out.extend(payload);
    out
}

#[test]
fn unknown_wrong_direction_and_bad_args_are_soft_rejections() {
    let mut s = setup();
    let unknown = engine_net_replication_forge_raw(&s.pump, 99, None, &[]);
    s.pump.server.receive(1, &unknown).unwrap();
    // Server-to-client rpc sent by a client.
    let wrong_way =
        engine_net_replication_forge(&s.pump, s.ids.announce, None, &[Value::String("x".into())]);
    s.pump.server.receive(1, &wrong_way).unwrap();
    // Invalid UTF-8 argument.
    let mut bad_utf8 =
        engine_net_replication_forge(&s.pump, s.ids.chat, None, &[Value::String("ab".into())]);
    let n = bad_utf8.len();
    bad_utf8[n - 1] = 0xff;
    s.pump.server.receive(1, &bad_utf8).unwrap();
    // Truncated argument payload for the declared types.
    let short = engine_net_replication_forge_raw(&s.pump, s.ids.fire.0 as u8, Some(s.mine), &[2]);
    s.pump.server.receive(1, &short).unwrap();
    // Trailing bytes after valid arguments.
    let mut long =
        engine_net_replication_forge(&s.pump, s.ids.chat, None, &[Value::String("ok".into())]);
    long.push(0);
    // keep framing intact: bump the declared argument length
    let len_at = long.len() - 5;
    long[len_at] += 1;
    s.pump.server.receive(1, &long).unwrap();
    assert_eq!(
        rejected(&s.pump.server.drain_events()),
        vec![
            RpcRejectReason::UnknownRpc,
            RpcRejectReason::WrongDirection,
            RpcRejectReason::BadArgs,
            RpcRejectReason::BadArgs,
            RpcRejectReason::BadArgs,
        ]
    );
    let t = s.pump.server.peer_telemetry(1).unwrap();
    assert_eq!(t.rpc_rejected, 5);
    assert_eq!(t.rpc_rejected_by_reason["bad_args"], 3);
}

fn engine_net_replication_forge_raw(
    pump: &Pump,
    rpc: u8,
    entity: Option<NetId>,
    payload: &[u8],
) -> Vec<u8> {
    let mut out = vec![PROTOCOL_VERSION, 3];
    out.extend(pump.server.registry().fingerprint().to_le_bytes());
    out.push(1);
    out.push(rpc);
    out.push(u8::from(entity.is_some()));
    if let Some(id) = entity {
        out.push(id.index() as u8);
        out.extend(id.generation().to_le_bytes());
    }
    out.push(payload.len() as u8);
    out.extend(payload);
    out
}

#[test]
fn client_side_checks_refuse_misuse_before_sending() {
    let mut s = setup();
    let c = s.pump.client(1);
    assert!(
        c.call_server(s.ids.hit, Some(s.mine), &[Value::U16(1)])
            .is_err()
    );
    assert!(c.call_server(s.ids.fire, None, &fire_args()).is_err());
    assert!(
        c.call_server(s.ids.fire, Some(s.mine), &[Value::U8(1)])
            .is_err()
    );
    assert!(
        c.call_server(
            s.ids.fire,
            Some(s.mine),
            &[Value::U16(1), Value::Vec3([0.0; 3])]
        )
        .is_err()
    );
    assert!(
        c.call_server(s.ids.chat, None, &[Value::String("x".repeat(65))])
            .is_err()
    );
    assert!(
        c.call_server(s.ids.poke, Some(NetId::new(77, 0)), &[])
            .is_err()
    );
}

#[test]
fn rate_limit_rejects_bursts_and_recovers_over_time() {
    let mut s = setup();
    // Fire allows 5 calls per second: a burst of 8 gets 5 through.
    for _ in 0..8 {
        let p = s
            .pump
            .client(1)
            .call_server(s.ids.fire, Some(s.mine), &fire_args())
            .unwrap();
        s.pump.server.receive(1, &p.bytes).unwrap();
    }
    let events = s.pump.server.drain_events();
    let ok = events
        .iter()
        .filter(|e| matches!(e, ServerEvent::RpcReceived { .. }))
        .count();
    assert_eq!(ok, 5);
    assert_eq!(rejected(&events), vec![RpcRejectReason::RateLimited; 3]);

    // Another peer has its own bucket, and so does another rpc.
    let other = s
        .pump
        .client(2)
        .call_server(s.ids.fire, Some(s.theirs), &fire_args())
        .unwrap();
    s.pump.server.receive(2, &other.bytes).unwrap();
    assert!(matches!(
        s.pump.server.drain_events()[..],
        [ServerEvent::RpcReceived { peer: 2, .. }]
    ));

    // One second (60 ticks) later the bucket has refilled.
    s.pump.run(60);
    let p = s
        .pump
        .client(1)
        .call_server(s.ids.fire, Some(s.mine), &fire_args())
        .unwrap();
    s.pump.server.receive(1, &p.bytes).unwrap();
    assert!(matches!(
        s.pump.server.drain_events()[..],
        [ServerEvent::RpcReceived { peer: 1, .. }]
    ));
}

#[test]
fn server_rpcs_reach_the_right_peers_only() {
    let mut s = setup();
    // Peer 2's view moves away from `mine`'s area, so only peer 1 has `mine`... both are
    // near the origin here, so shrink peer 2's view to exclude it.
    s.pump
        .server
        .set_peer_view(2, [900.0, 0.0, 0.0], 10.0)
        .unwrap();
    s.pump.run(8);
    assert!(s.pump.client(2).entity(s.mine).is_none());

    // Multicast to peers that have the entity: only peer 1.
    let out = s
        .pump
        .server
        .call_rpc(
            s.ids.hit,
            RpcTarget::Relevant,
            Some(s.mine),
            &[Value::U16(33)],
        )
        .unwrap();
    assert_eq!(out.iter().map(|(p, _)| *p).collect::<Vec<_>>(), vec![1]);
    assert_eq!(out[0].1.lane, Lane::RPC);
    assert_eq!(out[0].1.delivery, Delivery::ReliableOrdered);
    s.pump.client(1).receive(&out[0].1.bytes).unwrap();
    let events = s.pump.client(1).drain_events();
    assert!(events.contains(&ClientEvent::RpcReceived {
        rpc: s.ids.hit,
        entity: Some(s.mine),
        args: vec![Value::U16(33)],
    }));

    // Owner and explicit peer targets.
    let to_owner = s
        .pump
        .server
        .call_rpc(s.ids.hit, RpcTarget::Owner, Some(s.mine), &[Value::U16(1)])
        .unwrap();
    assert_eq!(to_owner[0].0, 1);
    // Peer 2 does not have `mine`: naming it must fail instead of leaking the ID.
    assert!(
        s.pump
            .server
            .call_rpc(
                s.ids.hit,
                RpcTarget::Peer(2),
                Some(s.mine),
                &[Value::U16(1)]
            )
            .is_err()
    );
    // Entity-less broadcast to a chosen peer, unreliable delivery.
    let note = s
        .pump
        .server
        .call_rpc(
            s.ids.announce,
            RpcTarget::Peer(2),
            None,
            &[Value::String("round over".into())],
        )
        .unwrap();
    assert_eq!(note[0].1.delivery, Delivery::Unreliable);
    s.pump.client(2).receive(&note[0].1.bytes).unwrap();

    // Misuse is refused locally.
    assert!(
        s.pump
            .server
            .call_rpc(s.ids.fire, RpcTarget::Owner, Some(s.mine), &[])
            .is_err()
    );
    assert!(
        s.pump
            .server
            .call_rpc(s.ids.hit, RpcTarget::Peer(1), None, &[Value::U16(1)])
            .is_err()
    );
    assert!(
        s.pump
            .server
            .call_rpc(s.ids.hit, RpcTarget::Peer(1), Some(s.mine), &[Value::U8(1)])
            .is_err()
    );
    assert_eq!(s.pump.server.peer_telemetry(1).unwrap().rpc_sent, 2);
}

#[test]
fn clients_drop_rpcs_for_unknown_entities_or_wrong_direction() {
    let mut s = setup();
    let ghost = s
        .pump
        .server
        .call_rpc(
            s.ids.hit,
            RpcTarget::Peer(1),
            Some(s.mine),
            &[Value::U16(5)],
        )
        .unwrap();
    s.pump.server.despawn(s.mine).unwrap();
    s.pump.run(6);
    s.pump.client(1).drain_events();
    // The entity is gone by the time the RPC arrives: dropped, not delivered.
    s.pump.client(1).receive(&ghost[0].1.bytes).unwrap();
    assert!(s.pump.client(1).drain_events().is_empty());
    assert_eq!(s.pump.client(1).telemetry().rpc_dropped, 1);
    // A client-to-server rpc id arriving at a client is ignored.
    let forged =
        engine_net_replication_forge(&s.pump, s.ids.chat, None, &[Value::String("x".into())]);
    s.pump.client(1).receive(&forged).unwrap();
    assert_eq!(s.pump.client(1).telemetry().rpc_dropped, 2);
}
