//! Internal tests that inject raw QUIC frames a well-behaved engine peer never sends.
use super::*;
use bytes::Bytes;
use engine_network::{
    CertificateDer, Delivery, Lane, LinkEvent, MessageEndpoint, PeerId, PollBudget, PrivateKeyDer,
    SERVER_PEER, rustls,
};
use quinn_proto::Dir;
use rcgen::{CertifiedKey, KeyPair, generate_simple_self_signed};
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

fn identity(name: &str) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let CertifiedKey { cert, signing_key }: CertifiedKey<KeyPair> =
        generate_simple_self_signed(vec![name.into()]).unwrap();
    (
        cert.der().clone(),
        PrivateKeyDer::Pkcs8(signing_key.serialize_der().into()),
    )
}
fn until(mut test: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !test() {
        assert!(Instant::now() < end, "QUIC loopback timed out");
        thread::sleep(Duration::from_millis(1));
    }
}
struct Identities {
    server_cert: CertificateDer<'static>,
    client_cert: CertificateDer<'static>,
    client_key: PrivateKeyDer<'static>,
}
struct Pair {
    server: QuicServer,
    client: QuicClient,
    server_events: Vec<LinkEvent>,
    client_events: Vec<LinkEvent>,
}
impl Pair {
    /// Server trusting the generated client identity; `client` builds the client security.
    fn new(
        client: impl FnOnce(&Identities) -> QuicClientSecurity,
        calls: Arc<AtomicUsize>,
    ) -> Self {
        let (server_cert, server_key) = identity("localhost");
        let (client_cert, client_key) = identity("player");
        let ids = Identities {
            server_cert,
            client_cert,
            client_key,
        };
        let security = QuicServerSecurity::new(
            vec![ids.server_cert.clone()],
            server_key,
            [ids.client_cert.clone()],
            move |_, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                true
            },
        )
        .unwrap();
        let server = QuicServer::bind(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            QuicServerConfig::default(),
            security,
        )
        .unwrap();
        let client = QuicClient::connect(
            server.local_addr().unwrap(),
            QuicClientConfig::default(),
            client(&ids),
        )
        .unwrap();
        Self {
            server,
            client,
            server_events: Vec::new(),
            client_events: Vec::new(),
        }
    }
    fn trusted() -> Self {
        Self::new(
            |ids| {
                QuicClientSecurity::new(
                    "localhost",
                    [ids.server_cert.clone()],
                    vec![ids.client_cert.clone()],
                    ids.client_key.clone_key(),
                )
                .unwrap()
            },
            Arc::default(),
        )
    }
    fn poll(&mut self) {
        self.server_events
            .extend(self.server.poll(PollBudget::default()).unwrap());
        self.client_events
            .extend(self.client.poll(PollBudget::default()).unwrap());
    }
    fn connected_peer(&mut self) -> PeerId {
        until(|| {
            self.poll();
            self.client.is_connected() && !self.server.connected_peers().is_empty()
        });
        self.server.connected_peers()[0]
    }
    fn client_link(&mut self) -> &mut link::Link {
        self.client.link.as_mut().unwrap()
    }
    fn server_disconnect_reason(&mut self) -> String {
        let mut reason = None;
        until(|| {
            self.poll();
            reason = self.server_events.iter().find_map(|event| match event {
                LinkEvent::Disconnected { reason, .. } => Some(reason.clone()),
                _ => None,
            });
            reason.is_some()
        });
        reason.unwrap_or_default()
    }
}
fn messages(events: &[LinkEvent]) -> Vec<(Lane, Delivery, Vec<u8>)> {
    events
        .iter()
        .filter_map(|event| match event {
            LinkEvent::Message {
                lane,
                delivery,
                payload,
                ..
            } => Some((*lane, *delivery, payload.clone())),
            _ => None,
        })
        .collect()
}
fn write_stream(link: &mut link::Link, data: &[u8], finish: bool) {
    let stream = link.conn.streams().open(Dir::Uni).unwrap();
    let mut send = link.conn.send_stream(stream);
    assert_eq!(send.write(data).unwrap(), data.len());
    if finish {
        send.finish().unwrap();
    }
}
fn send_datagram(link: &mut link::Link, datagram: Vec<u8>) {
    link.conn
        .datagrams()
        .send(Bytes::from(datagram), false)
        .unwrap();
}

#[test]
fn client_without_certificate_never_connects_and_policy_never_runs() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut pair = Pair::new(
        |ids| {
            // Bypasses QuicClientSecurity's own "client certificate is required" check.
            let mut roots = rustls::RootCertStore::empty();
            roots.add(ids.server_cert.clone()).unwrap();
            let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
            config.alpn_protocols = vec![QUIC_ALPN.to_vec()];
            assert!(
                QuicClientSecurity::from_rustls_config(Arc::new(config.clone()), "localhost")
                    .is_err()
            );
            QuicClientSecurity {
                config: Arc::new(
                    quinn_proto::crypto::rustls::QuicClientConfig::try_from(Arc::new(config))
                        .unwrap(),
                ),
                server_name: "localhost".into(),
            }
        },
        calls.clone(),
    );
    until(|| {
        pair.poll();
        pair.client_events
            .iter()
            .any(|e| matches!(e, LinkEvent::Disconnected { .. }))
    });
    assert!(pair.server_events.is_empty(), "{:?}", pair.server_events);
    assert!(
        !pair
            .client_events
            .iter()
            .any(|e| matches!(e, LinkEvent::Connected(_) | LinkEvent::Message { .. }))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

type Injection = Box<dyn Fn(&mut link::Link)>;

#[test]
fn malformed_datagram_and_stream_headers_disconnect_without_delivery() {
    let max = QuicLimits::default().max_message_bytes as u32;
    let injections: Vec<(&str, Injection)> = vec![
        (
            "datagram kind",
            Box::new(|l| send_datagram(l, vec![0x7F, 1])),
        ),
        (
            "short sequenced",
            Box::new(|l| send_datagram(l, vec![0x11, 0])),
        ),
        ("empty datagram", Box::new(|l| send_datagram(l, Vec::new()))),
        ("stream kind", Box::new(|l| write_stream(l, &[9, 0], true))),
        (
            "control from client",
            Box::new(|l| write_stream(l, &[1, 1], true)),
        ),
        (
            "ordered lane",
            Box::new(|l| write_stream(l, &[2, 16], false)),
        ),
        (
            "oversized ordered length",
            Box::new(move |l| {
                let mut data = vec![2, 1];
                data.extend_from_slice(&(max + 1).to_be_bytes());
                write_stream(l, &data, false);
            }),
        ),
        (
            "second ordered stream for one lane",
            Box::new(|l| {
                write_stream(l, &[2, 1, 0, 0, 0, 0], false);
                write_stream(l, &[2, 1], false);
            }),
        ),
    ];
    for (name, inject) in injections {
        let mut pair = Pair::trusted();
        let peer = pair.connected_peer();
        inject(pair.client_link());
        let reason = pair.server_disconnect_reason();
        assert!(
            reason.contains("header")
                || reason.contains("max_message_bytes")
                || reason.contains("stream"),
            "{name}: {reason}"
        );
        // Only the empty message of the duplicate-lane case may precede the violation.
        assert!(
            messages(&pair.server_events).iter().all(|m| m.2.is_empty()),
            "{name}"
        );
        assert!(!pair.server.connected_peers().contains(&peer), "{name}");
        until(|| {
            pair.poll();
            pair.client_events.iter().any(|e| {
                matches!(e, LinkEvent::Disconnected { reason, .. } if reason == "protocol violation")
            })
        });
    }
}

#[test]
fn sequenced_datagrams_drop_stale_and_duplicate_payloads() {
    let mut pair = Pair::trusted();
    let peer = pair.connected_peer();
    let lane = Lane::INPUT.0;
    for (sequence, byte) in [
        (5_u16, b'a'),
        (3, b'b'),
        (5, b'x'),
        (6, b'c'),
        (0x8005, b'd'),
    ] {
        let [high, low] = sequence.to_be_bytes();
        send_datagram(pair.client_link(), vec![0x10 | lane, high, low, byte]);
    }
    until(|| {
        pair.poll();
        pair.server.stats(peer).unwrap().dropped_unreliable == 2
            && messages(&pair.server_events).len() == 3
    });
    let got: Vec<u8> = messages(&pair.server_events)
        .into_iter()
        .map(|(l, d, p)| {
            assert_eq!((l, d), (Lane::INPUT, Delivery::UnreliableSequenced));
            p[0]
        })
        .collect();
    // 3 and the duplicate 5 are stale; 0x8005 is newer than 6 under serial arithmetic.
    assert_eq!(got, b"acd");
}

#[test]
fn unknown_peers_invalid_lanes_and_pre_admission_sends_are_rejected() {
    let mut pair = Pair::trusted();
    assert!(
        pair.client
            .send(SERVER_PEER, Lane::RPC, Delivery::ReliableOrdered, b"early")
            .is_err()
    );
    assert_eq!(
        pair.client.max_payload(SERVER_PEER, Delivery::Unreliable),
        None
    );
    let peer = pair.connected_peer();
    assert!(
        pair.server
            .send(peer + 1, Lane::RPC, Delivery::Unreliable, b"x")
            .is_err()
    );
    assert!(
        pair.client
            .send(SERVER_PEER, Lane(16), Delivery::Unreliable, b"x")
            .is_err()
    );
    assert!(QuicLimits::default().validate().is_ok());
    for bad in [
        QuicLimits {
            keep_alive_interval: Some(Duration::from_secs(60)),
            ..QuicLimits::default()
        },
        QuicLimits {
            max_queued_bytes: 1024,
            ..QuicLimits::default()
        },
        QuicLimits {
            max_pending_handshakes: 0,
            ..QuicLimits::default()
        },
    ] {
        assert!(bad.validate().is_err());
    }
}
