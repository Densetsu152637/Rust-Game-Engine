//! Lobby state machine over the fake lobby world.

mod common;

use engine_network::{MessageEndpoint, PollBudget, SERVER_PEER};
use engine_steam::*;
use std::sync::{Arc, Mutex};

struct Shared {
    fake: Arc<Mutex<FakeBackend>>,
}

impl SteamBackend for Shared {
    fn status(&self) -> SteamStatus {
        self.fake.lock().unwrap().status()
    }
    fn lobby_backend(&mut self) -> Option<&mut dyn LobbyBackend> {
        Some(self)
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
common::forward_lobby!(Shared);

fn me() -> SteamId {
    SteamId(76_561_198_000_000_001)
}
fn other(n: u64) -> SteamId {
    SteamId(76_561_198_000_000_100 + n)
}

struct World {
    fake: Arc<Mutex<FakeBackend>>,
    session: SteamSession,
    lobbies: SteamLobbies,
}

fn world() -> World {
    let fake = Arc::new(Mutex::new(FakeBackend::ready(AppId(480), "tester")));
    let session = SteamSession::new(Box::new(Shared { fake: fake.clone() }));
    let lobbies = SteamLobbies::new(session.clone()).unwrap();
    World {
        fake,
        session,
        lobbies,
    }
}

impl World {
    fn fake(&self) -> std::sync::MutexGuard<'_, FakeBackend> {
        self.fake.lock().unwrap()
    }
    /// Creates a lobby and returns its id.
    fn create(&mut self, max: u32) -> LobbyId {
        self.lobbies.create(LobbyVisibility::Public, max).unwrap();
        let events = self.lobbies.poll();
        match events.as_slice() {
            [LobbyEvent::Created { lobby }] => *lobby,
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[test]
fn create_reports_created_once_and_owns_the_lobby() {
    let mut w = world();
    let id = w.create(4);
    let view = w.lobbies.lobby(id).unwrap();
    assert_eq!(view.owner, me());
    assert_eq!(view.members.len(), 1);
    assert_eq!(view.max_members, 4);
    // The creator's own "entered" callback produces no second event.
    assert!(w.lobbies.poll().is_empty());
}

#[test]
fn invalid_requests_fail_locally() {
    let mut w = world();
    assert!(w.lobbies.create(LobbyVisibility::Public, 0).is_err());
    assert!(w.lobbies.create(LobbyVisibility::Public, 251).is_err());
    assert!(matches!(
        w.lobbies.leave(LobbyId(999)),
        Err(SteamError::NotFound(_))
    ));
    let id = w.create(2);
    assert!(matches!(
        w.lobbies.set_data(id, "a\0b", Some("x")),
        Err(SteamError::InvalidName(_))
    ));
    assert!(matches!(
        w.lobbies.set_data(id, HOST_KEY, Some("x")),
        Err(SteamError::InvalidName(_))
    ));
    assert!(matches!(
        w.lobbies.set_data(id, &"k".repeat(256), Some("x")),
        Err(SteamError::InvalidName(_))
    ));
}

#[test]
fn join_member_events_and_leave() {
    let mut w = world();
    let id = w.fake().sim_add_lobby(other(1), 4, &[("mode", "ctf")]);
    w.lobbies.join(id).unwrap();
    assert_eq!(w.lobbies.poll(), vec![LobbyEvent::Joined { lobby: id }]);
    let view = w.lobbies.lobby(id).unwrap();
    assert_eq!(view.owner, other(1));
    assert!(view.members.contains(&me()) && view.members.contains(&other(1)));
    assert_eq!(view.data["mode"], "ctf");

    w.fake().sim_lobby_join(id, other(2));
    assert_eq!(
        w.lobbies.poll(),
        vec![LobbyEvent::MemberJoined {
            lobby: id,
            user: other(2)
        }]
    );
    w.fake().sim_lobby_leave(id, other(2));
    assert_eq!(
        w.lobbies.poll(),
        vec![LobbyEvent::MemberLeft {
            lobby: id,
            user: other(2)
        }]
    );
    // Joining again is a no-op.
    w.lobbies.join(id).unwrap();
    assert!(w.lobbies.poll().is_empty());

    w.lobbies.leave(id).unwrap();
    assert_eq!(w.lobbies.poll(), vec![LobbyEvent::Left { lobby: id }]);
    assert!(w.lobbies.lobby(id).is_none());
    assert_eq!(w.fake().call_count("lobby_leave"), 1);
}

#[test]
fn host_change_is_reported_when_the_owner_leaves() {
    let mut w = world();
    let id = w.fake().sim_add_lobby(other(1), 4, &[]);
    w.fake().sim_lobby_join(id, other(2));
    w.lobbies.join(id).unwrap();
    w.lobbies.poll();
    w.fake().sim_lobby_leave(id, other(1));
    let events = w.lobbies.poll();
    assert!(events.contains(&LobbyEvent::MemberLeft {
        lobby: id,
        user: other(1)
    }));
    assert!(events.contains(&LobbyEvent::OwnerChanged {
        lobby: id,
        owner: other(2)
    }));
    assert_eq!(w.lobbies.lobby(id).unwrap().owner, other(2));
}

#[test]
fn member_limit_and_missing_lobbies_fail_the_join() {
    let mut w = world();
    let full = w.fake().sim_add_lobby(other(1), 1, &[]);
    w.lobbies.join(full).unwrap();
    assert_eq!(
        w.lobbies.poll(),
        vec![LobbyEvent::JoinFailed {
            lobby: full,
            reason: EnterError::Full
        }]
    );
    w.lobbies.join(LobbyId(42)).unwrap();
    assert_eq!(
        w.lobbies.poll(),
        vec![LobbyEvent::JoinFailed {
            lobby: LobbyId(42),
            reason: EnterError::NotFound
        }]
    );
    assert!(w.lobbies.lobby(full).is_none());
    assert_eq!(w.lobbies.joined().count(), 0);
}

#[test]
fn list_applies_filters_slots_and_limits() {
    let mut w = world();
    w.fake().sim_add_lobby(other(1), 4, &[("mode", "ctf")]);
    w.fake().sim_add_lobby(other(2), 4, &[("mode", "dm")]);
    w.fake().sim_add_lobby(other(3), 1, &[("mode", "ctf")]);
    let want = |w: &mut World, filter: LobbyFilter| -> Vec<SteamId> {
        w.lobbies.list(&filter).unwrap();
        match w.lobbies.poll().as_slice() {
            [LobbyEvent::ListReceived { lobbies }] => lobbies.iter().map(|l| l.owner).collect(),
            other => panic!("unexpected {other:?}"),
        }
    };
    let ctf = LobbyFilter {
        string_equals: vec![("mode".into(), "ctf".into())],
        ..LobbyFilter::default()
    };
    assert_eq!(want(&mut w, ctf.clone()), vec![other(1), other(3)]);
    let roomy = LobbyFilter {
        open_slots: 2,
        ..ctf.clone()
    };
    assert_eq!(want(&mut w, roomy), vec![other(1)]);
    let one = LobbyFilter {
        max_results: 1,
        ..LobbyFilter::default()
    };
    assert_eq!(want(&mut w, one).len(), 1);
    let none = LobbyFilter {
        string_equals: vec![("mode".into(), "none".into())],
        ..LobbyFilter::default()
    };
    assert!(want(&mut w, none).is_empty());
}

#[test]
fn metadata_changes_are_diffed_and_owner_only() {
    let mut w = world();
    let id = w.create(4);
    w.lobbies.set_data(id, "map", Some("dust")).unwrap();
    assert_eq!(
        w.lobbies.poll(),
        vec![LobbyEvent::DataChanged {
            lobby: id,
            key: "map".into(),
            value: Some("dust".into())
        }]
    );
    w.lobbies.set_data(id, "map", None).unwrap();
    assert_eq!(
        w.lobbies.poll(),
        vec![LobbyEvent::DataChanged {
            lobby: id,
            key: "map".into(),
            value: None
        }]
    );
    // Not the owner: refused locally, before Steam.
    let theirs = w.fake().sim_add_lobby(other(1), 4, &[]);
    w.lobbies.join(theirs).unwrap();
    w.lobbies.poll();
    assert!(matches!(
        w.lobbies.set_data(theirs, "map", Some("x")),
        Err(SteamError::Rejected(_))
    ));
}

#[test]
fn out_of_order_callbacks_still_produce_the_right_events() {
    let mut w = world();
    // Held callbacks: the creator's `Entered` arrives before `Created`.
    w.fake().hold_lobby_callbacks(true);
    w.lobbies.create(LobbyVisibility::Public, 4).unwrap();
    w.fake().release_lobby_callbacks(true);
    let events = w.lobbies.poll();
    let [LobbyEvent::Created { lobby }] = events.as_slice() else {
        panic!("{events:?}");
    };
    let id = *lobby;
    assert!(w.lobbies.lobby(id).is_some());

    // Members join and metadata changes; callbacks arrive reversed and twice.
    w.fake().hold_lobby_callbacks(true);
    w.fake().sim_lobby_join(id, other(1));
    w.lobbies.set_data(id, "a", Some("1")).unwrap();
    w.fake().sim_lobby_join(id, other(2));
    w.fake().release_lobby_callbacks(true);
    w.fake().duplicate_lobby_callbacks();
    let events = w.lobbies.poll();
    let joined = events
        .iter()
        .filter(|e| matches!(e, LobbyEvent::MemberJoined { .. }))
        .count();
    let data = events
        .iter()
        .filter(|e| matches!(e, LobbyEvent::DataChanged { .. }))
        .count();
    assert_eq!((joined, data, events.len()), (2, 1, 3), "{events:?}");

    // Change callbacks for a lobby not entered yet wait for the join.
    let theirs = w.fake().sim_add_lobby(other(5), 4, &[]);
    w.fake().hold_lobby_callbacks(true);
    w.lobbies.join(theirs).unwrap();
    w.fake().sim_lobby_set_data(theirs, "x", "1");
    w.fake().release_lobby_callbacks(true);
    let events = w.lobbies.poll();
    assert_eq!(
        events,
        vec![LobbyEvent::Joined { lobby: theirs }],
        "the join snapshot already contains the data"
    );
    assert_eq!(w.lobbies.lobby(theirs).unwrap().data["x"], "1");
}

#[test]
fn losing_the_lobby_is_reported_as_left() {
    let mut w = world();
    let id = w.fake().sim_add_lobby(other(1), 4, &[]);
    w.lobbies.join(id).unwrap();
    w.lobbies.poll();
    w.fake().sim_lobby_destroy(id);
    // One empty read is not enough; the next poll confirms.
    assert!(w.lobbies.poll().is_empty());
    assert_eq!(w.lobbies.poll(), vec![LobbyEvent::Left { lobby: id }]);
    assert!(w.lobbies.lobby(id).is_none());
}

#[test]
fn creation_failure_is_an_event() {
    let mut w = world();
    w.fake().fail_next_lobby_create("rate limited");
    w.lobbies.create(LobbyVisibility::Public, 4).unwrap();
    assert_eq!(
        w.lobbies.poll(),
        vec![LobbyEvent::CreateFailed {
            reason: "rate limited".into()
        }]
    );
    assert_eq!(w.lobbies.joined().count(), 0);
}

#[test]
fn lobbies_need_a_backend_with_lobby_support() {
    let session = SteamSession::new(Box::new(NullBackend::not_compiled()));
    assert!(SteamLobbies::new(session).is_err());
    // A ready backend without a lobby surface refuses cleanly.
    struct NoLobbies(FakeBackend);
    impl SteamBackend for NoLobbies {
        fn status(&self) -> SteamStatus {
            self.0.status()
        }
    }
    let session = SteamSession::new(Box::new(NoLobbies(FakeBackend::ready(AppId(480), "t"))));
    let mut lobbies = SteamLobbies::new(session).unwrap();
    assert!(lobbies.create(LobbyVisibility::Public, 2).is_err());
    assert!(lobbies.poll().is_empty());
}

#[test]
fn the_lobby_carries_the_p2p_connect_identity() {
    let mut w = world();
    w.lobbies.create_p2p(LobbyVisibility::Public, 4, 7).unwrap();
    let events = w.lobbies.poll();
    let id = match events.first() {
        Some(LobbyEvent::Created { lobby }) => *lobby,
        other => panic!("{other:?}"),
    };
    // Creation published the host identity; Steam confirms the two keys.
    let changed = w.lobbies.poll();
    assert_eq!(
        changed
            .iter()
            .filter(|e| matches!(e, LobbyEvent::DataChanged { .. }))
            .count(),
        2,
        "{changed:?}"
    );
    let want = ConnectIdentity {
        host: me(),
        virtual_port: 7,
    };
    assert_eq!(w.lobbies.connect_identity(id), Some(want));
    // The host cannot dial itself.
    assert!(
        w.lobbies
            .connect_client(id, SteamClientConfig::default())
            .is_err()
    );
    let listed = w.fake().lobby_info(id).unwrap();
    assert_eq!(listed_identity(&listed), Some(want));
}

#[test]
fn a_joiner_connects_to_the_host_named_by_the_lobby() {
    // The fake resolves a dial only to its own account, so the "remote" host is the
    // local account: the joiner is a second SteamLobbies on the same session, and the
    // owner/host check is bypassed by dialing the published identity.
    let mut w = world();
    let mut server =
        SteamServerEndpoint::listen(w.session.clone(), SteamServerConfig::p2p(7)).unwrap();
    let remote =
        w.fake()
            .sim_add_lobby(me(), 4, &[(HOST_KEY, &me().0.to_string()), (PORT_KEY, "7")]);
    w.lobbies.join(remote).unwrap();
    w.lobbies.poll();
    let identity = w.lobbies.connect_identity(remote).unwrap();
    assert_eq!(identity.virtual_port, 7);
    let mut client = SteamClientEndpoint::connect_p2p(
        w.session.clone(),
        identity.host,
        identity.virtual_port,
        SteamClientConfig::default(),
    )
    .unwrap();
    for _ in 0..8 {
        server.poll(PollBudget::default()).unwrap();
        client.poll(PollBudget::default()).unwrap();
    }
    assert_eq!(client.connected_peers(), vec![SERVER_PEER]);
}

#[test]
fn stale_host_metadata_is_ignored() {
    let mut w = world();
    let id = w.fake().sim_add_lobby(
        other(1),
        4,
        &[(HOST_KEY, &other(1).0.to_string()), (PORT_KEY, "9")],
    );
    w.fake().sim_lobby_join(id, other(2));
    w.lobbies.join(id).unwrap();
    w.lobbies.poll();
    let identity = w.lobbies.connect_identity(id).unwrap();
    assert_eq!((identity.host, identity.virtual_port), (other(1), 9));
    // A different account hosts this lobby, so the connect helper builds an endpoint.
    assert!(
        w.lobbies
            .connect_client(id, SteamClientConfig::default())
            .is_ok()
    );
    // The host leaves: ownership moves, the old identity no longer names the owner.
    w.fake().sim_lobby_leave(id, other(1));
    w.lobbies.poll();
    assert_eq!(w.lobbies.connect_identity(id), None);
    assert!(
        w.lobbies
            .connect_client(id, SteamClientConfig::default())
            .is_err()
    );
}

#[test]
fn a_transient_empty_read_is_not_a_false_left() {
    let mut w = world();
    let id = w.fake().sim_add_lobby(other(1), 4, &[]);
    w.lobbies.join(id).unwrap();
    w.lobbies.poll();
    w.fake().sim_lobby_join(id, other(2));
    w.fake().glitch_lobby_info(1);
    assert!(w.lobbies.poll().is_empty(), "no Left on one empty read");
    assert!(w.lobbies.lobby(id).is_some());
    // The next look sees the lobby again and reports the change that was pending.
    assert_eq!(
        w.lobbies.poll(),
        vec![LobbyEvent::MemberJoined {
            lobby: id,
            user: other(2)
        }]
    );
    // A later single glitch does not carry over as a confirmation.
    w.fake().sim_lobby_set_data(id, "a", "1");
    w.fake().glitch_lobby_info(1);
    w.lobbies.poll();
    w.fake().sim_lobby_set_data(id, "b", "2");
    let events = w.lobbies.poll();
    assert!(
        !events.contains(&LobbyEvent::Left { lobby: id }),
        "{events:?}"
    );
    assert!(w.lobbies.lobby(id).is_some());
}

#[test]
fn leaving_while_a_join_is_in_flight_undoes_it() {
    let mut w = world();
    let id = w.fake().sim_add_lobby(other(1), 4, &[]);
    w.fake().hold_lobby_callbacks(true);
    w.lobbies.join(id).unwrap();
    w.lobbies.leave(id).unwrap();
    assert_eq!(w.lobbies.poll(), vec![LobbyEvent::Left { lobby: id }]);
    assert_eq!(
        w.fake().call_count("lobby_leave"),
        0,
        "nothing to leave yet"
    );
    w.fake().release_lobby_callbacks(false);
    w.fake().hold_lobby_callbacks(false);
    // The late success is undone: no Joined, and the account leaves the room.
    assert!(w.lobbies.poll().is_empty());
    assert!(w.lobbies.lobby(id).is_none());
    assert_eq!(w.fake().call_count("lobby_leave"), 1);
    assert!(!w.fake().lobby_info(id).unwrap().members.contains(&me()));
    // Joining again afterwards works normally.
    w.lobbies.join(id).unwrap();
    assert_eq!(w.lobbies.poll(), vec![LobbyEvent::Joined { lobby: id }]);
}
