//! Steam Networking Sockets transport over the fake in-process socket pair.

mod common;

use engine_network::contract::{self, ContractFixture};
use engine_network::{
    Delivery, Error, Lane, LinkEvent, MessageEndpoint, PollBudget, SERVER_PEER, TransportKind,
    TransportSecurity,
};
use engine_steam::*;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Shares one `FakeBackend` between the session and the test.
struct Shared {
    fake: Arc<Mutex<FakeBackend>>,
}

impl SteamBackend for Shared {
    fn status(&self) -> SteamStatus {
        self.fake.lock().unwrap().status()
    }
    fn auth_session_ticket(&mut self) -> SteamResult<AuthTicket> {
        self.fake.lock().unwrap().auth_session_ticket()
    }
    fn cancel_auth_ticket(&mut self, handle: u32) -> SteamResult<()> {
        self.fake.lock().unwrap().cancel_auth_ticket(handle)
    }
    common::forward_net!();
    common::forward_input!();
    common::forward_ugc!();
}

fn me() -> SteamId {
    SteamId(76_561_198_000_000_001)
}

fn session_for(fake: FakeBackend) -> (SteamSession, Arc<Mutex<FakeBackend>>) {
    let fake = Arc::new(Mutex::new(fake));
    let session = SteamSession::new(Box::new(Shared { fake: fake.clone() }));
    (session, fake)
}

fn ready() -> FakeBackend {
    FakeBackend::ready(AppId(480), "tester")
}

struct Fixture {
    server: SteamServerEndpoint,
    client: SteamClientEndpoint,
    fake: Arc<Mutex<FakeBackend>>,
}

impl ContractFixture for Fixture {
    fn server(&mut self) -> &mut dyn MessageEndpoint {
        &mut self.server
    }
    fn client(&mut self) -> &mut dyn MessageEndpoint {
        &mut self.client
    }
    fn reorder_unreliable(&mut self, enabled: bool) -> bool {
        self.fake.lock().unwrap().set_net_reorder(enabled);
        true
    }
}

fn fixture(config: SteamServerConfig, ip: bool) -> Fixture {
    let (session, fake) = session_for(ready());
    let server = SteamServerEndpoint::listen(session.clone(), config).unwrap();
    let client = if ip {
        let addr = "127.0.0.1:27015".parse().unwrap();
        SteamClientEndpoint::connect_ip(session, addr, SteamClientConfig::default()).unwrap()
    } else {
        SteamClientEndpoint::connect_p2p(session, me(), 0, SteamClientConfig::default()).unwrap()
    };
    Fixture {
        server,
        client,
        fake,
    }
}

#[test]
fn contract_p2p_with_tickets() {
    contract::run_all(|| fixture(SteamServerConfig::p2p(0), false));
}

#[test]
fn contract_p2p_identity_only() {
    contract::run_all(|| {
        fixture(
            SteamServerConfig::p2p(0).with_auth(AuthPolicy::IdentityOnly),
            false,
        )
    });
}

#[test]
fn contract_ip_with_tickets() {
    contract::run_all(|| {
        fixture(
            SteamServerConfig::ip("127.0.0.1:27015".parse().unwrap()),
            true,
        )
    });
}

fn full() -> PollBudget {
    PollBudget::default()
}

fn pump(fx: &mut Fixture, rounds: usize) -> (Vec<LinkEvent>, Vec<LinkEvent>) {
    let (mut s, mut c) = (Vec::new(), Vec::new());
    for _ in 0..rounds {
        s.extend(fx.server.poll(full()).unwrap());
        c.extend(fx.client.poll(full()).unwrap());
    }
    (s, c)
}

#[test]
fn reports_transport_and_security() {
    let fx = fixture(SteamServerConfig::p2p(0), false);
    assert_eq!(fx.server.transport(), TransportKind::Steam);
    assert_eq!(fx.client.security(), TransportSecurity::PlatformEncrypted);
}

#[test]
fn authenticated_peers_carry_their_steam_id() {
    let mut fx = fixture(SteamServerConfig::p2p(0), false);
    let (server_events, client_events) = pump(&mut fx, 6);
    let peer = fx.server.connected_peers()[0];
    assert!(server_events.contains(&LinkEvent::Authenticated(peer)));
    assert!(client_events.contains(&LinkEvent::Authenticated(SERVER_PEER)));
    assert_eq!(fx.server.peer_steam_id(peer), Some(me()));
    assert_eq!(fx.client.server_steam_id(), Some(me()));
    assert_eq!(fx.server.peer_steam_id(999), None);
    let calls = fx.fake.lock().unwrap().call_count("net_begin_auth");
    assert_eq!(calls, 1, "the server validated the client's ticket");
}

#[test]
fn identity_only_skips_the_ticket_check() {
    let mut fx = fixture(
        SteamServerConfig::p2p(0).with_auth(AuthPolicy::IdentityOnly),
        false,
    );
    pump(&mut fx, 6);
    assert_eq!(fx.server.connected_peers().len(), 1);
    assert_eq!(fx.fake.lock().unwrap().call_count("net_begin_auth"), 0);
}

#[test]
fn identity_only_is_refused_for_ip_listeners() {
    let (session, _) = session_for(ready());
    let config = SteamServerConfig::ip("127.0.0.1:27016".parse().unwrap())
        .with_auth(AuthPolicy::IdentityOnly);
    assert!(matches!(
        SteamServerEndpoint::listen(session, config),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn unavailable_steam_is_an_error_not_a_panic() {
    let session = SteamSession::new(Box::new(NullBackend::not_compiled()));
    assert!(SteamServerEndpoint::listen(session.clone(), SteamServerConfig::p2p(0)).is_err());
    assert!(
        SteamClientEndpoint::connect_p2p(session, me(), 0, SteamClientConfig::default()).is_err()
    );
}

#[test]
fn rejected_ticket_keeps_the_client_out() {
    let (session, fake) = session_for(ready().with_auth_rejection(true));
    let mut server =
        SteamServerEndpoint::listen(session.clone(), SteamServerConfig::p2p(0)).unwrap();
    let mut client =
        SteamClientEndpoint::connect_p2p(session, me(), 0, SteamClientConfig::default()).unwrap();
    let (mut s, mut c) = (Vec::new(), Vec::new());
    for _ in 0..8 {
        s.extend(server.poll(full()).unwrap());
        c.extend(client.poll(full()).unwrap());
    }
    assert!(server.connected_peers().is_empty());
    assert!(
        !s.iter()
            .any(|e| matches!(e, LinkEvent::Authenticated(_) | LinkEvent::Connected(_)))
    );
    assert!(c.iter().any(|e| matches!(
        e,
        LinkEvent::Disconnected { reason, .. } if reason.contains("authentication failed")
    )));
    assert_eq!(fake.lock().unwrap().net_open_connections(), 0);
    assert_eq!(fake.lock().unwrap().net_auth_sessions(), 0);
}

#[test]
fn revoked_auth_disconnects_an_admitted_peer() {
    let mut fx = fixture(SteamServerConfig::p2p(0), false);
    pump(&mut fx, 6);
    let peer = fx.server.connected_peers()[0];
    fx.fake.lock().unwrap().revoke_net_auth(me());
    let (s, c) = pump(&mut fx, 4);
    assert!(s.iter().any(|e| matches!(
        e,
        LinkEvent::Disconnected { peer: p, reason } if *p == peer && reason.contains("revoked")
    )));
    assert!(
        c.iter()
            .any(|e| matches!(e, LinkEvent::Disconnected { .. }))
    );
}

#[test]
fn relay_readiness_is_explicit() {
    let (session, _) = session_for(ready().with_relay_delay(5));
    let mut server =
        SteamServerEndpoint::listen(session.clone(), SteamServerConfig::p2p(0)).unwrap();
    let mut client =
        SteamClientEndpoint::connect_p2p(session, me(), 0, SteamClientConfig::default()).unwrap();
    assert!(client.waiting_for_relay());
    assert_eq!(client.relay_status(), RelayStatus::Pending);
    assert!(client.connected_peers().is_empty());
    for _ in 0..40 {
        server.poll(full()).unwrap();
        client.poll(full()).unwrap();
    }
    assert!(!client.waiting_for_relay());
    assert_eq!(client.relay_status(), RelayStatus::Ready);
    assert_eq!(client.connected_peers(), vec![SERVER_PEER]);
}

#[test]
fn relay_failure_is_reported_as_disconnected() {
    let (session, _) = session_for(ready().with_relay_failure("no route to Steam"));
    let mut client =
        SteamClientEndpoint::connect_p2p(session, me(), 0, SteamClientConfig::default()).unwrap();
    let events = client.poll(full()).unwrap();
    assert!(matches!(
        events.as_slice(),
        [LinkEvent::Disconnected { peer: SERVER_PEER, reason }] if reason.contains("no route to Steam")
    ));
    assert!(!client.waiting_for_relay());
}

#[test]
fn relay_timeout_gives_up() {
    let (session, _) = session_for(ready().with_relay_delay(u32::MAX));
    let config = SteamClientConfig {
        limits: SteamLimits {
            relay_timeout: Duration::ZERO,
            ..SteamLimits::default()
        },
        ..SteamClientConfig::default()
    };
    let mut client = SteamClientEndpoint::connect_p2p(session, me(), 0, config).unwrap();
    let events = client.poll(full()).unwrap();
    assert!(matches!(
        events.as_slice(),
        [LinkEvent::Disconnected { reason, .. }] if reason.contains("relay")
    ));
}

#[test]
fn dialing_a_missing_listener_fails_cleanly() {
    let (session, fake) = session_for(ready());
    let mut client =
        SteamClientEndpoint::connect_p2p(session, me(), 7, SteamClientConfig::default()).unwrap();
    let events = client.poll(full()).unwrap();
    assert!(matches!(
        events.as_slice(),
        [LinkEvent::Disconnected {
            peer: SERVER_PEER,
            ..
        }]
    ));
    assert_eq!(fake.lock().unwrap().net_open_connections(), 0);
}

#[test]
fn unreliable_drops_under_pressure_are_counted_not_errors() {
    let (session, _) = session_for(ready().with_net_queue_limit(4096));
    let mut server =
        SteamServerEndpoint::listen(session.clone(), SteamServerConfig::p2p(0)).unwrap();
    let mut client =
        SteamClientEndpoint::connect_p2p(session, me(), 0, SteamClientConfig::default()).unwrap();
    for _ in 0..8 {
        server.poll(full()).unwrap();
        client.poll(full()).unwrap();
    }
    let payload = [1u8; 1000];
    for _ in 0..10 {
        client
            .send(SERVER_PEER, Lane::VOICE, Delivery::Unreliable, &payload)
            .unwrap();
    }
    let stats = client.stats(SERVER_PEER).unwrap();
    assert!(stats.dropped_unreliable > 0);
    assert!(stats.sent_packets > 0 && stats.sent_packets < 10);
    assert!(stats.queued_bytes > 0);
    assert!(matches!(
        client.send(SERVER_PEER, Lane::RPC, Delivery::ReliableOrdered, &payload),
        Err(Error::QueueFull)
    ));
}

#[test]
fn max_payload_follows_the_steam_limit() {
    let (session, _) = session_for(ready().with_net_max_message(1000));
    let mut server =
        SteamServerEndpoint::listen(session.clone(), SteamServerConfig::p2p(0)).unwrap();
    let mut client =
        SteamClientEndpoint::connect_p2p(session, me(), 0, SteamClientConfig::default()).unwrap();
    for _ in 0..8 {
        server.poll(full()).unwrap();
        client.poll(full()).unwrap();
    }
    assert_eq!(
        client.max_payload(SERVER_PEER, Delivery::ReliableOrdered),
        Some(999)
    );
    assert_eq!(
        client.max_payload(SERVER_PEER, Delivery::UnreliableSequenced),
        Some(995)
    );
}

#[test]
fn dropping_endpoints_releases_everything() {
    let mut fx = fixture(SteamServerConfig::p2p(0), false);
    pump(&mut fx, 6);
    let fake = fx.fake.clone();
    assert!(fake.lock().unwrap().net_open_connections() > 0);
    assert_eq!(fake.lock().unwrap().net_auth_sessions(), 1);
    drop(fx);
    assert_eq!(fake.lock().unwrap().net_open_connections(), 0);
    assert_eq!(fake.lock().unwrap().net_auth_sessions(), 0);
}

#[test]
fn a_second_login_with_the_same_steam_id_is_refused() {
    let (session, _) = session_for(ready());
    let mut server =
        SteamServerEndpoint::listen(session.clone(), SteamServerConfig::p2p(0)).unwrap();
    let mut first =
        SteamClientEndpoint::connect_p2p(session.clone(), me(), 0, SteamClientConfig::default())
            .unwrap();
    for _ in 0..8 {
        server.poll(full()).unwrap();
        first.poll(full()).unwrap();
    }
    let mut second =
        SteamClientEndpoint::connect_p2p(session, me(), 0, SteamClientConfig::default()).unwrap();
    let mut events = Vec::new();
    for _ in 0..8 {
        server.poll(full()).unwrap();
        first.poll(full()).unwrap();
        events.extend(second.poll(full()).unwrap());
    }
    assert_eq!(server.connected_peers().len(), 1);
    assert!(second.connected_peers().is_empty());
    assert!(events.iter().any(|e| matches!(
        e,
        LinkEvent::Disconnected { reason, .. } if reason.contains("already connected")
    )));
}

#[test]
fn max_peers_caps_pending_and_admitted_connections() {
    let (session, _) = session_for(ready());
    let limits = SteamLimits {
        max_peers: 1,
        ..SteamLimits::default()
    };
    let mut server = SteamServerEndpoint::listen(
        session.clone(),
        SteamServerConfig::p2p(0).with_limits(limits),
    )
    .unwrap();
    let mut a =
        SteamClientEndpoint::connect_p2p(session.clone(), me(), 0, SteamClientConfig::default())
            .unwrap();
    let mut b =
        SteamClientEndpoint::connect_p2p(session, me(), 0, SteamClientConfig::default()).unwrap();
    let mut b_events = Vec::new();
    for _ in 0..8 {
        server.poll(full()).unwrap();
        a.poll(full()).unwrap();
        b_events.extend(b.poll(full()).unwrap());
    }
    assert_eq!(a.connected_peers().len(), 1);
    assert!(b.connected_peers().is_empty());
    assert!(
        b_events.iter().any(
            |e| matches!(e, LinkEvent::Disconnected { reason, .. } if reason.contains("full"))
        )
    );
}

#[test]
fn max_accepts_bounds_connection_work_per_poll() {
    let (session, _) = session_for(ready());
    let mut server =
        SteamServerEndpoint::listen(session.clone(), SteamServerConfig::p2p(0)).unwrap();
    let _clients: Vec<_> = (0..3)
        .map(|_| {
            SteamClientEndpoint::connect_p2p(session.clone(), me(), 0, SteamClientConfig::default())
                .unwrap()
        })
        .collect();
    let one = PollBudget {
        max_accepts: 1,
        ..PollBudget::default()
    };
    server.poll(one).unwrap();
    assert_eq!(server.pending_peers(), 1);
    server.poll(one).unwrap();
    assert_eq!(server.pending_peers(), 2);
    let none = PollBudget {
        max_accepts: 0,
        ..PollBudget::default()
    };
    server.poll(none).unwrap();
    assert_eq!(server.pending_peers(), 2);
}

/// A hand-driven connection that speaks raw frames at the server through the backend.
struct Raw {
    fake: Arc<Mutex<FakeBackend>>,
    conn: NetConnId,
}

impl Raw {
    fn open(
        session: &SteamSession,
        fake: &Arc<Mutex<FakeBackend>>,
        server: &mut SteamServerEndpoint,
    ) -> Self {
        let _ = session;
        let conn = fake.lock().unwrap().net_connect_p2p(me(), 0).unwrap();
        server.poll(full()).unwrap();
        server.poll(full()).unwrap();
        Raw {
            fake: fake.clone(),
            conn,
        }
    }
    fn send(&self, frame: &[u8]) {
        let flags = NetSendFlags {
            reliable: true,
            no_nagle: true,
        };
        self.fake
            .lock()
            .unwrap()
            .net_send(self.conn, frame, flags)
            .unwrap();
    }
}

fn hostile_server() -> (SteamServerEndpoint, SteamSession, Arc<Mutex<FakeBackend>>) {
    let (session, fake) = session_for(ready());
    let server = SteamServerEndpoint::listen(session.clone(), SteamServerConfig::p2p(0)).unwrap();
    (server, session, fake)
}

#[test]
fn malformed_and_premature_frames_close_the_peer() {
    let hello = |user: u64, ticket: &[u8]| {
        let mut f = vec![0x80, 1];
        f.extend_from_slice(&user.to_le_bytes());
        f.extend_from_slice(ticket);
        f
    };
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", vec![]),
        ("data before hello", vec![0x01, 1, 2, 3]),
        ("reserved bits", vec![0x40]),
        ("truncated hello", vec![0x80, 1, 2]),
        ("bad version", vec![0x80, 9, 0, 0, 0, 0, 0, 0, 0, 0]),
        ("wrong claimed id", hello(me().0 + 1, &1u32.to_le_bytes())),
        ("garbage ticket", hello(me().0, b"nope")),
        (
            "unknown ticket handle",
            hello(me().0, &999u32.to_le_bytes()),
        ),
        ("oversize ticket", hello(me().0, &vec![0; 1025])),
        (
            "accept from a client",
            vec![0x81, 1, 0, 0, 0, 0, 0, 0, 0, 0],
        ),
    ];
    for (name, frame) in cases {
        let (mut server, session, fake) = hostile_server();
        let raw = Raw::open(&session, &fake, &mut server);
        assert_eq!(server.pending_peers(), 1, "{name}: connection pending");
        raw.send(&frame);
        server.poll(full()).unwrap();
        assert_eq!(server.pending_peers(), 0, "{name}: peer dropped");
        assert!(server.connected_peers().is_empty(), "{name}");
        assert_eq!(fake.lock().unwrap().net_auth_sessions(), 0, "{name}");
    }
}

#[test]
fn data_after_admission_with_a_second_hello_is_a_violation() {
    let mut fx = fixture(SteamServerConfig::p2p(0), false);
    pump(&mut fx, 6);
    let mut hello = vec![0x80, 1];
    hello.extend_from_slice(&me().0.to_le_bytes());
    let flags = NetSendFlags {
        reliable: true,
        no_nagle: true,
    };
    // Ids: 1 is the listen socket, 2 the client's connection, 3 the server's.
    fx.fake
        .lock()
        .unwrap()
        .net_send(NetConnId(2), &hello, flags)
        .unwrap();
    let (s, _) = pump(&mut fx, 2);
    assert!(
        s.iter()
            .any(|e| matches!(e, LinkEvent::Disconnected { .. }))
    );
    assert!(fx.server.connected_peers().is_empty());
}

#[test]
fn silent_connections_time_out() {
    let (session, fake) = session_for(ready());
    let limits = SteamLimits {
        handshake_timeout: Duration::ZERO,
        ..SteamLimits::default()
    };
    let mut server = SteamServerEndpoint::listen(
        session.clone(),
        SteamServerConfig::p2p(0).with_limits(limits),
    )
    .unwrap();
    let _conn = fake.lock().unwrap().net_connect_p2p(me(), 0).unwrap();
    server.poll(full()).unwrap();
    server.poll(full()).unwrap();
    assert_eq!(server.pending_peers(), 0);
    let _ = session;
}

#[test]
fn a_stalled_handshake_with_no_ticket_times_out_on_the_client() {
    // A client whose Steam cannot issue a ticket never sends Hello and gives up.
    struct NoTicket(Shared);
    impl SteamBackend for NoTicket {
        fn status(&self) -> SteamStatus {
            self.0.status()
        }
        fn auth_session_ticket(&mut self) -> SteamResult<AuthTicket> {
            Err(SteamError::Rejected("no ticket yet".into()))
        }
        fn net_init_relay(&mut self) -> SteamResult<()> {
            self.0.net_init_relay()
        }
        fn net_relay_status(&self) -> SteamResult<RelayStatus> {
            self.0.net_relay_status()
        }
        fn net_max_message_size(&self) -> SteamResult<usize> {
            self.0.net_max_message_size()
        }
        fn net_listen_p2p(&mut self, port: i32) -> SteamResult<NetListenId> {
            self.0.net_listen_p2p(port)
        }
        fn net_connect_p2p(&mut self, peer: SteamId, port: i32) -> SteamResult<NetConnId> {
            self.0.net_connect_p2p(peer, port)
        }
        fn net_accept(&mut self, conn: NetConnId) -> SteamResult<()> {
            self.0.net_accept(conn)
        }
        fn net_close(&mut self, conn: NetConnId, reason: &str, linger: bool) -> SteamResult<()> {
            self.0.net_close(conn, reason, linger)
        }
        fn net_poll_events(&mut self) -> Vec<NetEvent> {
            self.0.net_poll_events()
        }
    }
    let fake = Arc::new(Mutex::new(ready()));
    let session = SteamSession::new(Box::new(NoTicket(Shared { fake: fake.clone() })));
    let mut server =
        SteamServerEndpoint::listen(session.clone(), SteamServerConfig::p2p(0)).unwrap();
    let config = SteamClientConfig {
        limits: SteamLimits {
            handshake_timeout: Duration::from_millis(30),
            ..SteamLimits::default()
        },
        ..SteamClientConfig::default()
    };
    let mut client = SteamClientEndpoint::connect_p2p(session, me(), 0, config).unwrap();
    let mut events = Vec::new();
    for _ in 0..40 {
        server.poll(full()).unwrap();
        events.extend(client.poll(full()).unwrap());
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(events.iter().any(|e| matches!(
        e,
        LinkEvent::Disconnected { reason, .. } if reason.contains("timed out")
    )));
}

#[test]
fn refused_requests_do_not_leak_router_state() {
    let (session, _) = session_for(ready());
    let limits = SteamLimits {
        max_peers: 1,
        max_pending_requests: 2,
        ..SteamLimits::default()
    };
    let mut server = SteamServerEndpoint::listen(
        session.clone(),
        SteamServerConfig::p2p(0).with_limits(limits),
    )
    .unwrap();
    let mut first =
        SteamClientEndpoint::connect_p2p(session.clone(), me(), 0, SteamClientConfig::default())
            .unwrap();
    for _ in 0..8 {
        server.poll(full()).unwrap();
        first.poll(full()).unwrap();
    }
    assert_eq!(server.connected_peers().len(), 1);
    let baseline = session.net_route_count();
    // Past max_peers (server full) and past the pending-request cap, with a tight accept budget.
    let mut spam: Vec<_> = (0..20)
        .map(|_| {
            SteamClientEndpoint::connect_p2p(session.clone(), me(), 0, SteamClientConfig::default())
                .unwrap()
        })
        .collect();
    let tight = PollBudget {
        max_accepts: 1,
        ..PollBudget::default()
    };
    for _ in 0..60 {
        server.poll(tight).unwrap();
        first.poll(full()).unwrap();
        for client in &mut spam {
            client.poll(full()).unwrap();
        }
    }
    assert!(spam.iter().all(|c| c.connected_peers().is_empty()));
    drop(spam);
    assert_eq!(
        session.net_route_count(),
        baseline,
        "refused connections left ownership entries behind"
    );
}

#[test]
fn a_newer_pending_claim_supersedes_an_older_one() {
    let (session, fake) = session_for(ready());
    let mut server =
        SteamServerEndpoint::listen(session.clone(), SteamServerConfig::p2p(0)).unwrap();
    let hello = |ticket: u32| {
        let mut f = vec![0x80, 1];
        f.extend_from_slice(&me().0.to_le_bytes());
        f.extend_from_slice(&ticket.to_le_bytes());
        f
    };
    let (t1, t2) = {
        let mut f = fake.lock().unwrap();
        (
            f.auth_session_ticket().unwrap().handle,
            f.auth_session_ticket().unwrap().handle,
        )
    };
    let a = Raw::open(&session, &fake, &mut server);
    let b = Raw::open(&session, &fake, &mut server);
    // Both claims arrive before Steam answers either.
    a.send(&hello(t1));
    b.send(&hello(t2));
    for _ in 0..4 {
        server.poll(full()).unwrap();
    }
    assert_eq!(server.connected_peers().len(), 1, "the newer claim won");
    assert_eq!(server.pending_peers(), 0);
}

#[test]
fn a_backlogged_request_does_not_stall_other_events() {
    let (session, _) = session_for(ready());
    let mut server =
        SteamServerEndpoint::listen(session.clone(), SteamServerConfig::p2p(0)).unwrap();
    let mut client =
        SteamClientEndpoint::connect_p2p(session.clone(), me(), 0, SteamClientConfig::default())
            .unwrap();
    let tight = PollBudget {
        max_accepts: 1,
        ..PollBudget::default()
    };
    for _ in 0..8 {
        server.poll(tight).unwrap();
        client.poll(full()).unwrap();
    }
    assert_eq!(server.connected_peers().len(), 1);
    // A second request waits behind a zero accept budget while the first peer disconnects.
    let _second =
        SteamClientEndpoint::connect_p2p(session, me(), 0, SteamClientConfig::default()).unwrap();
    client.disconnect(SERVER_PEER);
    let no_accepts = PollBudget {
        max_accepts: 0,
        max_messages: 8,
        max_bytes: 1024,
    };
    let events = server.poll(no_accepts).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, LinkEvent::Disconnected { .. })),
        "{events:?}"
    );
}

#[test]
fn an_unauthenticated_peer_cannot_evict_a_pending_validation() {
    let (session, fake) = session_for(ready());
    let addr: std::net::SocketAddr = "127.0.0.1:27040".parse().unwrap();
    let mut server =
        SteamServerEndpoint::listen(session.clone(), SteamServerConfig::ip(addr)).unwrap();
    let hello = |ticket: &[u8]| {
        let mut f = vec![0x80, 1];
        f.extend_from_slice(&me().0.to_le_bytes());
        f.extend_from_slice(ticket);
        f
    };
    let real = fake.lock().unwrap().auth_session_ticket().unwrap().handle;
    let open = |server: &mut SteamServerEndpoint| {
        let conn = fake.lock().unwrap().net_connect_ip(addr).unwrap();
        server.poll(full()).unwrap();
        server.poll(full()).unwrap();
        Raw {
            fake: fake.clone(),
            conn,
        }
    };
    let honest = open(&mut server);
    let attacker = open(&mut server);
    // The honest login is validating when the attacker claims the same SteamID with junk.
    honest.send(&hello(&real.to_le_bytes()));
    server.poll(full()).unwrap();
    assert_eq!(server.pending_peers(), 2);
    attacker.send(&hello(b"junk"));
    for _ in 0..4 {
        server.poll(full()).unwrap();
    }
    assert_eq!(
        server.connected_peers().len(),
        1,
        "the honest login survived"
    );
    assert_eq!(server.pending_peers(), 0);
    assert_eq!(fake.lock().unwrap().net_auth_sessions(), 1);
}

#[test]
fn a_stale_verdict_for_a_superseded_ticket_does_not_admit_the_new_peer() {
    // Steam accepts the first login's ticket and rejects the second's.
    let (session, fake) = session_for(ready().with_auth_rejection_after(1));
    let mut server =
        SteamServerEndpoint::listen(session.clone(), SteamServerConfig::p2p(0)).unwrap();
    let hello = |ticket: u32| {
        let mut f = vec![0x80, 1];
        f.extend_from_slice(&me().0.to_le_bytes());
        f.extend_from_slice(&ticket.to_le_bytes());
        f
    };
    let (t1, t2) = {
        let mut f = fake.lock().unwrap();
        (
            f.auth_session_ticket().unwrap().handle,
            f.auth_session_ticket().unwrap().handle,
        )
    };
    let a = Raw::open(&session, &fake, &mut server);
    let b = Raw::open(&session, &fake, &mut server);
    // Both tickets are valid and both hellos are read in one poll, so the first login's
    // accepted verdict is still queued in the backend when the second claim supersedes it.
    // The endpoint must discard that verdict instead of routing it to the new claim, whose
    // own verdict is the rejection: the second peer is never authenticated.
    a.send(&hello(t1));
    b.send(&hello(t2));
    let mut events = Vec::new();
    for _ in 0..4 {
        events.extend(server.poll(full()).unwrap());
    }
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, LinkEvent::Authenticated(_) | LinkEvent::Connected(_))),
        "no peer may be authenticated: {events:?}"
    );
    assert!(server.connected_peers().is_empty());
    assert_eq!(server.pending_peers(), 0);
}
