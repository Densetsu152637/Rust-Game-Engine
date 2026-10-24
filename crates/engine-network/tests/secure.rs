use engine_network::*;
use rcgen::{CertifiedKey, KeyPair, generate_simple_self_signed};
use std::{
    io::{self, Read, Write},
    net::TcpStream,
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
}
impl Credentials {
    fn new() -> Self {
        Self {
            server: Identity::new("localhost"),
            client: Identity::new("player"),
        }
    }
    fn server_security(&self) -> ServerSecurity {
        let allowed = self.client.cert.clone();
        ServerSecurity::new(
            vec![self.server.cert.clone()],
            self.server.key.clone_key(),
            [self.client.cert.clone()],
            move |_, chain| chain.first() == Some(&allowed),
        )
        .unwrap()
    }
    fn client_security(&self) -> ClientSecurity {
        ClientSecurity::new(
            "localhost",
            [self.server.cert.clone()],
            vec![self.client.cert.clone()],
            self.client.key.clone_key(),
        )
        .unwrap()
    }
    fn server(&self) -> Server {
        Server::bind_secure(ServerConfig::default(), self.server_security()).unwrap()
    }
    fn client(&self, server: &Server) -> Client {
        Client::connect_secure(
            ClientConfig::new(server.local_addr().unwrap()),
            self.client_security(),
        )
        .unwrap()
    }
}
fn until(mut test: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !test() {
        assert!(Instant::now() < end, "TLS loopback timed out");
        thread::sleep(Duration::from_millis(1));
    }
}
fn connect(server: &mut Server, clients: &mut [&mut Client]) {
    until(|| {
        server.poll(PollBudget::default()).unwrap();
        for c in clients.iter_mut() {
            c.poll(PollBudget::default()).unwrap();
        }
        clients.iter().all(|c| c.is_connected())
    });
}
fn input(tick: u64, id: &str) -> InputCommand {
    InputCommand {
        tick,
        entity_id: id.into(),
        movement: [1.0, 0.0, 0.0],
        buttons: 0,
    }
}

#[test]
fn mutually_authenticated_clients_replicate_and_cleanup_ownership() {
    let credentials = Credentials::new();
    let mut server = credentials.server();
    let mut a = credentials.client(&server);
    let mut b = credentials.client(&server);
    assert!(a.send_input(input(1, "hero")).is_err());
    assert!(server.assign_owner("hero", 1).is_err());
    connect(&mut server, &mut [&mut a, &mut b]);
    assert_eq!(server.security(), TransportSecurity::MutualTls13);
    assert_eq!(a.security(), TransportSecurity::MutualTls13);
    assert_eq!(server.pending_peers(), 0);
    let peer = a.peer_id().unwrap();
    server.assign_owner("hero", peer).unwrap();
    a.send_input(input(1, "hero")).unwrap();
    b.send_input(input(1, "hero")).unwrap();
    let (mut accepted, mut rejected) = (0, 0);
    until(|| {
        a.poll(PollBudget::default()).unwrap();
        b.poll(PollBudget::default()).unwrap();
        for e in server.poll(PollBudget::default()).unwrap() {
            match e {
                ServerEvent::Input { .. } => accepted += 1,
                ServerEvent::InputRejected { .. } => rejected += 1,
                _ => {}
            }
        }
        accepted == 1 && rejected == 1
    });
    let snapshot = Snapshot {
        tick: 2,
        entities: vec![EntitySnapshot {
            id: "hero".into(),
            translation: [3.0, 0.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0; 3],
            velocity: [0.0; 3],
        }],
    };
    server.publish_snapshot(snapshot.clone()).unwrap();
    until(|| {
        server.poll(PollBudget::default()).unwrap();
        a.poll(PollBudget::default()).unwrap();
        b.poll(PollBudget::default()).unwrap();
        a.state().latest() == Some(&snapshot) && b.state().latest() == Some(&snapshot)
    });
    a.disconnect();
    until(|| {
        server
            .poll(PollBudget::default())
            .unwrap()
            .iter()
            .any(|e| matches!(e, ServerEvent::Disconnected { peer_id, .. } if *peer_id == peer))
    });
    assert!(server.assign_owner("hero", peer).is_err());
    b.send_input(input(3, "hero")).unwrap();
    until(|| {
        b.poll(PollBudget::default()).unwrap();
        server
            .poll(PollBudget::default())
            .unwrap()
            .iter()
            .any(|e| matches!(e, ServerEvent::InputRejected { .. }))
    });
}

#[test]
fn invalid_roots_hostname_client_credentials_and_policy_fail_closed() {
    for failure in 0..4 {
        let credentials = Credentials::new();
        let wrong = Identity::new("wrong");
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let security = ServerSecurity::new(
            vec![credentials.server.cert.clone()],
            credentials.server.key.clone_key(),
            [credentials.client.cert.clone()],
            move |_, _| {
                count.fetch_add(1, Ordering::SeqCst);
                failure != 3
            },
        )
        .unwrap();
        let mut server = Server::bind_secure(ServerConfig::default(), security).unwrap();
        let client_security = ClientSecurity::new(
            if failure == 1 {
                "wrong.example"
            } else {
                "localhost"
            },
            [if failure == 0 {
                wrong.cert.clone()
            } else {
                credentials.server.cert.clone()
            }],
            vec![if failure == 2 {
                wrong.cert.clone()
            } else {
                credentials.client.cert.clone()
            }],
            if failure == 2 {
                wrong.key.clone_key()
            } else {
                credentials.client.key.clone_key()
            },
        )
        .unwrap();
        let mut client = Client::connect_secure(
            ClientConfig::new(server.local_addr().unwrap()),
            client_security,
        )
        .unwrap();
        let mut rejected = false;
        until(|| {
            for e in server.poll(PollBudget::default()).unwrap() {
                assert!(!matches!(
                    e,
                    ServerEvent::Connected(_)
                        | ServerEvent::Authenticated(_)
                        | ServerEvent::Input { .. }
                ));
                rejected |= matches!(e, ServerEvent::Disconnected { .. });
            }
            for e in client.poll(PollBudget::default()).unwrap() {
                assert!(!matches!(
                    e,
                    ClientEvent::Connected(_) | ClientEvent::Authenticated(_)
                ));
            }
            rejected
        });
        assert_eq!(calls.load(Ordering::SeqCst), usize::from(failure == 3));
        assert_eq!(server.peer_ids().count(), 0);
    }
}

/// Raw rustls client bypasses the engine's local input validation, keeping adversarial tests honest.
struct Raw {
    tls: rustls::ClientConnection,
    socket: TcpStream,
}
impl Raw {
    fn new(credentials: &Credentials, server: &Server, certificate: bool) -> Self {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(credentials.server.cert.clone()).unwrap();
        let builder = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots);
        let mut config = if certificate {
            builder
                .with_client_auth_cert(
                    vec![credentials.client.cert.clone()],
                    credentials.client.key.clone_key(),
                )
                .unwrap()
        } else {
            builder.with_no_client_auth()
        };
        config.alpn_protocols = vec![b"rust-game-engine/1".to_vec()];
        let tls = rustls::ClientConnection::new(Arc::new(config), "localhost".try_into().unwrap())
            .unwrap();
        let socket = TcpStream::connect(server.local_addr().unwrap()).unwrap();
        socket.set_nonblocking(true).unwrap();
        Self { tls, socket }
    }
    fn poll(&mut self) {
        if let Err(e) = self.tls.complete_io(&mut self.socket) {
            assert!(matches!(
                e.kind(),
                io::ErrorKind::WouldBlock
                    | io::ErrorKind::InvalidData
                    | io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
            ));
        }
        let mut discard = [0; 4096];
        let _ = self.tls.reader().read(&mut discard);
    }
    fn send(&mut self, bytes: &[u8]) {
        self.tls.writer().write_all(bytes).unwrap();
    }
    fn json(&mut self, value: serde_json::Value) {
        let bytes = serde_json::to_vec(&value).unwrap();
        self.send(&(bytes.len() as u32).to_be_bytes());
        self.send(&bytes);
    }
}
fn raw_connect(server: &mut Server, raw: &mut Raw) -> PeerId {
    let mut id = None;
    until(|| {
        raw.poll();
        for e in server.poll(PollBudget::default()).unwrap() {
            if let ServerEvent::Connected(peer) = e {
                id = Some(peer);
            }
        }
        id.is_some()
    });
    id.unwrap()
}

#[test]
fn unauthenticated_plaintext_and_missing_client_certificate_never_join() {
    let credentials = Credentials::new();
    let mut server = credentials.server();
    let mut raw = Raw::new(&credentials, &server, false);
    until(|| {
        raw.poll();
        server
            .poll(PollBudget::default())
            .unwrap()
            .iter()
            .any(|e| matches!(e, ServerEvent::Disconnected { .. }))
    });
    assert_eq!(server.peer_ids().count(), 0);
    let mut plain = TcpStream::connect(server.local_addr().unwrap()).unwrap();
    plain
        .write_all(b"\0\0\0\x14{\"type\":\"Heartbeat\"}")
        .unwrap();
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
fn raw_tls_replay_foreign_owner_oversized_and_malformed_frames_fail() {
    let credentials = Credentials::new();
    let mut server = credentials.server();
    let mut raw = Raw::new(&credentials, &server, true);
    let peer = raw_connect(&mut server, &mut raw);
    server.assign_owner("hero", peer).unwrap();
    for (tick, id) in [(1, "hero"), (1, "hero"), (2, "foreign")] {
        raw.json(serde_json::json!({"type":"Input","tick":tick,"entity_id":id,"movement":[1,0,0],"buttons":0}));
    }
    let (mut accepted, mut rejected) = (0, 0);
    until(|| {
        raw.poll();
        for e in server.poll(PollBudget::default()).unwrap() {
            match e {
                ServerEvent::Input { .. } => accepted += 1,
                ServerEvent::InputRejected { .. } => rejected += 1,
                _ => {}
            }
        }
        accepted + rejected == 3
    });
    assert_eq!((accepted, rejected), (1, 2));
    raw.send(&u32::MAX.to_be_bytes());
    until(|| {
        raw.poll();
        server
            .poll(PollBudget::default())
            .unwrap()
            .iter()
            .any(|e| matches!(e, ServerEvent::Disconnected { .. }))
    });
    assert!(server.assign_owner("hero", peer).is_err());
    let mut raw = Raw::new(&credentials, &server, true);
    raw_connect(&mut server, &mut raw);
    raw.json(serde_json::json!({"type":"Input","tick":1,"entity_id":"hero","movement":[0,0,0],"buttons":0,"translation":[100,0,0]}));
    until(|| {
        raw.poll();
        server
            .poll(PollBudget::default())
            .unwrap()
            .iter()
            .any(|e| matches!(e, ServerEvent::Disconnected { .. }))
    });
}

#[test]
fn pending_connections_are_capped_and_expire_even_without_io_budget() {
    let credentials = Credentials::new();
    let mut security = credentials.server_security();
    security.limits.handshake_timeout = Duration::from_millis(20);
    let mut server = Server::bind_secure(
        ServerConfig {
            limits: Limits {
                max_peers: 1,
                ..Limits::default()
            },
            ..ServerConfig::default()
        },
        security,
    )
    .unwrap();
    let _idle = TcpStream::connect(server.local_addr().unwrap()).unwrap();
    server.poll(PollBudget::default()).unwrap();
    assert_eq!(server.pending_peers(), 1);
    assert_eq!(server.peer_ids().count(), 0);
    assert!(server.assign_owner("hero", 1).is_err());
    server
        .publish_snapshot(Snapshot {
            tick: 1,
            entities: vec![],
        })
        .unwrap();
    let _extra = TcpStream::connect(server.local_addr().unwrap()).unwrap();
    server.poll(PollBudget::default()).unwrap();
    assert_eq!(server.pending_peers(), 1);
    until(|| {
        server.poll(PollBudget { max_accepts:0,max_bytes:0,max_messages:0 }).unwrap().iter().any(|e| matches!(e, ServerEvent::Disconnected { reason, .. } if reason.contains("timeout")))
    });
    assert_eq!(server.pending_peers(), 0);
}

#[test]
fn authenticated_message_flood_is_disconnected() {
    let credentials = Credentials::new();
    let mut security = credentials.server_security();
    security.limits.max_messages_per_second = 3;
    let mut server = Server::bind_secure(ServerConfig::default(), security).unwrap();
    let mut raw = Raw::new(&credentials, &server, true);
    raw_connect(&mut server, &mut raw);
    for _ in 0..4 {
        raw.json(serde_json::json!({"type":"Heartbeat"}));
    }
    until(|| {
        raw.poll();
        server.poll(PollBudget::default()).unwrap().iter().any(
            |e| matches!(e, ServerEvent::Disconnected { reason, .. } if reason.contains("rate")),
        )
    });
}

#[test]
fn encrypted_records_hide_gameplay_and_record_replay_is_rejected() {
    let credentials = Credentials::new();
    let mut server = credentials.server();
    let mut raw = Raw::new(&credentials, &server, true);
    let id = raw_connect(&mut server, &mut raw);
    let marker = "sensitive-gameplay-marker";
    server.assign_owner(marker, id).unwrap();
    raw.json(serde_json::json!({"type":"Input","tick":1,"entity_id":marker,"movement":[0,0,0],"buttons":0}));
    let mut wire = Vec::new();
    while raw.tls.wants_write() {
        raw.tls.write_tls(&mut wire).unwrap();
    }
    assert!(!wire.windows(marker.len()).any(|w| w == marker.as_bytes()));
    raw.socket.write_all(&wire).unwrap();
    until(|| {
        server
            .poll(PollBudget::default())
            .unwrap()
            .iter()
            .any(|e| matches!(e, ServerEvent::Input { .. }))
    });
    raw.socket.write_all(&wire).unwrap();
    until(|| {
        let events = server.poll(PollBudget::default()).unwrap();
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, ServerEvent::Input { .. }))
        );
        events
            .iter()
            .any(|e| matches!(e, ServerEvent::Disconnected { .. }))
    });
}

#[test]
fn tiny_tls_budgets_progress_and_zero_budgets_do_not_authenticate() {
    let credentials = Credentials::new();
    let mut server = credentials.server();
    let mut client = credentials.client(&server);
    server.poll(PollBudget::default()).unwrap();
    let zero = PollBudget {
        max_bytes: 0,
        max_messages: 0,
        max_accepts: 0,
    };
    for _ in 0..4 {
        client.poll(zero).unwrap();
        server.poll(zero).unwrap();
    }
    assert!(!client.is_connected());
    assert_eq!(server.pending_peers(), 1);
    let tiny = PollBudget {
        max_bytes: 17,
        max_messages: 1,
        max_accepts: 1,
    };
    until(|| {
        client.poll(tiny).unwrap();
        server.poll(tiny).unwrap();
        client.is_connected()
    });
    assert_eq!(server.peer_ids().count(), 1);
}

#[test]
fn handshake_and_receive_byte_caps_reject_work_before_admission() {
    for handshake_cap in [true, false] {
        let credentials = Credentials::new();
        let mut security = credentials.server_security();
        if handshake_cap {
            security.limits.max_handshake_bytes = 1;
        } else {
            security.limits.max_received_bytes_per_second = 1;
        }
        let mut server = Server::bind_secure(ServerConfig::default(), security).unwrap();
        let mut client = credentials.client(&server);
        until(|| {
            client.poll(PollBudget::default()).unwrap();
            server
                .poll(PollBudget::default())
                .unwrap()
                .iter()
                .any(|e| matches!(e, ServerEvent::Disconnected { .. }))
        });
        assert_eq!(server.peer_ids().count(), 0);
    }
}

#[test]
fn snapshots_larger_than_tls_buffer_drain_without_losing_frames() {
    let credentials = Credentials::new();
    let mut server = credentials.server();
    let mut client = credentials.client(&server);
    connect(&mut server, &mut [&mut client]);
    let snapshot = Snapshot {
        tick: 1,
        entities: (0..700)
            .map(|i| EntitySnapshot {
                id: format!("replicated-entity-{i}"),
                translation: [1.0, 2.0, 3.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
                scale: [1.0; 3],
                velocity: [0.0; 3],
            })
            .collect(),
    };
    assert!(serde_json::to_vec(&snapshot).unwrap().len() > 64 * 1024);
    server.publish_snapshot(snapshot.clone()).unwrap();
    until(|| {
        server.poll(PollBudget::default()).unwrap();
        client.poll(PollBudget::default()).unwrap();
        client.state().latest() == Some(&snapshot)
    });
}
