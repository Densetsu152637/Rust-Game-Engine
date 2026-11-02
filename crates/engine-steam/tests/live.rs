//! Live Steam tests: `cargo test -p engine-steam --features steam -- --ignored core`.
//!
//! They use App ID 480 (Spacewar) and skip, passing, when no Steam client runs.
//! The test executable needs `steam_api64.dll` (Windows) next to it; `build.rs`
//! copies it there.
#![cfg(feature = "steam")]

use engine_steam::*;

/// Only one initialised Steam client may exist per process, so live tests take turns.
static LIVE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Initialises App 480, or prints a skip message and returns `None`. The guard
/// serialises tests; it must outlive the backend, so it is declared first.
fn steam_live() -> Option<(std::sync::MutexGuard<'static, ()>, RealBackend)> {
    let guard = LIVE.lock().unwrap_or_else(|p| p.into_inner());
    let mut backend = RealBackend::new();
    match backend.init(Some(AppId(480))) {
        SteamStatus::Ready { .. } => Some((guard, backend)),
        other => {
            println!("skipped: Steam not running ({other:?})");
            None
        }
    }
}

#[test]
#[ignore = "needs a running Steam client"]
fn core() {
    let Some((_guard, mut steam)) = steam_live() else {
        // Uninitialised calls must fail softly, never panic.
        let mut backend = RealBackend::new();
        assert!(backend.auth_session_ticket().is_err());
        assert!(backend.activate_overlay("friends").is_err());
        assert!(backend.run_callbacks().is_empty());
        return;
    };
    let SteamStatus::Ready { app_id, user } = steam.status() else {
        panic!("steam_live returned a non-ready backend");
    };
    assert_eq!(app_id, AppId(480));
    assert_ne!(user.id, SteamId(0));
    println!("signed in as {} ({})", user.name, user.id.0);

    // Second init in one process is refused, not undefined.
    let mut second = RealBackend::new();
    assert!(matches!(
        second.init(Some(AppId(480))),
        SteamStatus::InitFailed(_)
    ));

    assert_eq!(steam.restart_app_if_necessary(AppId(480)), Ok(false));
    // Steam returns an empty ticket right after init; it works once callbacks have run.
    let mut ticket = steam.auth_session_ticket();
    for _ in 0..30 {
        if ticket.is_ok() {
            break;
        }
        let _ = steam.run_callbacks();
        std::thread::sleep(std::time::Duration::from_millis(100));
        ticket = steam.auth_session_ticket();
    }
    let ticket = ticket.expect("auth ticket");
    assert!(!ticket.data.is_empty());
    steam.cancel_auth_ticket(ticket.handle).expect("cancel");
    assert!(matches!(
        steam.cancel_auth_ticket(ticket.handle),
        Err(SteamError::NotFound(_))
    ));

    steam
        .set_rich_presence("status", Some("engine-steam live test"))
        .expect("set");
    steam.set_rich_presence("status", None).expect("clear");
    assert!(matches!(
        steam.set_rich_presence("a\0b", Some("x")),
        Err(SteamError::InvalidName(_))
    ));
    let _ = steam.run_callbacks();
}

/// Read-only: lists what Spacewar exposes to this account and reads one stat. It
/// never writes, so the account is untouched.
#[test]
#[ignore = "needs a running Steam client"]
fn stats() {
    let Some((_guard, mut steam)) = steam_live() else {
        return;
    };
    steam.request_current_stats().expect("request");
    let mut received = false;
    for _ in 0..50 {
        if steam.run_callbacks().contains(&SteamEvent::StatsReceived) {
            received = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    println!("stats received callback: {received}");
    for name in ["NumGames", "NumWins", "NumLosses", "FeetTraveled"] {
        println!("stat {name}: {:?}", steam.stat(name));
    }
    for name in ["ACH_WIN_ONE_GAME", "ACH_WIN_100_GAMES"] {
        println!("achievement {name}: {:?}", steam.achievement(name));
    }
    // An unknown name is a soft error, never a panic.
    assert!(steam.stat("definitely_not_a_stat").is_err());
    assert!(steam.achievement("definitely_not_an_achievement").is_err());
}

/// Round trip through Steam Cloud on App 480: write, list, timestamp, read, delete.
/// When Cloud is disabled for the app or account, checks the degrade path instead.
#[test]
#[ignore = "needs a running Steam client"]
fn cloud() {
    let Some((_guard, mut steam)) = steam_live() else {
        return;
    };
    let app = steam.cloud_enabled_for_app().expect("app flag");
    let account = steam.cloud_enabled_for_account().expect("account flag");
    println!("cloud enabled: app={app} account={account}");
    let name = "saves/engine_steam_live.json";
    let payload = br#"{"schema":1,"content_version":1,"data":{"live":true}}"#;
    if !(app && account) {
        assert_eq!(
            steam.cloud_write(name, payload),
            Err(SteamError::CloudDisabled)
        );
        println!("degrade path verified: writes are refused, the game stays local-only");
        return;
    }
    steam.cloud_write(name, payload).expect("write");
    assert!(steam.cloud_list().expect("list").iter().any(|n| n == name));
    let stamp = steam.cloud_timestamp(name).expect("timestamp");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    println!("cloud timestamp {stamp}, now {now}");
    assert!((now - stamp).abs() < 600);
    assert_eq!(steam.cloud_read(name).expect("read"), payload);
    assert_eq!(
        steam.cloud_write("../x", payload),
        Err(SteamError::InvalidName("../x".into()))
    );
    steam.cloud_delete(name).expect("delete");
    assert!(matches!(
        steam.cloud_timestamp(name),
        Err(SteamError::NotFound(_))
    ));
}

mod net_live {
    use super::*;
    use engine_network::{
        Delivery, Lane, LinkEvent, MessageEndpoint, PollBudget, SERVER_PEER, TransportSecurity,
    };
    use std::time::{Duration, Instant};

    struct Pair {
        session: SteamSession,
        server: SteamServerEndpoint,
        client: SteamClientEndpoint,
        server_events: Vec<LinkEvent>,
        client_events: Vec<LinkEvent>,
    }

    impl Pair {
        /// Pumps Steam callbacks and both endpoints until `done` or the timeout.
        fn drive(&mut self, what: &str, seconds: u64, done: impl Fn(&Pair) -> bool) {
            let end = Instant::now() + Duration::from_secs(seconds);
            while !done(self) {
                assert!(
                    Instant::now() < end,
                    "timed out waiting for {what}; server {:?} client {:?}",
                    self.server_events,
                    self.client_events
                );
                let _ = self.session.run_callbacks();
                self.server_events.extend(
                    self.server
                        .poll(PollBudget::default())
                        .expect("server poll"),
                );
                self.client_events.extend(
                    self.client
                        .poll(PollBudget::default())
                        .expect("client poll"),
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        fn connected(&self) -> bool {
            !self.server.connected_peers().is_empty() && !self.client.connected_peers().is_empty()
        }
    }

    fn exchange(pair: &mut Pair) {
        let peer = pair.server.connected_peers()[0];
        assert_eq!(
            pair.server.transport(),
            engine_network::TransportKind::Steam
        );
        assert_eq!(pair.server.security(), TransportSecurity::PlatformEncrypted);
        let me = pair.server.local_steam_id();
        assert_eq!(pair.server.peer_steam_id(peer), Some(me));
        println!("admitted: peer {peer} is {}", me.0);
        let big = vec![0xAB; 200_000];
        for i in 0u8..5 {
            pair.client
                .send(SERVER_PEER, Lane::RPC, Delivery::ReliableOrdered, &[i])
                .expect("send");
        }
        pair.client
            .send(
                SERVER_PEER,
                Lane::REPLICATION,
                Delivery::ReliableOrdered,
                &big,
            )
            .expect("big send");
        pair.server
            .send(peer, Lane::INPUT, Delivery::UnreliableSequenced, b"tick")
            .expect("server send");
        pair.drive("payloads", 15, |p| {
            let got = |events: &[LinkEvent]| {
                events
                    .iter()
                    .filter(|e| matches!(e, LinkEvent::Message { .. }))
                    .count()
            };
            got(&p.server_events) >= 6 && got(&p.client_events) >= 1
        });
        let rpc: Vec<u8> = pair
            .server_events
            .iter()
            .filter_map(|e| match e {
                LinkEvent::Message { lane, payload, .. } if *lane == Lane::RPC => Some(payload[0]),
                _ => None,
            })
            .collect();
        assert_eq!(rpc, vec![0, 1, 2, 3, 4]);
        assert!(pair.server_events.iter().any(
            |e| matches!(e, LinkEvent::Message { payload, .. } if payload.len() == big.len())
        ));
        let stats = pair.client.stats(SERVER_PEER).expect("stats");
        println!("client stats: {stats:?}");
        assert!(stats.sent_packets >= 6);
        assert!(pair.client.disconnect(SERVER_PEER));
        pair.drive("the server to see the disconnect", 15, |p| {
            p.server_events
                .iter()
                .any(|e| matches!(e, LinkEvent::Disconnected { .. }))
        });
        println!("disconnect reason: {:?}", pair.server_events.last());
    }

    /// `ConnectByIPAddress` to a local listen socket, with auth session tickets.
    #[test]
    #[ignore = "needs a running Steam client"]
    fn net_ip() {
        let Some((_guard, backend)) = steam_live() else {
            return;
        };
        let session = SteamSession::new(Box::new(backend));
        let addr = "127.0.0.1:27333".parse().unwrap();
        let server = SteamServerEndpoint::listen(session.clone(), SteamServerConfig::ip(addr))
            .expect("listen");
        let client =
            SteamClientEndpoint::connect_ip(session.clone(), addr, SteamClientConfig::default())
                .expect("connect");
        let mut pair = Pair {
            session,
            server,
            client,
            server_events: Vec::new(),
            client_events: Vec::new(),
        };
        pair.drive("the IP loopback to connect", 20, Pair::connected);
        exchange(&mut pair);
    }

    /// `ConnectP2P` to our own SteamID through the relay network (identity from Steam).
    #[test]
    #[ignore = "needs a running Steam client"]
    fn net_p2p() {
        let Some((_guard, backend)) = steam_live() else {
            return;
        };
        let session = SteamSession::new(Box::new(backend));
        let SteamStatus::Ready { user, .. } = session.status() else {
            panic!("not ready");
        };
        let config = SteamServerConfig::p2p(0).with_auth(AuthPolicy::IdentityOnly);
        let server = SteamServerEndpoint::listen(session.clone(), config).expect("listen");
        let client = SteamClientEndpoint::connect_p2p(
            session.clone(),
            user.id,
            0,
            SteamClientConfig::default(),
        )
        .expect("connect");
        println!("relay status at start: {:?}", client.relay_status());
        let mut pair = Pair {
            session,
            server,
            client,
            server_events: Vec::new(),
            client_events: Vec::new(),
        };
        pair.drive("the P2P loopback to connect", 60, Pair::connected);
        println!(
            "relay status when connected: {:?}",
            pair.client.relay_status()
        );
        exchange(&mut pair);
    }
}

mod lobby_live {
    use super::*;
    use std::time::{Duration, Instant};

    /// Polls callbacks and lobby events until `done` matches or the timeout passes.
    fn wait_for(
        session: &SteamSession,
        lobbies: &mut SteamLobbies,
        seconds: u64,
        mut done: impl FnMut(&LobbyEvent) -> bool,
    ) -> Option<Vec<LobbyEvent>> {
        let end = Instant::now() + Duration::from_secs(seconds);
        let mut all = Vec::new();
        while Instant::now() < end {
            let _ = session.run_callbacks();
            let events = lobbies.poll();
            let hit = events.iter().any(&mut done);
            all.extend(events);
            if hit {
                return Some(all);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        println!("timed out; events so far: {all:?}");
        None
    }

    /// Creates a private lobby on App 480, publishes the P2P host identity, changes
    /// metadata, sees the confirmations, then leaves.
    #[test]
    #[ignore = "needs a running Steam client"]
    fn lobby_create_and_leave() {
        let Some((_guard, backend)) = steam_live() else {
            return;
        };
        let session = SteamSession::new(Box::new(backend));
        let mut lobbies = SteamLobbies::new(session.clone()).expect("lobbies");
        lobbies
            .create_p2p(LobbyVisibility::Private, 2, 5)
            .expect("create");
        let events = wait_for(&session, &mut lobbies, 30, |e| {
            matches!(
                e,
                LobbyEvent::Created { .. } | LobbyEvent::CreateFailed { .. }
            )
        })
        .expect("a creation result");
        let Some(LobbyEvent::Created { lobby }) = events
            .iter()
            .find(|e| matches!(e, LobbyEvent::Created { .. }))
            .cloned()
        else {
            panic!("lobby creation failed: {events:?}");
        };
        println!("created lobby {}", lobby.0);
        let me = lobbies.me();
        let view = lobbies.lobby(lobby).expect("view").clone();
        assert_eq!(view.owner, me);
        assert!(view.members.contains(&me));

        // The published host identity arrives back as metadata.
        wait_for(&session, &mut lobbies, 3, |_| false);
        assert_eq!(
            lobbies.connect_identity(lobby),
            Some(ConnectIdentity {
                host: me,
                virtual_port: 5
            }),
            "host identity published: {:?}",
            lobbies.lobby(lobby)
        );
        lobbies
            .set_data(lobby, "mode", Some("engine-live-test"))
            .expect("set data");
        wait_for(&session, &mut lobbies, 3, |_| false);
        assert_eq!(
            lobbies
                .lobby(lobby)
                .and_then(|v| v.data.get("mode"))
                .map(String::as_str),
            Some("engine-live-test")
        );

        lobbies.leave(lobby).expect("leave");
        let left = lobbies.poll();
        assert_eq!(left, vec![LobbyEvent::Left { lobby }]);
        assert!(lobbies.lobby(lobby).is_none());
        let _ = session.run_callbacks();
    }
}

/// Steam Input enumeration. Spacewar has no action manifest of ours, so this only checks
/// that init, `RunFrame` and controller listing work; it skips cleanly with no controller.
#[test]
#[ignore = "needs a running Steam client"]
fn input_enumeration() {
    let Some((_guard, mut steam)) = steam_live() else {
        let mut backend = RealBackend::new();
        assert!(backend.input_init(None).is_err());
        assert!(backend.controllers().is_err());
        return;
    };
    // Before init the Input calls are refused, not undefined.
    assert!(steam.controllers().is_err());
    steam.input_init(None).expect("Steam Input init");
    steam.input_init(None).expect("init is idempotent");
    let mut controllers = Vec::new();
    for _ in 0..10 {
        steam.input_run_frame().expect("run frame");
        let _ = steam.run_callbacks();
        controllers = steam.controllers().expect("controllers");
        if !controllers.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(controllers.iter().all(|c| c.0 != 0));
    if controllers.is_empty() {
        println!("skipped: no Steam Input controller connected");
        return;
    }
    println!("controllers: {controllers:?}");
    // An action Spacewar's manifest does not define is NotFound, not a crash.
    assert!(matches!(
        steam.input_digital(controllers[0], "engine_steam_no_such_action"),
        Err(SteamError::NotFound(_))
    ));
}
