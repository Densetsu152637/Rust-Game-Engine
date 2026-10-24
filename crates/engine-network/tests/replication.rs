use engine_network::*;
use std::{
    io::Write,
    net::TcpStream,
    thread,
    time::{Duration, Instant},
};

fn until(mut test: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !test() {
        assert!(Instant::now() < deadline, "loopback operation timed out");
        thread::sleep(Duration::from_millis(1));
    }
}
fn server() -> Server {
    Server::bind(ServerConfig::default()).unwrap()
}
fn client(server: &Server) -> Client {
    Client::connect(ClientConfig::new(server.local_addr().unwrap())).unwrap()
}
fn command(tick: u64, id: &str) -> InputCommand {
    InputCommand {
        tick,
        entity_id: id.into(),
        movement: [1.0, 0.0, 0.0],
        buttons: 0,
    }
}
fn connect(server: &mut Server, clients: &mut [&mut Client]) {
    until(|| {
        server.poll(PollBudget::default()).unwrap();
        for client in clients.iter_mut() {
            client.poll(PollBudget::default()).unwrap();
        }
        clients.iter().all(|client| client.is_connected())
    });
}
#[test]
fn real_two_client_authority_replication_and_disconnect() {
    let mut server = server();
    let mut a = client(&server);
    let mut b = client(&server);
    connect(&mut server, &mut [&mut a, &mut b]);
    let a_id = a.peer_id().unwrap();
    let b_id = b.peer_id().unwrap();
    assert_ne!(a_id, b_id);
    server.assign_owner("hero", a_id).unwrap();
    server.assign_owner("guest", b_id).unwrap();
    a.send_input(command(1, "hero")).unwrap();
    b.send_input(command(1, "hero")).unwrap();
    let mut accepted = 0;
    let mut rejected = 0;
    until(|| {
        a.poll(PollBudget::default()).unwrap();
        b.poll(PollBudget::default()).unwrap();
        for event in server.poll(PollBudget::default()).unwrap() {
            match event {
                ServerEvent::Input { peer_id, command } => {
                    assert_eq!(peer_id, a_id);
                    assert_eq!(command.entity_id, "hero");
                    accepted += 1;
                }
                ServerEvent::InputRejected { peer_id, .. } => {
                    assert_eq!(peer_id, b_id);
                    rejected += 1;
                }
                _ => {}
            }
        }
        accepted == 1 && rejected == 1
    });
    let snapshot = Snapshot {
        tick: 1,
        entities: vec![
            EntitySnapshot::identity("hero"),
            EntitySnapshot::identity("guest"),
        ],
    };
    server.publish_snapshot(snapshot.clone()).unwrap();
    until(|| {
        server.poll(PollBudget::default()).unwrap();
        a.poll(PollBudget::default()).unwrap();
        b.poll(PollBudget::default()).unwrap();
        a.state().latest().is_some() && b.state().latest().is_some()
    });
    assert_eq!(a.state().latest(), Some(&snapshot));
    assert_eq!(b.state().latest(), Some(&snapshot));
    server
        .publish_snapshot(Snapshot {
            tick: 2,
            entities: vec![],
        })
        .unwrap();
    until(|| {
        server.poll(PollBudget::default()).unwrap();
        a.poll(PollBudget::default()).unwrap();
        b.poll(PollBudget::default()).unwrap();
        a.state().latest().unwrap().tick == 2 && b.state().latest().unwrap().tick == 2
    });
    assert!(a.state().latest().unwrap().entities.is_empty());
    a.disconnect();
    until(|| {
        server.poll(PollBudget::default()).unwrap().iter().any(
            |event| matches!(event, ServerEvent::Disconnected { peer_id, .. } if *peer_id == a_id),
        )
    });
    assert_eq!(server.peer_ids().count(), 1);
}
#[test]
fn state_interpolates_trs_velocity_and_applies_spawn_despawn_atomically() {
    let mut state = ReplicationState::default();
    let old = EntitySnapshot::identity("hero");
    state
        .apply(Snapshot {
            tick: 10,
            entities: vec![old.clone(), EntitySnapshot::identity("gone")],
        })
        .unwrap();
    let mut new = old;
    new.translation = [10.0, 0.0, 0.0];
    new.scale = [3.0; 3];
    new.velocity = [2.0; 3];
    new.rotation = [0.0, 0.0, 1.0, 0.0];
    state
        .apply(Snapshot {
            tick: 20,
            entities: vec![new, EntitySnapshot::identity("spawn")],
        })
        .unwrap();
    let sampled = state.sample(15.0).unwrap();
    assert_eq!(sampled.entities.len(), 2);
    assert_eq!(sampled.entities[0].translation, [5.0, 0.0, 0.0]);
    assert_eq!(sampled.entities[0].scale, [2.0; 3]);
    assert_eq!(sampled.entities[0].velocity, [1.0; 3]);
    assert!(
        (sampled.entities[0].rotation[2].abs() - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.001
    );
    assert_eq!(sampled.entities[1].id, "spawn");
    assert!(
        state
            .apply(Snapshot {
                tick: 20,
                entities: vec![]
            })
            .is_err()
    );
    assert!(
        state
            .apply(Snapshot {
                tick: 21,
                entities: vec![EntitySnapshot::identity("duplicate"); 2]
            })
            .is_err()
    );
    assert_eq!(state.latest().unwrap().tick, 20);
    assert_eq!(
        state.sample(-1.0).unwrap().entities[0].translation,
        [0.0; 3]
    );
    assert_eq!(
        state.sample(50.0).unwrap().entities[0].translation,
        [10.0, 0.0, 0.0]
    );
}
fn write_frame(stream: &mut TcpStream, json: &[u8]) {
    stream
        .write_all(&(json.len() as u32).to_be_bytes())
        .unwrap();
    stream.write_all(json).unwrap();
}
#[test]
fn raw_client_replay_future_tick_and_transform_injection_are_rejected() {
    let mut server = server();
    let mut stream = TcpStream::connect(server.local_addr().unwrap()).unwrap();
    until(|| {
        server.poll(PollBudget::default()).unwrap();
        server.peer_ids().count() == 1
    });
    let peer = server.peer_ids().next().unwrap();
    server.assign_owner("hero", peer).unwrap();
    let mut received = Vec::new();
    for tick in [1, 1, 10000] {
        write_frame(&mut stream, serde_json::to_string(&serde_json::json!({"type":"Input", "tick":tick,"entity_id":"hero","movement":[0,0,0],"buttons":0})).unwrap().as_bytes());
    }
    until(|| {
        received.extend(server.poll(PollBudget::default()).unwrap());
        received.len() == 3
    });
    assert!(matches!(received[0], ServerEvent::Input { .. }));
    assert!(matches!(received[1], ServerEvent::InputRejected { .. }));
    assert!(matches!(received[2], ServerEvent::InputRejected { .. }));
    write_frame(&mut stream, br#"{"type":"Input","tick":2,"entity_id":"hero","movement":[0,0,0],"buttons":0,"translation":[100,0,0]}"#);
    until(|| {
        server
            .poll(PollBudget::default())
            .unwrap()
            .iter()
            .any(|e| matches!(e, ServerEvent::Disconnected { .. }))
    });
    assert_eq!(server.peer_ids().count(), 0);
}
#[test]
fn resource_caps_and_zero_budget_are_enforced() {
    let limits = Limits {
        max_peers: 1,
        max_queued_frames: 1,
        max_messages_per_poll: 1,
        ..Limits::default()
    };
    let mut server = Server::bind(ServerConfig {
        limits,
        ..ServerConfig::default()
    })
    .unwrap();
    let mut a = client(&server);
    assert!(
        server
            .poll(PollBudget {
                max_messages: 0,
                max_bytes: 0,
                max_accepts: 0
            })
            .unwrap()
            .is_empty()
    );
    assert_eq!(server.peer_ids().count(), 0);
    server
        .poll(PollBudget {
            max_messages: 0,
            max_bytes: 0,
            max_accepts: 1,
        })
        .unwrap();
    assert!(matches!(
        server.publish_snapshot(Snapshot {
            tick: 1,
            entities: vec![]
        }),
        Err(Error::QueueFull)
    ));
    connect(&mut server, &mut [&mut a]);
    server
        .publish_snapshot(Snapshot {
            tick: 1,
            entities: vec![],
        })
        .unwrap();
    assert!(
        server
            .publish_snapshot(Snapshot {
                tick: 1,
                entities: vec![]
            })
            .is_err()
    );
    let mut b = client(&server);
    until(|| {
        server.poll(PollBudget::default()).unwrap();
        b.poll(PollBudget::default())
            .unwrap()
            .iter()
            .any(|e| matches!(e, ClientEvent::Disconnected(_)))
    });
    assert_eq!(server.peer_ids().count(), 1);
}
#[test]
fn idle_and_oversized_frames_disconnect_without_unbounded_work() {
    let mut server = Server::bind(ServerConfig {
        limits: Limits {
            idle_timeout: Duration::from_millis(30),
            ..Limits::default()
        },
        ..ServerConfig::default()
    })
    .unwrap();
    let _idle = TcpStream::connect(server.local_addr().unwrap()).unwrap();
    until(|| {
        server.poll(PollBudget::default()).unwrap();
        server.peer_ids().count() == 1
    });
    until(|| {
        server
            .poll(PollBudget::default())
            .unwrap()
            .iter()
            .any(|e| matches!(e, ServerEvent::Disconnected { .. }))
    });
    let mut stream = TcpStream::connect(server.local_addr().unwrap()).unwrap();
    stream.write_all(&u32::MAX.to_be_bytes()).unwrap();
    until(|| {
        server
            .poll(PollBudget::default())
            .unwrap()
            .iter()
            .any(|e| matches!(e, ServerEvent::Disconnected { .. }))
    });
}

#[test]
fn final_authorized_input_is_delivered_before_half_close() {
    let mut server = server();
    let mut stream = TcpStream::connect(server.local_addr().unwrap()).unwrap();
    until(|| {
        server.poll(PollBudget::default()).unwrap();
        server.peer_ids().count() == 1
    });
    let peer = server.peer_ids().next().unwrap();
    server.assign_owner("hero", peer).unwrap();
    write_frame(
        &mut stream,
        br#"{"type":"Input","tick":1,"entity_id":"hero","movement":[0,0,0],"buttons":0}"#,
    );
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    let mut events = Vec::new();
    until(|| {
        events.extend(server.poll(PollBudget::default()).unwrap());
        events
            .iter()
            .any(|e| matches!(e, ServerEvent::Disconnected { .. }))
    });
    assert!(matches!(
        events.as_slice(),
        [ServerEvent::Input { .. }, ServerEvent::Disconnected { .. }]
    ));
}

#[test]
fn invalid_snapshots_preserve_state_and_oversize_serialization_fails() {
    let mut state = ReplicationState::default();
    state
        .apply(Snapshot {
            tick: 1,
            entities: vec![EntitySnapshot::identity("ok")],
        })
        .unwrap();
    let mut bad = EntitySnapshot::identity("bad");
    bad.translation[0] = f32::NAN;
    assert!(
        state
            .apply(Snapshot {
                tick: 2,
                entities: vec![bad]
            })
            .is_err()
    );
    assert_eq!(state.latest().unwrap().tick, 1);
    let mut server = Server::bind(ServerConfig {
        limits: Limits {
            max_frame_bytes: 64,
            ..Limits::default()
        },
        ..ServerConfig::default()
    })
    .unwrap();
    assert!(
        server
            .publish_snapshot(Snapshot {
                tick: 1,
                entities: vec![EntitySnapshot::identity("long")]
            })
            .is_err()
    );
    server
        .publish_snapshot(Snapshot {
            tick: 1,
            entities: vec![],
        })
        .unwrap();
}
