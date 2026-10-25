//! QUIC endpoints over real loopback UDP sockets (127.0.0.1:0) with synthetic in-memory
//! identities. Measured numbers are printed; run with `--nocapture` to see them.
use engine_net_quic::*;
use engine_network::{
    CertificateDer, Delivery, Error, Lane, LinkEvent, MessageEndpoint, NetworkConditions, PeerId,
    PollBudget, PrivateKeyDer, SERVER_PEER, TransportKind, TransportSecurity, rustls,
};
use rcgen::{CertifiedKey, KeyPair, generate_simple_self_signed};
use std::{
    collections::BTreeSet,
    net::{SocketAddr, UdpSocket},
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
struct Credentials {
    server: Identity,
    client: Identity,
    calls: Arc<AtomicUsize>,
}
impl Credentials {
    fn new() -> Self {
        Self {
            server: Identity::new("localhost"),
            client: Identity::new("player"),
            calls: Arc::default(),
        }
    }
    fn server_security(&self, allow: bool) -> QuicServerSecurity {
        let allowed = self.client.cert.clone();
        let calls = self.calls.clone();
        QuicServerSecurity::new(
            vec![self.server.cert.clone()],
            self.server.key.clone_key(),
            [self.client.cert.clone()],
            move |_, chain| {
                calls.fetch_add(1, Ordering::SeqCst);
                allow && chain.first() == Some(&allowed)
            },
        )
        .unwrap()
    }
    fn client_security(&self) -> QuicClientSecurity {
        QuicClientSecurity::new(
            "localhost",
            [self.server.cert.clone()],
            vec![self.client.cert.clone()],
            self.client.key.clone_key(),
        )
        .unwrap()
    }
    fn server(&self, config: QuicServerConfig) -> QuicServer {
        QuicServer::bind(loopback(), config, self.server_security(true)).unwrap()
    }
    fn client(&self, server: &QuicServer, config: QuicClientConfig) -> QuicClient {
        QuicClient::connect(server.local_addr().unwrap(), config, self.client_security()).unwrap()
    }
}
fn loopback() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 0))
}
fn udp() -> UdpSocket {
    let socket = UdpSocket::bind(loopback()).unwrap();
    socket.set_nonblocking(true).unwrap();
    socket
}
fn until_within(limit: Duration, mut test: impl FnMut() -> bool) {
    let end = Instant::now() + limit;
    while !test() {
        assert!(Instant::now() < end, "QUIC loopback timed out");
        thread::sleep(Duration::from_millis(1));
    }
}
fn until(test: impl FnMut() -> bool) {
    until_within(Duration::from_secs(15), test);
}
fn poll(endpoint: &mut impl MessageEndpoint) -> Vec<LinkEvent> {
    endpoint.poll(PollBudget::default()).unwrap()
}
/// Polls until both sides are connected; returns (peer, server events, client events).
fn connect(
    server: &mut impl MessageEndpoint,
    client: &mut impl MessageEndpoint,
) -> (PeerId, Vec<LinkEvent>, Vec<LinkEvent>) {
    let (mut server_events, mut client_events) = (Vec::new(), Vec::new());
    until(|| {
        server_events.extend(poll(server));
        client_events.extend(poll(client));
        client.connected_peers() == [SERVER_PEER] && !server.connected_peers().is_empty()
    });
    (server.connected_peers()[0], server_events, client_events)
}
fn messages(events: Vec<LinkEvent>) -> Vec<(Lane, Delivery, Vec<u8>)> {
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
fn index(payload: &[u8]) -> u32 {
    u32::from_be_bytes(payload[..4].try_into().unwrap())
}
fn fast_limits() -> QuicLimits {
    QuicLimits {
        max_received_bytes_per_second: 1 << 30,
        max_messages_per_second: 1 << 20,
        ..QuicLimits::default()
    }
}

#[test]
fn mutual_auth_succeeds_and_every_delivery_round_trips() {
    let credentials = Credentials::new();
    let mut server = credentials.server(QuicServerConfig::default());
    let mut client = credentials.client(&server, QuicClientConfig::default());
    let (peer, server_events, client_events) = connect(&mut server, &mut client);
    for (events, id) in [(&server_events, peer), (&client_events, SERVER_PEER)] {
        let at = |e: LinkEvent| events.iter().position(|x| *x == e).unwrap();
        assert!(at(LinkEvent::Authenticated(id)) < at(LinkEvent::Connected(id)));
    }
    assert_eq!(credentials.calls.load(Ordering::SeqCst), 1);
    assert_eq!(server.transport(), TransportKind::Quic);
    assert_eq!(client.security(), TransportSecurity::MutualTls13);
    let limits = QuicLimits::default();
    assert_eq!(
        server.max_payload(peer, Delivery::ReliableOrdered),
        Some(limits.max_message_bytes)
    );
    let datagram_max = client
        .max_payload(SERVER_PEER, Delivery::Unreliable)
        .unwrap();
    assert!(datagram_max >= 1000, "datagram limit {datagram_max}");
    assert_eq!(
        client.max_payload(SERVER_PEER, Delivery::UnreliableSequenced),
        Some(datagram_max - 2)
    );
    // Oversize payloads are rejected for both reliable and datagram deliveries.
    for (delivery, size) in [
        (Delivery::ReliableOrdered, limits.max_message_bytes + 1),
        (Delivery::Unreliable, datagram_max + 1),
        (Delivery::UnreliableSequenced, datagram_max - 1),
    ] {
        assert!(matches!(
            client.send(SERVER_PEER, Lane::RPC, delivery, &vec![0; size]),
            Err(Error::Invalid(_))
        ));
    }
    for (sender_is_client, to) in [(true, SERVER_PEER), (false, peer)] {
        let (sender, receiver): (&mut dyn MessageEndpoint, &mut dyn MessageEndpoint) =
            if sender_is_client {
                (&mut client, &mut server)
            } else {
                (&mut server, &mut client)
            };
        let lanes = [Lane::REPLICATION, Lane::RPC, Lane(15)];
        let deliveries = [
            Delivery::ReliableOrdered,
            Delivery::ReliableUnordered,
            Delivery::UnreliableSequenced,
            Delivery::Unreliable,
        ];
        for i in 0..30_u32 {
            for delivery in deliveries {
                let mut payload = i.to_be_bytes().to_vec();
                payload.resize(4 + (i as usize * 37) % 900, i as u8);
                sender
                    .send(to, lanes[i as usize % 3], delivery, &payload)
                    .unwrap();
            }
        }
        let max_unreliable = vec![9; datagram_max];
        sender
            .send(to, Lane::VOICE, Delivery::Unreliable, &max_unreliable)
            .unwrap();
        let mut received = Vec::new();
        until(|| {
            sender.poll(PollBudget::default()).unwrap();
            received.extend(messages(receiver.poll(PollBudget::default()).unwrap()));
            received.iter().filter(|m| m.1.is_reliable()).count() == 60
                && received.iter().any(|m| m.0 == Lane::VOICE)
        });
        for lane in lanes {
            let of = |delivery| -> Vec<u32> {
                received
                    .iter()
                    .filter(|m| m.0 == lane && m.1 == delivery)
                    .map(|m| index(&m.2))
                    .collect()
            };
            let expected: Vec<u32> = (0..30).filter(|i| lanes[*i as usize % 3] == lane).collect();
            assert_eq!(
                of(Delivery::ReliableOrdered),
                expected,
                "ordered lane {lane:?}"
            );
            let mut unordered = of(Delivery::ReliableUnordered);
            unordered.sort_unstable();
            assert_eq!(unordered, expected);
            let sequenced = of(Delivery::UnreliableSequenced);
            assert!(sequenced.windows(2).all(|w| w[0] < w[1]));
            assert!(
                sequenced.len() * 10 >= expected.len() * 9,
                "loopback sequenced loss"
            );
            assert!(of(Delivery::Unreliable).len() * 10 >= expected.len() * 9);
        }
        for message in &received {
            if message.0 != Lane::VOICE {
                let i = index(&message.2);
                assert_eq!(message.2.len(), 4 + (i as usize * 37) % 900);
            }
        }
        assert!(received.iter().any(|m| m.2 == max_unreliable));
    }
    let stats = client.stats(SERVER_PEER).unwrap();
    assert!(
        stats.rtt_ms > 0.0 && stats.rtt_ms < 100.0,
        "rtt {}",
        stats.rtt_ms
    );
    assert!(stats.sent_bytes > 0 && stats.received_packets > 0);
    assert!(stats.congestion_window_bytes > 0);
    assert!(client.disconnect(SERVER_PEER));
    let mut reason = String::new();
    until(|| {
        poll(&mut client);
        poll(&mut server).into_iter().any(|e| match e {
            LinkEvent::Disconnected { peer: p, reason: r } if p == peer => {
                reason = r;
                true
            }
            _ => false,
        })
    });
    assert_eq!(reason, "closed by peer");
}

#[test]
fn wrong_roots_name_client_identity_and_denied_policy_fail_closed() {
    for case in ["roots", "name", "client identity", "policy"] {
        let credentials = Credentials::new();
        let stranger = Identity::new("localhost");
        let mut server = QuicServer::bind(
            loopback(),
            QuicServerConfig::default(),
            credentials.server_security(case != "policy"),
        )
        .unwrap();
        let client_identity = if case == "client identity" {
            &stranger
        } else {
            &credentials.client
        };
        let security = QuicClientSecurity::new(
            if case == "name" {
                "wrong.example"
            } else {
                "localhost"
            },
            [if case == "roots" {
                stranger.cert.clone()
            } else {
                credentials.server.cert.clone()
            }],
            vec![client_identity.cert.clone()],
            client_identity.key.clone_key(),
        )
        .unwrap();
        let mut client = QuicClient::connect(
            server.local_addr().unwrap(),
            QuicClientConfig::default(),
            security,
        )
        .unwrap();
        let mut client_reason = None;
        until(|| {
            let server_events = poll(&mut server);
            assert!(
                server_events.is_empty(),
                "{case}: server emitted {server_events:?}"
            );
            for event in poll(&mut client) {
                match event {
                    LinkEvent::Disconnected { reason, .. } => client_reason = Some(reason),
                    other => panic!("{case}: client emitted {other:?}"),
                }
            }
            client_reason.is_some() && server.pending_peers() == 0
        });
        assert!(server.connected_peers().is_empty() && client.connected_peers().is_empty());
        assert_eq!(
            credentials.calls.load(Ordering::SeqCst),
            usize::from(case == "policy"),
            "{case}"
        );
        println!("{case}: client saw {:?}", client_reason.unwrap());
    }
}

#[test]
fn peer_cap_pending_cap_and_handshake_timeout_refuse_extra_connections() {
    let credentials = Credentials::new();
    let config = QuicServerConfig {
        limits: QuicLimits {
            max_peers: 2,
            max_pending_handshakes: 1,
            handshake_timeout: Duration::from_millis(400),
            ..QuicLimits::default()
        },
        ..QuicServerConfig::default()
    };
    let mut server = credentials.server(config);
    // One stalled handshake occupies the only pending slot.
    let mut stalled = credentials.client(&server, QuicClientConfig::default());
    // Stop right after the server accepts, before the client sees the server's flight.
    until(|| {
        poll(&mut stalled);
        poll(&mut server);
        server.pending_peers() == 1
    });
    let stalled_at = Instant::now();
    let refused = |server: &mut QuicServer| {
        let mut extra = credentials.client(server, QuicClientConfig::default());
        let mut reason = None;
        until(|| {
            let events = poll(server);
            assert!(events.is_empty(), "{events:?}");
            for event in poll(&mut extra) {
                match event {
                    LinkEvent::Disconnected { reason: r, .. } => reason = Some(r),
                    other => panic!("refused client emitted {other:?}"),
                }
            }
            reason.is_some()
        });
        reason.unwrap()
    };
    assert_eq!(refused(&mut server), "connection refused");
    // The server expires the stalled handshake on its own deadline, with no I/O from it.
    until(|| {
        assert!(poll(&mut server).is_empty());
        server.pending_peers() == 0
    });
    assert!(stalled_at.elapsed() >= Duration::from_millis(300));
    // The stalled client learns of the server's close (or hits its own deadline), never Connected.
    let mut stalled_reason = None;
    until(|| {
        poll(&mut server);
        for event in poll(&mut stalled) {
            match event {
                LinkEvent::Disconnected { reason, .. } => stalled_reason = Some(reason),
                other => panic!("stalled client emitted {other:?}"),
            }
        }
        stalled_reason.is_some()
    });
    let stalled_reason = stalled_reason.unwrap();
    assert!(
        ["closed by peer", "handshake timeout"].contains(&stalled_reason.as_str()),
        "{stalled_reason}"
    );
    // Two admitted peers fill max_peers; a third is refused.
    let mut a = credentials.client(&server, QuicClientConfig::default());
    connect(&mut server, &mut a);
    let mut b = credentials.client(&server, QuicClientConfig::default());
    until(|| {
        poll(&mut server);
        poll(&mut a);
        poll(&mut b);
        b.connected_peers() == [SERVER_PEER] && server.connected_peers().len() == 2
    });
    let mut c = credentials.client(&server, QuicClientConfig::default());
    let mut reason = None;
    until(|| {
        poll(&mut server);
        poll(&mut a);
        poll(&mut b);
        for event in poll(&mut c) {
            match event {
                LinkEvent::Disconnected { reason: r, .. } => reason = Some(r),
                other => panic!("third client emitted {other:?}"),
            }
        }
        reason.is_some()
    });
    assert_eq!(reason.unwrap(), "connection refused");
    assert_eq!(server.connected_peers().len(), 2);
}

#[test]
fn idle_timeout_closes_silent_peers_and_keepalive_preserves_quiet_ones() {
    let credentials = Credentials::new();
    let limits = QuicLimits {
        idle_timeout: Duration::from_millis(500),
        keep_alive_interval: Some(Duration::from_millis(100)),
        ..QuicLimits::default()
    };
    let mut server = credentials.server(QuicServerConfig {
        limits: limits.clone(),
        ..QuicServerConfig::default()
    });
    let mut client = credentials.client(
        &server,
        QuicClientConfig {
            limits,
            ..QuicClientConfig::default()
        },
    );
    let (peer, _, _) = connect(&mut server, &mut client);
    // No application traffic for three idle periods: keep-alives hold the link open.
    let quiet = Instant::now();
    while quiet.elapsed() < Duration::from_millis(1500) {
        assert!(poll(&mut server).is_empty());
        assert!(poll(&mut client).is_empty());
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(server.connected_peers(), vec![peer]);
    // The client stops polling entirely; the server times it out.
    let silent = Instant::now();
    let mut reason = None;
    until(|| {
        for event in poll(&mut server) {
            if let LinkEvent::Disconnected { reason: r, .. } = event {
                reason = Some(r);
            }
        }
        reason.is_some()
    });
    let waited = silent.elapsed();
    assert_eq!(reason.unwrap(), "idle timeout");
    assert!(
        waited >= Duration::from_millis(400) && waited < Duration::from_secs(3),
        "{waited:?}"
    );
}

#[test]
fn full_queues_backpressure_reliable_without_loss_and_count_unreliable_drops() {
    let credentials = Credentials::new();
    let mut server = credentials.server(QuicServerConfig {
        limits: fast_limits(),
        ..QuicServerConfig::default()
    });
    let mut client = credentials.client(
        &server,
        QuicClientConfig {
            limits: fast_limits(),
            ..QuicClientConfig::default()
        },
    );
    connect(&mut server, &mut client);
    // Without polling nothing leaves the queue, so it fills to max_queued_bytes.
    let payload = vec![1; 16 * 1024];
    let mut queued = 0_u32;
    let full = loop {
        let mut message = queued.to_be_bytes().to_vec();
        message.extend_from_slice(&payload);
        match client.send(SERVER_PEER, Lane::RPC, Delivery::ReliableOrdered, &message) {
            Ok(()) => queued += 1,
            Err(error) => break error,
        }
        assert!(queued < 1000);
    };
    assert!(matches!(full, Error::QueueFull));
    let stats = client.stats(SERVER_PEER).unwrap();
    assert!(stats.queued_bytes as usize <= fast_limits().max_queued_bytes);
    assert!(queued >= 50, "queued {queued}");
    // Datagrams beyond the DATAGRAM send buffer are discarded and counted, never an error.
    for _ in 0..200 {
        client
            .send(SERVER_PEER, Lane::VOICE, Delivery::Unreliable, &[3; 1000])
            .unwrap();
    }
    let dropped = client.stats(SERVER_PEER).unwrap().dropped_unreliable;
    assert!(dropped > 0, "no datagram drops counted");
    let mut ordered = Vec::new();
    until(|| {
        poll(&mut client);
        for message in messages(poll(&mut server)) {
            if message.1 == Delivery::ReliableOrdered {
                ordered.push(index(&message.2));
            }
        }
        ordered.len() == queued as usize
    });
    assert_eq!(ordered, (0..queued).collect::<Vec<_>>());
    until(|| {
        poll(&mut client);
        poll(&mut server);
        client.stats(SERVER_PEER).unwrap().queued_bytes == 0
    });
    println!(
        "backpressure: {queued} x 16 KiB queued before QueueFull, {dropped} datagrams dropped locally"
    );
}

#[test]
fn garbage_and_malformed_udp_never_panics_or_admits() {
    let credentials = Credentials::new();
    let mut server = credentials.server(QuicServerConfig::default());
    let target = server.local_addr().unwrap();
    let attacker = UdpSocket::bind(loopback()).unwrap();
    let mut state = 0x2545_F491_4F6C_DD1D_u64;
    let mut random = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for round in 0..400 {
        let len = 1 + (random() % 1400) as usize;
        let mut datagram: Vec<u8> = (0..len).map(|_| random() as u8).collect();
        match round % 4 {
            // Long-header Initial for QUIC v1 with garbage connection IDs and payload.
            0 => {
                datagram.resize(1200, 0);
                datagram[..5].copy_from_slice(&[0xC3, 0, 0, 0, 1]);
            }
            // Short header with a random connection ID.
            1 => datagram[0] = 0x40 | (datagram[0] & 0x3F),
            // Version negotiation / unknown version.
            2 if len > 5 => datagram[1..5].copy_from_slice(&[0xFF, 0, 0, 0x1D]),
            _ => {}
        }
        attacker.send_to(&datagram, target).unwrap();
        if round % 20 == 0 {
            assert!(poll(&mut server).is_empty());
        }
    }
    for _ in 0..20 {
        assert!(poll(&mut server).is_empty());
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(server.pending_peers(), 0);
    assert!(server.connected_peers().is_empty());
    // A genuine client still connects afterwards.
    let mut client = credentials.client(&server, QuicClientConfig::default());
    connect(&mut server, &mut client);
}

#[test]
fn caller_rustls_configs_are_corrected_and_interoperate() {
    let credentials = Credentials::new();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut client_roots = rustls::RootCertStore::empty();
    client_roots.add(credentials.client.cert.clone()).unwrap();
    let mut server_config = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_client_cert_verifier(
            rustls::server::WebPkiClientVerifier::builder_with_provider(
                Arc::new(client_roots),
                provider.clone(),
            )
            .build()
            .unwrap(),
        )
        .with_single_cert(
            vec![credentials.server.cert.clone()],
            credentials.server.key.clone_key(),
        )
        .unwrap();
    // 0-RTT enabled and ALPN omitted: the constructor must correct both.
    server_config.max_early_data_size = u32::MAX;
    server_config.alpn_protocols.clear();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let security = QuicServerSecurity::from_rustls_config(Arc::new(server_config), move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        true
    })
    .unwrap();
    let mut server = QuicServer::bind(loopback(), QuicServerConfig::default(), security).unwrap();
    let mut server_roots = rustls::RootCertStore::empty();
    server_roots.add(credentials.server.cert.clone()).unwrap();
    let mut client_config = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(server_roots)
        .with_client_auth_cert(
            vec![credentials.client.cert.clone()],
            credentials.client.key.clone_key(),
        )
        .unwrap();
    client_config.enable_early_data = true;
    client_config.alpn_protocols = vec![b"h3".to_vec()];
    let client_security =
        QuicClientSecurity::from_rustls_config(Arc::new(client_config), "localhost").unwrap();
    for _ in 0..2 {
        let mut client = QuicClient::connect(
            server.local_addr().unwrap(),
            QuicClientConfig::default(),
            client_security.clone(),
        )
        .unwrap();
        let (peer, _, _) = connect(&mut server, &mut client);
        client
            .send(SERVER_PEER, Lane::SESSION, Delivery::ReliableOrdered, b"hi")
            .unwrap();
        let mut got = Vec::new();
        until(|| {
            poll(&mut client);
            got.extend(messages(poll(&mut server)));
            !got.is_empty()
        });
        assert_eq!(got[0].2, b"hi");
        server.disconnect(peer);
    }
    // No resumption or 0-RTT path skipped native policy.
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

/// Measures RTT, sending one reliable message every 5 ms for `duration`.
fn measured_rtt(server: &mut QuicServer, client: &mut QuicClient, duration: Duration) -> f32 {
    let start = Instant::now();
    let mut last = start;
    while start.elapsed() < duration {
        if last.elapsed() >= Duration::from_millis(5) {
            client
                .send(SERVER_PEER, Lane::RPC, Delivery::ReliableOrdered, b"ping")
                .unwrap();
            last = Instant::now();
        }
        poll(server);
        poll(client);
        thread::sleep(Duration::from_millis(1));
    }
    client.stats(SERVER_PEER).unwrap().rtt_ms
}

#[test]
fn configured_latency_raises_measured_rtt_by_the_configured_amount() {
    let credentials = Credentials::new();
    let mut server = credentials.server(QuicServerConfig::default());
    let mut client = credentials.client(&server, QuicClientConfig::default());
    connect(&mut server, &mut client);
    let baseline = measured_rtt(&mut server, &mut client, Duration::from_millis(1500));

    let latency_ms = 60;
    let mut server = credentials.server(QuicServerConfig::default());
    let socket = ConditionedSocket::new(
        udp(),
        NetworkConditions {
            latency_ms,
            seed: 1,
            ..NetworkConditions::default()
        },
    )
    .unwrap();
    let mut client = QuicClient::with_socket(
        socket,
        server.local_addr().unwrap(),
        QuicClientConfig::default(),
        credentials.client_security(),
    )
    .unwrap();
    connect(&mut server, &mut client);
    let conditioned = measured_rtt(&mut server, &mut client, Duration::from_millis(3000));
    let added = conditioned - baseline;
    println!(
        "RTT: loopback baseline {baseline:.2} ms, with {latency_ms} ms one-way latency {conditioned:.2} ms (added {added:.2} ms)"
    );
    assert!(
        (added - latency_ms as f32).abs() <= 15.0,
        "added {added} ms"
    );
}

#[test]
fn lossy_link_keeps_reliable_ordered_complete_and_drops_about_the_configured_fraction() {
    let credentials = Credentials::new();
    let lossy = |seed| {
        ConditionedSocket::new(
            udp(),
            NetworkConditions {
                loss: 0.2,
                seed,
                ..NetworkConditions::default()
            },
        )
        .unwrap()
    };
    let mut server = QuicServer::with_socket(
        lossy(11),
        QuicServerConfig::default(),
        credentials.server_security(true),
    )
    .unwrap();
    let mut client = QuicClient::with_socket(
        lossy(29),
        server.local_addr().unwrap(),
        QuicClientConfig::default(),
        credentials.client_security(),
    )
    .unwrap();
    until_within(Duration::from_secs(30), || {
        poll(&mut server);
        poll(&mut client);
        client.is_connected() && !server.connected_peers().is_empty()
    });
    let peer = server.connected_peers()[0];
    const RELIABLE: u32 = 300;
    const UNRELIABLE: u32 = 1000;
    let (mut ordered, mut unreliable) = (Vec::new(), BTreeSet::new());
    let mut sent = 0;
    until_within(Duration::from_secs(60), || {
        if sent < UNRELIABLE {
            client
                .send(
                    SERVER_PEER,
                    Lane::VOICE,
                    Delivery::Unreliable,
                    &sent.to_be_bytes(),
                )
                .unwrap();
            if sent < RELIABLE {
                client
                    .send(
                        SERVER_PEER,
                        Lane::RPC,
                        Delivery::ReliableOrdered,
                        &sent.to_be_bytes(),
                    )
                    .unwrap();
            }
            sent += 1;
        }
        poll(&mut client);
        for message in messages(poll(&mut server)) {
            match message.1 {
                Delivery::ReliableOrdered => ordered.push(index(&message.2)),
                _ => {
                    unreliable.insert(index(&message.2));
                }
            }
        }
        sent == UNRELIABLE && ordered.len() == RELIABLE as usize
    });
    // Late datagrams still in flight get a short grace period.
    let grace = Instant::now();
    while grace.elapsed() < Duration::from_millis(200) {
        poll(&mut client);
        for message in messages(poll(&mut server)) {
            if message.1 == Delivery::Unreliable {
                unreliable.insert(index(&message.2));
            }
        }
    }
    assert_eq!(ordered, (0..RELIABLE).collect::<Vec<_>>());
    let local_drops = client.stats(SERVER_PEER).unwrap().dropped_unreliable;
    let offered = u64::from(UNRELIABLE) - local_drops;
    let lost = 1.0 - unreliable.len() as f64 / offered as f64;
    let server_stats = server.stats(peer).unwrap();
    let client_stats = client.stats(SERVER_PEER).unwrap();
    println!(
        "20% loss: reliable-ordered {}/{RELIABLE} in order; unreliable delivered {}/{offered} (lost {:.1}%, {local_drops} dropped locally); QUIC declared {} client / {} server packets lost; client RTT {:.2} ms",
        ordered.len(),
        unreliable.len(),
        lost * 100.0,
        client_stats.lost_packets,
        server_stats.lost_packets,
        client_stats.rtt_ms,
    );
    assert!((0.12..=0.28).contains(&lost), "unreliable loss {lost}");
    assert!(client_stats.lost_packets > 0);
}

#[test]
fn loopback_throughput_measurement() {
    let credentials = Credentials::new();
    let limits = QuicLimits {
        max_queued_bytes: 4 * 1024 * 1024,
        ..fast_limits()
    };
    let mut server = credentials.server(QuicServerConfig {
        limits: limits.clone(),
        ..QuicServerConfig::default()
    });
    let mut client = credentials.client(
        &server,
        QuicClientConfig {
            limits,
            ..QuicClientConfig::default()
        },
    );
    connect(&mut server, &mut client);
    const MESSAGES: usize = 256;
    let payload = vec![0x5A; 32 * 1024];
    let start = Instant::now();
    let (mut sent, mut received, mut bytes) = (0, 0, 0);
    until_within(Duration::from_secs(60), || {
        while sent < MESSAGES
            && client
                .send(
                    SERVER_PEER,
                    Lane::REPLICATION,
                    Delivery::ReliableOrdered,
                    &payload,
                )
                .is_ok()
        {
            sent += 1;
        }
        poll(&mut client);
        for message in messages(
            server
                .poll(PollBudget {
                    max_messages: 1024,
                    max_bytes: 8 * 1024 * 1024,
                    max_accepts: 1,
                })
                .unwrap(),
        ) {
            received += 1;
            bytes += message.2.len();
        }
        received == MESSAGES
    });
    let elapsed = start.elapsed().as_secs_f64();
    let rate = bytes as f64 / elapsed / (1024.0 * 1024.0);
    let rtt = client.stats(SERVER_PEER).unwrap().rtt_ms;
    println!(
        "throughput: {} MiB reliable-ordered in {elapsed:.3} s = {rate:.1} MiB/s (debug build), RTT {rtt:.2} ms",
        bytes / (1024 * 1024)
    );
    assert_eq!(bytes, MESSAGES * payload.len());
}
