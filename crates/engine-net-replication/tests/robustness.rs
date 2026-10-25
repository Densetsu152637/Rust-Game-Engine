mod common;

use common::*;
use engine_net_replication::testing::Rng;
use engine_net_replication::*;

fn small_limits() -> ReplicationLimits {
    ReplicationLimits {
        max_entities: 64,
        max_entities_per_peer: 64,
        max_pending_events: 256,
        ..ReplicationLimits::default()
    }
}

/// A realistic mix of valid packets: spawns, deltas, owner flips, despawns, acks and rpcs.
struct Corpus {
    state: Vec<Vec<u8>>,
    acks: Vec<Vec<u8>>,
    rpcs: Vec<Vec<u8>>,
}

fn corpus() -> Corpus {
    let (reg, ids) = registry();
    let limits = small_limits();
    let mut server =
        ReplicationServer::new(reg.clone(), limits.clone(), ServerConfig::default()).unwrap();
    let mut clients: Vec<_> = (1..=2)
        .map(|p| {
            server.add_peer(p).unwrap();
            server.set_peer_view(p, [0.0; 3], 500.0).unwrap();
            ReplicationClient::new(reg.clone(), limits.clone()).unwrap()
        })
        .collect();
    let mut rng = Rng::new(99);
    let mut live = Vec::new();
    let mut c = Corpus {
        state: Vec::new(),
        acks: Vec::new(),
        rpcs: Vec::new(),
    };
    for tick in 1..=60u32 {
        if tick % 6 == 1 && live.len() < 12 {
            let pos = [rng.range(-100.0, 100.0), rng.range(-100.0, 100.0), 0.0];
            let mut init = basic(&ids, pos, 5, &format!("ent{tick}"));
            if tick % 12 == 1 {
                init = with_inventory(&ids, init.owner(1), 3, "bag")
                    .component(ids.observer, vec![Value::U8(4)])
                    .component(ids.slow, vec![Value::F32(1.0)]);
            }
            live.push(server.spawn(init).unwrap());
        }
        if tick % 17 == 0 {
            let id = live.remove(0);
            server.despawn(id).unwrap();
        }
        if tick % 23 == 0 {
            server.set_owner(live[live.len() - 1], Some(2)).unwrap();
        }
        for id in &live {
            let _ =
                server.set_property(*id, ids.health, ids.hp, &Value::U16(rng.below(500) as u16));
            let _ = server.set_property(
                *id,
                ids.transform,
                ids.pos,
                &Value::Vec3([rng.range(-100.0, 100.0), 0.0, 1.0]),
            );
        }
        for (i, client) in clients.iter_mut().enumerate() {
            let peer = i as u64 + 1;
            for p in server.build(peer, tick, 4000, 1100).unwrap() {
                client.receive(&p.bytes).unwrap();
                c.state.push(p.bytes);
            }
            if let Some(a) = client.build_ack() {
                server.receive(peer, &a.bytes).unwrap();
                c.acks.push(a.bytes);
            }
        }
    }
    let owned = live
        .iter()
        .copied()
        .find(|id| server.owner(*id) == Some(1))
        .unwrap_or(live[0]);
    c.rpcs.push(
        clients[0]
            .call_server(ids.chat, None, &[Value::String("hello".into())])
            .unwrap()
            .bytes,
    );
    c.rpcs.push(
        clients[0]
            .call_server(ids.poke, Some(owned), &[])
            .map(|p| p.bytes)
            .unwrap_or_else(|_| {
                clients[0]
                    .call_server(ids.chat, None, &[Value::String("x".into())])
                    .unwrap()
                    .bytes
            }),
    );
    c
}

fn mutate(rng: &mut Rng, base: &[u8]) -> Vec<u8> {
    let mut v = base.to_vec();
    match rng.below(6) {
        0 => {
            for _ in 0..=rng.below(4) {
                if !v.is_empty() {
                    let i = rng.below(v.len() as u64) as usize;
                    v[i] ^= 1 << rng.below(8);
                }
            }
        }
        1 => v.truncate(rng.below(v.len() as u64 + 1) as usize),
        2 => {
            for _ in 0..=rng.below(32) {
                v.push(rng.below(256) as u8);
            }
        }
        3 => {
            let keep = rng.below(8).min(v.len() as u64) as usize;
            v.truncate(keep);
            for _ in 0..rng.below(200) {
                v.push(rng.below(256) as u8);
            }
        }
        4 => {
            if !v.is_empty() {
                let i = rng.below(v.len() as u64) as usize;
                v[i] = [0x00, 0xff, 0x80, 0x7f][rng.below(4) as usize];
            }
        }
        _ => {
            // Splice a random window of another position over the packet.
            if v.len() > 8 {
                let a = rng.below(v.len() as u64 - 4) as usize;
                let b = rng.below(v.len() as u64 - 4) as usize;
                let chunk: Vec<u8> = v[a..a + 4].to_vec();
                v[b..b + 4].copy_from_slice(&chunk);
            }
        }
    }
    v
}

#[test]
fn client_survives_20k_mutated_and_random_packets() {
    let (reg, _) = registry();
    let c = corpus();
    assert!(c.state.len() > 20);
    let limits = small_limits();
    let mut client = ReplicationClient::new(reg.clone(), limits.clone()).unwrap();
    for p in &c.state {
        let _ = client.receive(p);
    }
    let mut rng = Rng::new(0xC0FFEE);
    let (mut ok, mut err) = (0u32, 0u32);
    for i in 0..20_000u32 {
        let packet = if i % 5 == 4 {
            // Random bytes behind a valid header so decoding reaches the block parser.
            let mut v = vec![PROTOCOL_VERSION, 1];
            v.extend(reg.fingerprint().to_le_bytes());
            for _ in 0..rng.below(120) {
                v.push(rng.below(256) as u8);
            }
            v
        } else {
            let base = &c.state[rng.below(c.state.len() as u64) as usize];
            mutate(&mut rng, base)
        };
        match client.receive(&packet) {
            Ok(()) => ok += 1,
            Err(_) => err += 1,
        }
        assert!(client.entity_count() <= limits.max_entities);
        if i % 50 == 0 {
            assert!(client.drain_events().len() <= limits.max_pending_events);
            for e in client.entities() {
                assert!(e.id.index() < limits.max_entities as u32);
            }
        }
    }
    eprintln!("client fuzz: {ok} accepted, {err} rejected");
    assert!(err > 1000, "mutations should be rejected often");
    assert!(ok > 100, "some mutations remain valid packets");
}

#[test]
fn server_survives_20k_mutated_acks_and_rpcs() {
    let (reg, ids) = registry();
    let c = corpus();
    let limits = small_limits();
    let mut server =
        ReplicationServer::new(reg.clone(), limits.clone(), ServerConfig::default()).unwrap();
    server.add_peer(1).unwrap();
    server.set_peer_view(1, [0.0; 3], 500.0).unwrap();
    for i in 0..10 {
        server
            .spawn(basic(&ids, [i as f32, 0.0, 0.0], 1, "x").owner(1))
            .unwrap();
    }
    let mut rng = Rng::new(0xBEEF);
    let mut tick = 0;
    let (mut ok, mut err) = (0u32, 0u32);
    for i in 0..20_000u32 {
        let base: &Vec<u8> = if i % 2 == 0 {
            &c.acks[rng.below(c.acks.len() as u64) as usize]
        } else {
            &c.rpcs[rng.below(c.rpcs.len() as u64) as usize]
        };
        let packet = if i % 7 == 6 {
            let mut v = vec![PROTOCOL_VERSION, 2 + (i % 2) as u8];
            v.extend(reg.fingerprint().to_le_bytes());
            for _ in 0..rng.below(60) {
                v.push(rng.below(256) as u8);
            }
            v
        } else {
            mutate(&mut rng, base)
        };
        match server.receive(1, &packet) {
            Ok(()) => ok += 1,
            Err(_) => err += 1,
        }
        if i % 40 == 0 {
            tick += 1;
            let out = server.build(1, tick, 3000, 1100).unwrap();
            assert!(out.iter().map(|p| p.bytes.len()).sum::<usize>() <= 3000);
            assert!(server.drain_events().len() <= limits.max_pending_events);
        }
    }
    eprintln!("server fuzz: {ok} accepted, {err} rejected");
    assert!(err > 1000 && ok > 100);
}

// ------------------------------------------------------------------ targeted malformed cases

fn header(reg: &ReplicationRegistry, kind: u8, seq: u32) -> Vec<u8> {
    let mut v = vec![PROTOCOL_VERSION, kind];
    v.extend(reg.fingerprint().to_le_bytes());
    if kind == 1 {
        v.extend(seq.to_le_bytes());
        v.extend(1u32.to_le_bytes());
    }
    v
}

fn state(reg: &ReplicationRegistry, count: &[u8], blocks: &[u8]) -> Vec<u8> {
    let mut v = header(reg, 1, 10);
    v.extend(count);
    v.extend(blocks);
    v
}

/// Spawn block for entity index `idx` with a Health component only (valid).
fn spawn_health(idx: u8, hp: u16) -> Vec<u8> {
    let mut b = vec![1, idx, 0, 0, 0, 1, 1];
    b.extend(hp.to_le_bytes());
    b
}

fn assert_rejected(client: &mut ReplicationClient, bytes: &[u8], what: &str) -> ReplicationError {
    let before = client.entity_count();
    let err = client
        .receive(bytes)
        .expect_err(&format!("{what} must be rejected"));
    assert_eq!(client.entity_count(), before, "{what} must change nothing");
    assert!(client.drain_events().is_empty(), "{what} must emit nothing");
    err
}

#[test]
fn targeted_malformed_state_packets_are_rejected_without_side_effects() {
    let (reg, _) = registry();
    let mut client = ReplicationClient::new(reg.clone(), small_limits()).unwrap();
    let huge = {
        let mut v = vec![0xff; 9];
        v.push(0x01); // u64::MAX
        v
    };
    assert_rejected(&mut client, &[], "empty");
    assert_rejected(&mut client, &[PROTOCOL_VERSION], "header only");
    let mut wrong_version = state(&reg, &[0], &[]);
    wrong_version[0] = 9;
    assert_rejected(&mut client, &wrong_version, "wrong version");
    let mut wrong_kind = state(&reg, &[0], &[]);
    wrong_kind[1] = 9;
    assert_rejected(&mut client, &wrong_kind, "wrong kind");
    let mut wrong_schema = state(&reg, &[0], &[]);
    wrong_schema[2] ^= 0x55;
    assert_eq!(
        assert_rejected(&mut client, &wrong_schema, "schema mismatch"),
        ReplicationError::SchemaMismatch
    );
    assert!(matches!(
        assert_rejected(&mut client, &state(&reg, &huge, &[]), "u64::MAX blocks"),
        ReplicationError::Limit(_)
    ));
    assert!(matches!(
        assert_rejected(&mut client, &state(&reg, &[0xb8, 0x17], &[]), "3000 blocks"),
        ReplicationError::Limit(_)
    ));
    assert!(matches!(
        assert_rejected(
            &mut client,
            &state(&reg, &[100], &spawn_health(1, 1)),
            "count > bytes"
        ),
        ReplicationError::Malformed(_)
    ));
    let mut unknown_op = spawn_health(1, 1);
    unknown_op[0] = 9;
    assert_rejected(&mut client, &state(&reg, &[1], &unknown_op), "unknown op");
    let mut unknown_comp = spawn_health(1, 1);
    unknown_comp[6] = 200;
    assert_rejected(
        &mut client,
        &state(&reg, &[1], &unknown_comp),
        "unknown component",
    );
    let mut bad_flag = spawn_health(1, 1);
    bad_flag[4] = 2;
    assert_rejected(&mut client, &state(&reg, &[1], &bad_flag), "spawn flag 2");
    let mut trailing = spawn_health(1, 1);
    trailing.push(0);
    assert_rejected(&mut client, &state(&reg, &[1], &trailing), "trailing byte");
    // Entity index at/above max_entities (64).
    let mut far_index = spawn_health(1, 1);
    far_index[1] = 64;
    assert!(matches!(
        assert_rejected(&mut client, &state(&reg, &[1], &far_index), "index 64"),
        ReplicationError::Limit(_)
    ));
    // Components out of order: Health(1) then Transform(0).
    let ooo = [1u8, 1, 0, 0, 0, 2, 1, 5, 0, 0];
    assert_rejected(
        &mut client,
        &state(&reg, &[1], &ooo),
        "descending components",
    );
    // Updates: zero components, zero mask, mask bit beyond the property count.
    for (what, body) in [
        ("empty update", vec![2u8, 1, 0, 0, 0]),
        ("zero mask", vec![2, 1, 0, 0, 1, 1, 0, 9, 9]),
        ("mask beyond props", vec![2, 1, 0, 0, 1, 1, 2, 9, 9]),
    ] {
        assert_rejected(&mut client, &state(&reg, &[1], &body), what);
    }
    assert_rejected(
        &mut client,
        &state(&reg, &[1], &[4, 1, 0, 0, 7]),
        "owner flag 7",
    );
    assert_rejected(
        &mut client,
        &state(&reg, &[1], &[3, 1, 0]),
        "truncated despawn id",
    );

    // Invalid UTF-8 and over-long strings in a Name component (component 3, one property).
    let name_block = |name: &[u8]| {
        let mut b = vec![1u8, 1, 0, 0, 0, 1, 3];
        b.push(name.len() as u8);
        b.extend(name);
        b
    };
    assert_rejected(
        &mut client,
        &state(&reg, &[1], &name_block(&[0xff, 0xfe])),
        "bad utf8",
    );
    assert!(matches!(
        assert_rejected(
            &mut client,
            &state(&reg, &[1], &name_block(&[b'a'; 40])),
            "long string"
        ),
        ReplicationError::Limit(_)
    ));
    // Quantized value above its declared range (Transform position, 20-bit range).
    let mut transform = vec![1u8, 1, 0, 0, 0, 1, 0];
    transform.extend([0xff, 0xff, 0xff]);
    transform.extend([0; 6 + 4]);
    assert_rejected(
        &mut client,
        &state(&reg, &[1], &transform),
        "range overflow",
    );
    // A valid control still works afterwards.
    client
        .receive(&state(&reg, &[1], &spawn_health(1, 42)))
        .unwrap();
    assert_eq!(client.entity_count(), 1);
}

#[test]
fn every_truncation_of_a_valid_packet_is_rejected() {
    let (reg, _) = registry();
    let c = corpus();
    let biggest = c.state.iter().max_by_key(|p| p.len()).unwrap();
    for len in 0..biggest.len() {
        let mut client = ReplicationClient::new(reg.clone(), small_limits()).unwrap();
        assert!(
            client.receive(&biggest[..len]).is_err(),
            "prefix of {len}/{} bytes accepted",
            biggest.len()
        );
        assert_eq!(client.entity_count(), 0);
        assert!(client.drain_events().is_empty());
    }
    let mut client = ReplicationClient::new(reg, small_limits()).unwrap();
    client.receive(biggest).unwrap();
}

#[test]
fn duplicate_packets_apply_once() {
    let (reg, ids) = registry();
    let mut server = ReplicationServer::new(
        reg.clone(),
        ReplicationLimits::default(),
        ServerConfig::default(),
    )
    .unwrap();
    server.add_peer(1).unwrap();
    server
        .spawn(basic(&ids, [0.0; 3], 1, "dup").relevance(EntityRelevance::Always))
        .unwrap();
    let packets = server.build(1, 1, 5000, 1100).unwrap();
    let mut client = ReplicationClient::new(reg, ReplicationLimits::default()).unwrap();
    for _ in 0..3 {
        client.receive(&packets[0].bytes).unwrap();
    }
    assert_eq!(client.entity_count(), 1);
    assert_eq!(client.drain_events().len(), 1);
    assert_eq!(client.telemetry().duplicate_packets, 2);
}

#[test]
fn server_rejects_malformed_inbound_packets() {
    let (reg, ids) = registry();
    let limits = small_limits();
    let mut server =
        ReplicationServer::new(reg.clone(), limits.clone(), ServerConfig::default()).unwrap();
    server.add_peer(1).unwrap();
    let ack = |largest: u32, bits: u64| {
        let mut v = header(&reg, 2, 0);
        v.extend(largest.to_le_bytes());
        v.extend(bits.to_le_bytes());
        v
    };
    assert!(
        server.receive(1, &ack(5, 0)).is_err(),
        "ack for an unsent packet"
    );
    server
        .spawn(basic(&ids, [0.0; 3], 1, "a").relevance(EntityRelevance::Always))
        .unwrap();
    let sent = server.build(1, 1, 5000, 1100).unwrap();
    assert_eq!(sent[0].sequence, Some(1));
    assert!(
        server.receive(1, &ack(2, 0)).is_err(),
        "ack beyond the newest sent packet"
    );
    assert!(
        server.receive(1, &ack(1, 0)[..10]).is_err(),
        "truncated ack"
    );
    let mut padded = ack(1, 0);
    padded.push(0);
    assert!(server.receive(1, &padded).is_err(), "trailing byte");
    server.receive(1, &ack(1, u64::MAX)).unwrap();
    assert_eq!(server.peer_telemetry(1).unwrap().packets_acked, 1);
    assert!(
        server.receive(1, &state(&reg, &[0], &[])).is_err(),
        "state from a client"
    );
    assert!(server.receive(7, &ack(1, 0)).is_err(), "unknown peer");
    assert!(matches!(
        server.receive(1, &vec![0u8; limits.max_packet_bytes + 1]),
        Err(ReplicationError::Limit(_))
    ));
    // RPC framing: too many calls, oversize payload, unknown flags, truncated.
    let rpc = |body: &[u8]| {
        let mut v = header(&reg, 3, 0);
        v.extend(body);
        v
    };
    assert!(matches!(
        server.receive(1, &rpc(&[200, 1])),
        Err(ReplicationError::Limit(_))
    ));
    assert!(matches!(
        server.receive(1, &rpc(&[1, 0, 0, 0xff, 0x7f])),
        Err(ReplicationError::Limit(_))
    ));
    assert!(
        server.receive(1, &rpc(&[1, 0, 2, 0])).is_err(),
        "unknown call flags"
    );
    assert!(
        server.receive(1, &rpc(&[1, 0, 0, 5, 1])).is_err(),
        "payload truncated"
    );
    assert!(
        server.receive(1, &rpc(&[2, 0, 0, 0])).is_err(),
        "second call missing"
    );
    // A well-formed unknown call is a soft rejection.
    server.receive(1, &rpc(&[1, 77, 0, 0])).unwrap();
    assert_eq!(server.drain_events().len(), 1);
}

#[test]
fn public_value_decoder_rejects_non_finite_and_padding_bits() {
    let raw = ValueSpec::vec3();
    let mut nan = Vec::new();
    nan.extend(1.0f32.to_le_bytes());
    nan.extend(f32::NAN.to_le_bytes());
    nan.extend(0.0f32.to_le_bytes());
    assert!(raw.decode_exact(&nan).is_err());
    let mut inf = Vec::new();
    for v in [0.0f32, 0.0, f32::INFINITY] {
        inf.extend(v.to_le_bytes());
    }
    assert!(raw.decode_exact(&inf).is_err());
    // 9-bit smallest-three occupies 29 bits; bits above that must be zero.
    let quat = ValueSpec::quat().quantized(Quantization::SmallestThree { bits: 9 });
    assert!(quat.decode_exact(&[0, 0, 0, 0]).is_ok());
    assert!(quat.decode_exact(&[0, 0, 0, 0b1110_0000]).is_err());
    // Decoded quaternions are normalised.
    let Value::Quat(q) = quat.decode_exact(&[0x55, 0x55, 0x55, 0x15]).unwrap() else {
        panic!()
    };
    let len = q.iter().map(|c| c * c).sum::<f32>().sqrt();
    assert!((len - 1.0).abs() < 1e-5);
    // Unknown trailing bytes are rejected by the exact decoder.
    assert!(ValueSpec::u8().decode_exact(&[1, 2]).is_err());
}
