//! `MessageEndpoint` over TCP (plaintext and mutual TLS), the caller-rustls-config
//! constructors, and `ConditionedEndpoint`. Real loopback sockets only.
use engine_network::*;
use rcgen::{CertifiedKey, KeyPair, generate_simple_self_signed};
use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

struct Identity {
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
}
impl Identity {
    fn new(name: &str) -> Self {
        let CertifiedKey { cert, signing_key }: CertifiedKey<KeyPair> =
            generate_simple_self_signed(vec![name.into()]).unwrap();
        Self {
            cert: cert.der().clone(),
            key: PrivateKeyDer::Pkcs8(signing_key.serialize_der().into()),
        }
    }
}
fn until(mut test: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !test() {
        assert!(Instant::now() < end, "loopback operation timed out");
        thread::sleep(Duration::from_millis(1));
    }
}
fn poll(endpoint: &mut impl MessageEndpoint) -> Vec<LinkEvent> {
    endpoint.poll(PollBudget::default()).unwrap()
}
/// Polls both endpoints until each reports `Connected`; returns the server's peer ID and the
/// lifecycle events each side saw.
fn connect(
    server: &mut impl MessageEndpoint,
    client: &mut impl MessageEndpoint,
) -> (PeerId, Vec<LinkEvent>, Vec<LinkEvent>) {
    let (mut server_events, mut client_events) = (Vec::new(), Vec::new());
    let mut peer = None;
    until(|| {
        for event in poll(server) {
            if let LinkEvent::Connected(id) = event {
                peer = Some(id);
            }
            server_events.push(event);
        }
        client_events.extend(poll(client));
        peer.is_some() && client.connected_peers() == [SERVER_PEER]
    });
    (peer.unwrap(), server_events, client_events)
}
fn payloads(events: Vec<LinkEvent>) -> Vec<(Lane, Delivery, Vec<u8>)> {
    events
        .into_iter()
        .filter_map(|event| match event {
            LinkEvent::Message {
                lane,
                delivery,
                payload,
                ..
            } => Some((lane, delivery, payload)),
            _ => None,
        })
        .collect()
}
const DELIVERIES: [Delivery; 4] = [
    Delivery::ReliableOrdered,
    Delivery::ReliableUnordered,
    Delivery::UnreliableSequenced,
    Delivery::Unreliable,
];
/// Sends `count` payloads cycling deliveries and lanes, and checks TCP delivers all in order.
fn round_trip(sender: &mut impl MessageEndpoint, receiver: &mut impl MessageEndpoint, to: PeerId) {
    let mut sent = Vec::new();
    for i in 0..24_u32 {
        let lane = [Lane::REPLICATION, Lane::RPC, Lane::VOICE, Lane(15)][i as usize % 4];
        let delivery = DELIVERIES[(i / 4) as usize % 4];
        let payload = i.to_be_bytes().repeat(1 + i as usize);
        sender.send(to, lane, delivery, &payload).unwrap();
        sent.push((lane, delivery, payload));
    }
    let mut received = Vec::new();
    until(|| {
        poll(sender);
        received.extend(payloads(poll(receiver)));
        received.len() >= sent.len()
    });
    assert_eq!(received, sent);
}
fn credentials() -> (Identity, Identity) {
    (Identity::new("localhost"), Identity::new("player"))
}
fn engine_server_security(server: &Identity, client: &Identity) -> ServerSecurity {
    let allowed = client.cert.clone();
    ServerSecurity::new(
        vec![server.cert.clone()],
        server.key.clone_key(),
        [client.cert.clone()],
        move |_, chain| chain.first() == Some(&allowed),
    )
    .unwrap()
}
fn engine_client_security(server: &Identity, client: &Identity) -> ClientSecurity {
    ClientSecurity::new(
        "localhost",
        [server.cert.clone()],
        vec![client.cert.clone()],
        client.key.clone_key(),
    )
    .unwrap()
}

#[test]
fn plaintext_trait_round_trips_every_delivery_in_order_both_ways() {
    let mut server = Server::bind(ServerConfig::default()).unwrap();
    let mut client = Client::connect(ClientConfig::new(server.local_addr().unwrap())).unwrap();
    assert_eq!(client.connected_peers(), Vec::<PeerId>::new());
    let (peer, server_events, _) = connect(&mut server, &mut client);
    assert!(!server_events.contains(&LinkEvent::Authenticated(peer)));
    assert_eq!(MessageEndpoint::transport(&server), TransportKind::Tcp);
    assert_eq!(
        MessageEndpoint::security(&client),
        TransportSecurity::TrustedLanPlaintext
    );
    assert_eq!(server.connected_peers(), vec![peer]);
    let max = Limits::default().max_frame_bytes - 3;
    for delivery in DELIVERIES {
        assert_eq!(server.max_payload(peer, delivery), Some(max));
        assert_eq!(client.max_payload(SERVER_PEER, delivery), Some(max));
    }
    assert_eq!(server.max_payload(peer + 1, Delivery::Unreliable), None);
    round_trip(&mut client, &mut server, SERVER_PEER);
    round_trip(&mut server, &mut client, peer);
    // Oversized, invalid lane and unknown peer are rejected without queueing.
    let big = vec![0; max + 1];
    for result in [
        client.send(SERVER_PEER, Lane::RPC, Delivery::ReliableOrdered, &big),
        client.send(SERVER_PEER, Lane(16), Delivery::Unreliable, b"x"),
        client.send(7, Lane::RPC, Delivery::Unreliable, b"x"),
        server.send(peer + 1, Lane::RPC, Delivery::Unreliable, b"x"),
    ] {
        assert!(matches!(result, Err(Error::Invalid(_))));
    }
    // The largest payload fits a frame exactly.
    client
        .send(
            SERVER_PEER,
            Lane::RPC,
            Delivery::ReliableOrdered,
            &vec![7; max],
        )
        .unwrap();
    let mut got = Vec::new();
    until(|| {
        poll(&mut client);
        got.extend(payloads(poll(&mut server)));
        !got.is_empty()
    });
    assert_eq!(got[0].2.len(), max);
    let stats = server.stats(peer).unwrap();
    assert!(stats.received_packets >= 25 && stats.sent_packets >= 25);
    assert!(stats.received_bytes > max as u64);
    assert_eq!(stats.lost_packets, 0);
    assert!(MessageEndpoint::disconnect(&mut client, SERVER_PEER));
    assert!(!MessageEndpoint::disconnect(&mut client, SERVER_PEER));
    until(|| {
        poll(&mut server)
            .iter()
            .any(|e| matches!(e, LinkEvent::Disconnected { peer: p, .. } if *p == peer))
    });
}

#[test]
fn tls_trait_round_trips_and_authentication_precedes_connected() {
    let (server_id, client_id) = credentials();
    let mut server = Server::bind_secure(
        ServerConfig::default(),
        engine_server_security(&server_id, &client_id),
    )
    .unwrap();
    let mut client = Client::connect_secure(
        ClientConfig::new(server.local_addr().unwrap()),
        engine_client_security(&server_id, &client_id),
    )
    .unwrap();
    let (peer, server_events, client_events) = connect(&mut server, &mut client);
    let position =
        |events: &[LinkEvent], event: LinkEvent| events.iter().position(|e| *e == event).unwrap();
    assert!(
        position(&server_events, LinkEvent::Authenticated(peer))
            < position(&server_events, LinkEvent::Connected(peer))
    );
    assert!(
        position(&client_events, LinkEvent::Authenticated(SERVER_PEER))
            < position(&client_events, LinkEvent::Connected(SERVER_PEER))
    );
    assert_eq!(
        MessageEndpoint::security(&server),
        TransportSecurity::MutualTls13
    );
    round_trip(&mut client, &mut server, SERVER_PEER);
    round_trip(&mut server, &mut client, peer);
}

#[test]
fn first_poll_latches_the_event_api_and_snapshots_still_apply_under_lanes() {
    let mut server = Server::bind(ServerConfig::default()).unwrap();
    let mut client = Client::connect(ClientConfig::new(server.local_addr().unwrap())).unwrap();
    let mut peer = None;
    until(|| {
        for event in server.poll(PollBudget::default()).unwrap() {
            if let ServerEvent::Connected(id) = event {
                peer = Some(id);
            }
        }
        poll(&mut client);
        peer.is_some() && client.is_connected()
    });
    // Each endpoint is now latched; the other poll fails closed without draining anything.
    assert!(matches!(
        MessageEndpoint::poll(&mut server, PollBudget::default()),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        client.poll(PollBudget::default()),
        Err(Error::Invalid(_))
    ));
    // Legacy input still works on the legacy-latched server.
    server.assign_owner("hero", peer.unwrap()).unwrap();
    client
        .send_input(InputCommand {
            tick: 1,
            entity_id: "hero".into(),
            movement: [0.0; 3],
            buttons: 0,
        })
        .unwrap();
    let mut inputs = 0;
    until(|| {
        poll(&mut client);
        for event in server.poll(PollBudget::default()).unwrap() {
            assert!(!matches!(event, ServerEvent::Disconnected { .. }));
            inputs += usize::from(matches!(event, ServerEvent::Input { .. }));
        }
        inputs == 1
    });
    // A lane-latched client applies snapshots to its state; no notification exists to retain.
    let snapshot = Snapshot {
        tick: 2,
        entities: vec![],
    };
    server.publish_snapshot(snapshot.clone()).unwrap();
    until(|| {
        server.poll(PollBudget::default()).unwrap();
        assert!(payloads(poll(&mut client)).is_empty());
        client.state().latest() == Some(&snapshot)
    });
    assert!(client.is_connected());
}

#[test]
fn full_queue_rejects_reliable_and_counts_unreliable() {
    let mut server = Server::bind(ServerConfig::default()).unwrap();
    let mut client = Client::connect(ClientConfig {
        limits: Limits {
            max_queued_frames: 2,
            ..Limits::default()
        },
        ..ClientConfig::new(server.local_addr().unwrap())
    })
    .unwrap();
    connect(&mut server, &mut client);
    for _ in 0..2 {
        client
            .send(SERVER_PEER, Lane::RPC, Delivery::ReliableOrdered, b"r")
            .unwrap();
    }
    assert!(matches!(
        client.send(SERVER_PEER, Lane::RPC, Delivery::ReliableUnordered, b"r"),
        Err(Error::QueueFull)
    ));
    client
        .send(SERVER_PEER, Lane::INPUT, Delivery::Unreliable, b"u")
        .unwrap();
    let stats = client.stats(SERVER_PEER).unwrap();
    assert_eq!(stats.dropped_unreliable, 1);
    assert!(stats.queued_bytes >= 2 * 8);
}

/// Limits for the slot-pinning tests: one peer slot and a short idle deadline.
fn one_slot_limits() -> Limits {
    Limits {
        max_peers: 1,
        idle_timeout: Duration::from_millis(400),
        ..Limits::default()
    }
}
/// Polls `server` through one API until it reports the peer's disconnect, failing after
/// `idle_timeout` plus slack. Returns the reason.
fn await_drop(mut poll_server: impl FnMut() -> Option<String>) -> String {
    let deadline = Instant::now() + one_slot_limits().idle_timeout + Duration::from_secs(2);
    loop {
        if let Some(reason) = poll_server() {
            return reason;
        }
        assert!(
            Instant::now() < deadline,
            "peer kept its slot past idle_timeout"
        );
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn legacy_polled_server_drops_a_data_flooding_peer_and_reuses_its_slot() {
    let limits = one_slot_limits();
    let mut server = Server::bind(ServerConfig {
        limits: limits.clone(),
        ..ServerConfig::default()
    })
    .unwrap();
    let addr = server.local_addr().unwrap();
    let mut client = Client::connect(ClientConfig::new(addr)).unwrap();
    let mut connected = false;
    until(|| {
        connected |= server
            .poll(PollBudget::default())
            .unwrap()
            .iter()
            .any(|e| matches!(e, ServerEvent::Connected(_)));
        poll(&mut client);
        connected && client.is_connected()
    });
    // 33 data frames (one more than the old retention cap) that the legacy API cannot
    // represent, then the client goes idle.
    let mut sent = 0;
    until(|| {
        while sent < 33
            && client
                .send(SERVER_PEER, Lane::RPC, Delivery::ReliableOrdered, b"d")
                .is_ok()
        {
            sent += 1;
        }
        poll(&mut client);
        sent == 33
    });
    for _ in 0..20 {
        poll(&mut client);
    }
    let reason = await_drop(|| {
        server
            .poll(PollBudget::default())
            .unwrap()
            .into_iter()
            .find_map(|e| match e {
                ServerEvent::Disconnected { reason, .. } => Some(reason),
                _ => None,
            })
    });
    assert_eq!(server.peer_ids().count(), 0, "dropped peer: {reason}");
    // The single slot is free again.
    let mut second = Client::connect(ClientConfig::new(addr)).unwrap();
    let mut reconnected = false;
    until(|| {
        reconnected |= server
            .poll(PollBudget::default())
            .unwrap()
            .iter()
            .any(|e| matches!(e, ServerEvent::Connected(_)));
        second.poll(PollBudget::default()).unwrap();
        reconnected && second.is_connected()
    });
}

#[test]
fn trait_polled_server_drops_an_input_flooding_peer_and_reuses_its_slot() {
    let limits = one_slot_limits();
    let mut server = Server::bind(ServerConfig {
        limits: limits.clone(),
        ..ServerConfig::default()
    })
    .unwrap();
    let addr = server.local_addr().unwrap();
    let mut client = Client::connect(ClientConfig::new(addr)).unwrap();
    let mut connected = false;
    until(|| {
        connected |= poll(&mut server)
            .iter()
            .any(|e| matches!(e, LinkEvent::Connected(_)));
        client.poll(PollBudget::default()).unwrap();
        connected && client.is_connected()
    });
    let mut tick = 0;
    until(|| {
        while tick < 33
            && client
                .send_input(InputCommand {
                    tick: tick + 1,
                    entity_id: "hero".into(),
                    movement: [0.0; 3],
                    buttons: 0,
                })
                .is_ok()
        {
            tick += 1;
        }
        let _ = client.poll(PollBudget::default());
        tick == 33
    });
    for _ in 0..20 {
        let _ = client.poll(PollBudget::default());
    }
    let reason = await_drop(|| {
        poll(&mut server).into_iter().find_map(|e| match e {
            LinkEvent::Disconnected { reason, .. } => Some(reason),
            _ => None,
        })
    });
    assert!(
        server.connected_peers().is_empty(),
        "dropped peer: {reason}"
    );
    let mut second = Client::connect(ClientConfig::new(addr)).unwrap();
    connect(&mut server, &mut second);
}

#[test]
fn legacy_polled_client_drops_a_data_flooding_server() {
    let mut server = Server::bind(ServerConfig::default()).unwrap();
    let mut client = Client::connect(ClientConfig {
        limits: one_slot_limits(),
        ..ClientConfig::new(server.local_addr().unwrap())
    })
    .unwrap();
    let mut peer = None;
    until(|| {
        for event in poll(&mut server) {
            if let LinkEvent::Connected(id) = event {
                peer = Some(id);
            }
        }
        client.poll(PollBudget::default()).unwrap();
        peer.is_some() && client.is_connected()
    });
    let peer = peer.unwrap();
    let mut sent = 0;
    let reason = await_drop(|| {
        while sent < 33
            && server
                .send(peer, Lane::RPC, Delivery::ReliableOrdered, b"d")
                .is_ok()
        {
            sent += 1;
        }
        poll(&mut server);
        client
            .poll(PollBudget::default())
            .unwrap()
            .into_iter()
            .find_map(|e| match e {
                ClientEvent::Disconnected(reason) => Some(reason),
                _ => None,
            })
    });
    assert!(!client.is_connected(), "dropped server: {reason}");
}

fn caller_server_config(
    server: &Identity,
    client_roots: Option<&Identity>,
) -> rustls::ServerConfig {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap();
    let builder = match client_roots {
        Some(client) => {
            let mut roots = rustls::RootCertStore::empty();
            roots.add(client.cert.clone()).unwrap();
            builder.with_client_cert_verifier(
                rustls::server::WebPkiClientVerifier::builder_with_provider(
                    Arc::new(roots),
                    provider,
                )
                .build()
                .unwrap(),
            )
        }
        None => builder.with_no_client_auth(),
    };
    let mut config = builder
        .with_single_cert(vec![server.cert.clone()], server.key.clone_key())
        .unwrap();
    // Unsafe choices the endpoint must override.
    config.alpn_protocols.clear();
    config.max_early_data_size = 1 << 16;
    config.send_tls13_tickets = 2;
    config
}

#[test]
fn caller_rustls_configs_interoperate_after_alpn_and_early_data_are_corrected() {
    let (server_id, client_id) = credentials();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let security = ServerSecurity::from_rustls_config(
        Arc::new(caller_server_config(&server_id, Some(&client_id))),
        move |_, chain| {
            count.fetch_add(1, Ordering::SeqCst);
            !chain.is_empty()
        },
    )
    .unwrap();
    let mut server = Server::bind_secure(ServerConfig::default(), security).unwrap();
    // Caller client config: no ALPN, early data requested, resumption left enabled.
    let mut roots = rustls::RootCertStore::empty();
    roots.add(server_id.cert.clone()).unwrap();
    let mut client_config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_root_certificates(roots)
    .with_client_auth_cert(vec![client_id.cert.clone()], client_id.key.clone_key())
    .unwrap();
    client_config.enable_early_data = true;
    let client_security =
        ClientSecurity::from_rustls_config(Arc::new(client_config), "localhost").unwrap();
    for _ in 0..2 {
        let mut client = Client::connect_secure(
            ClientConfig::new(server.local_addr().unwrap()),
            client_security.clone(),
        )
        .unwrap();
        let (peer, _, _) = connect(&mut server, &mut client);
        client
            .send(
                SERVER_PEER,
                Lane::SESSION,
                Delivery::ReliableOrdered,
                b"hello",
            )
            .unwrap();
        let mut got = Vec::new();
        until(|| {
            poll(&mut client);
            got.extend(payloads(poll(&mut server)));
            !got.is_empty()
        });
        assert_eq!(got[0].2, b"hello");
        MessageEndpoint::disconnect(&mut server, peer);
    }
    // Policy ran exactly once per connection: no ticket/resumption skipped it.
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn caller_server_config_without_client_auth_fails_closed() {
    let (server_id, client_id) = credentials();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let security = ServerSecurity::from_rustls_config(
        Arc::new(caller_server_config(&server_id, None)),
        move |_, _| {
            count.fetch_add(1, Ordering::SeqCst);
            true
        },
    )
    .unwrap();
    let mut server = Server::bind_secure(ServerConfig::default(), security).unwrap();
    let mut client = Client::connect_secure(
        ClientConfig::new(server.local_addr().unwrap()),
        engine_client_security(&server_id, &client_id),
    )
    .unwrap();
    let mut rejected = false;
    until(|| {
        for event in server.poll(PollBudget::default()).unwrap() {
            assert!(!matches!(
                event,
                ServerEvent::Connected(_) | ServerEvent::Authenticated(_)
            ));
            rejected |= matches!(&event, ServerEvent::Disconnected { reason, .. } if reason.contains("identity"));
        }
        for event in MessageEndpoint::poll(&mut client, PollBudget::default()).unwrap() {
            assert!(!matches!(
                event,
                LinkEvent::Connected(_) | LinkEvent::Message { .. }
            ));
        }
        rejected
    });
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn conditioned_endpoint_delays_everything_and_drops_only_unreliable() {
    let server = Server::bind(ServerConfig::default()).unwrap();
    let mut client = Client::connect(ClientConfig::new(server.local_addr().unwrap())).unwrap();
    let mut server = ConditionedEndpoint::new(
        server,
        NetworkConditions {
            loss: 0.5,
            duplicate: 0.25,
            latency_ms: 40,
            jitter_ms: 20,
            reorder: 0.2,
            seed: 7,
            ..NetworkConditions::default()
        },
    )
    .unwrap();
    let (peer, _, _) = connect(&mut server, &mut client);
    const N: u32 = 200;
    let started = Instant::now();
    let mut sent = 0;
    let mut first_arrival = None;
    let mut received = Vec::new();
    until(|| {
        // Pace sends under the TCP queue cap; the server never sees backpressure.
        for _ in 0..4 {
            if sent < N {
                for (lane, delivery) in [
                    (Lane::RPC, Delivery::ReliableOrdered),
                    (Lane::REPLICATION, Delivery::ReliableUnordered),
                    (Lane::INPUT, Delivery::UnreliableSequenced),
                    (Lane::VOICE, Delivery::Unreliable),
                ] {
                    client
                        .send(SERVER_PEER, lane, delivery, &sent.to_be_bytes())
                        .unwrap();
                }
                sent += 1;
            }
        }
        poll(&mut client);
        let batch = payloads(poll(&mut server));
        if first_arrival.is_none() && !batch.is_empty() {
            first_arrival = Some(started.elapsed());
        }
        received.extend(batch);
        let reliable = received.iter().filter(|m| m.1.is_reliable()).count();
        sent == N && reliable == 2 * N as usize && server.held_events() == 0
    });
    assert!(first_arrival.unwrap() >= Duration::from_millis(40));
    let of = |delivery| -> Vec<u32> {
        received
            .iter()
            .filter(|m| m.1 == delivery)
            .map(|m| u32::from_be_bytes(m.2.clone().try_into().unwrap()))
            .collect()
    };
    assert_eq!(of(Delivery::ReliableOrdered), (0..N).collect::<Vec<_>>());
    let mut unordered = of(Delivery::ReliableUnordered);
    unordered.sort_unstable();
    assert_eq!(unordered, (0..N).collect::<Vec<_>>());
    let sequenced = of(Delivery::UnreliableSequenced);
    assert!(
        sequenced.windows(2).all(|w| w[0] < w[1]),
        "stale sequenced payload delivered"
    );
    let unreliable = of(Delivery::Unreliable);
    let unique: BTreeSet<_> = unreliable.iter().copied().collect();
    // 50% loss: expect about 100 of 200 distinct payloads; duplicates of some survivors.
    assert!(
        (60..=140).contains(&unique.len()),
        "unique unreliable {}",
        unique.len()
    );
    assert!(unreliable.len() > unique.len(), "no duplicates delivered");
    assert!(sequenced.len() < N as usize);
    assert!(server.stats(peer).unwrap().dropped_unreliable > 0);
}
