//! Deterministic (sans-IO) behaviour of the combined service: authentication failures, lobby
//! rules, limits, matchmaking, tokens, host migration and fencing.
mod common;

use common::*;
use engine_online::{
    ClientBody, InMemoryDirectory, ServerBody, TokenError,
    protocol::{
        Attributes, Blob, CreateSession, ErrorCode, JoinTarget, LeaveReason, SessionFilter,
        SessionView, TicketSpec, TokenKind,
    },
};
use std::collections::BTreeMap;

const GAME: &str = "engine-game";

fn create(
    sim: &mut Sim,
    peer: u64,
    name: &str,
    private: bool,
    capacity: u16,
) -> (SessionView, Vec<u8>) {
    let body = ClientBody::CreateSession(CreateSession {
        name: name.into(),
        mode: "duel".into(),
        capacity,
        private,
        attributes: Attributes::from([("map".into(), "dust".into())]),
        candidates: Some(Blob(vec![peer as u8; 8])),
    });
    match sim.request(peer, body) {
        ServerBody::SessionCreated {
            session,
            join_token,
        } => (session, join_token.0),
        other => panic!("create failed: {other:?}"),
    }
}
fn join_by_id(sim: &mut Sim, peer: u64, id: u64) -> ServerBody {
    sim.request(
        peer,
        ClientBody::JoinSession {
            target: JoinTarget::Id { session_id: id },
            candidates: Some(Blob(vec![peer as u8; 8])),
        },
    )
}
fn join_by_code(sim: &mut Sim, peer: u64, code: &str) -> ServerBody {
    sim.request(
        peer,
        ClientBody::JoinSession {
            target: JoinTarget::Code { code: code.into() },
            candidates: Some(Blob(vec![peer as u8; 8])),
        },
    )
}
fn ticket(mode: &str, rating: i32) -> ClientBody {
    ClientBody::EnqueueTicket(TicketSpec {
        mode: mode.into(),
        region: "eu".into(),
        rating,
        party_size: 1,
        attributes: Attributes::new(),
        latencies: BTreeMap::new(),
        candidates: Some(Blob(vec![9; 4])),
    })
}
/// Game-server-side verification of a join token for the player called `name`.
fn verify_join(
    sim: &Sim,
    token: &[u8],
    name: &str,
    session: u64,
    epoch: u64,
) -> Result<(), TokenError> {
    sim.auth
        .verifier()
        .verify_join(
            token,
            GAME,
            session,
            epoch,
            format!("sim-cert-{name}").as_bytes(),
            sim.now().unix_secs,
        )
        .map(|_| ())
}

#[test]
fn authentication_failures_get_one_generic_answer_and_disconnect() {
    let mut sim = Sim::new(|_| {});
    let now = sim.now().unix_secs;
    let other = Authority::new("sim-key"); // same key id, different key material
    let good = |sim: &Sim, name: &str| sim.connect_token(name, name, 600);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        (
            "expired",
            sim.auth
                .token("engine-online", "p1", Sim::print_for("p1"), 60, now - 1000),
        ),
        (
            "wrong audience",
            sim.auth
                .token("someone-else", "p2", Sim::print_for("p2"), 60, now),
        ),
        (
            "wrong key",
            other.token("engine-online", "p3", Sim::print_for("p3"), 60, now),
        ),
        ("garbage", vec![0xAB; 200]),
        ("empty-ish", vec![1]),
        (
            "bound to another certificate",
            sim.auth
                .token("engine-online", "p6", Sim::print_for("mallory"), 60, now),
        ),
        ("not yet valid", {
            sim.auth.token(
                "engine-online",
                "p7",
                Sim::print_for("p7"),
                60,
                now + 10_000,
            )
        }),
    ];
    let mut tampered = good(&sim, "p5");
    let middle = tampered.len() / 2;
    tampered[middle] ^= 0x40;
    let mut cases = cases;
    cases.push(("tampered", tampered));
    for (i, (label, token)) in cases.into_iter().enumerate() {
        let peer = 10 + i as u64;
        let name = format!("p{}", [1, 2, 3, 4, 5, 6, 7, 5][i]);
        sim.admit(peer, &name);
        let response = sim.request(peer, ClientBody::Hello { token: Blob(token) });
        match response {
            ServerBody::Error { code, message } => {
                assert_eq!(code, ErrorCode::Unauthorized, "{label}");
                assert_eq!(message, "authentication failed", "{label}");
            }
            other => panic!("{label}: {other:?}"),
        }
        assert!(sim.disconnected.contains(&peer), "{label}");
        assert_eq!(sim.service.authenticated_peers(), 0, "{label}");
    }
    assert_eq!(sim.service.stats().auth_failures, 8);
}

#[test]
fn replayed_connect_token_is_rejected_even_from_the_rightful_certificate() {
    let mut sim = Sim::new(|_| {});
    let token = sim.connect_token("alice", "alice", 600);
    sim.admit(1, "alice");
    let hello = |token: &[u8]| ClientBody::Hello {
        token: Blob(token.to_vec()),
    };
    assert!(matches!(
        sim.request(1, hello(&token)),
        ServerBody::HelloOk { .. }
    ));
    // The same certificate reconnects with the captured token: a replay.
    sim.admit(2, "alice");
    assert_eq!(
        expect_error(sim.request(2, hello(&token))),
        ErrorCode::Unauthorized
    );
    assert!(sim.disconnected.contains(&2));
    // The first connection is unharmed.
    assert!(sim.service.subject_of(1).is_some());
}

#[test]
fn peers_without_a_valid_token_are_disconnected_at_the_deadline_and_get_no_service() {
    let mut sim = Sim::new(|c| c.limits.auth_timeout_ms = 1000);
    sim.admit(1, "slow");
    // Before authenticating, lobby requests are refused and the peer is dropped.
    assert_eq!(
        expect_error(sim.request(
            1,
            ClientBody::ListSessions {
                filter: SessionFilter::default(),
                after: None,
                limit: 5
            }
        )),
        ErrorCode::Unauthorized
    );
    assert!(sim.disconnected.contains(&1));

    sim.admit(2, "idle");
    sim.advance(999);
    assert!(!sim.disconnected.contains(&2));
    sim.advance(1);
    assert!(sim.disconnected.contains(&2));
    // Late traffic from the dropped peer is ignored, not served.
    let before = sim.inbox.get(&2).map_or(0, |q| q.len());
    sim.send_raw(
        2,
        br#"{"v":1,"id":1,"body":{"type":"leave_session","session_id":1}}"#,
    );
    assert_eq!(sim.inbox.get(&2).map_or(0, |q| q.len()), before);

    // A peer the binding never recorded a certificate for is refused outright.
    sim.service.on_connected(3, None, sim.now());
    sim.pump();
    assert!(sim.disconnected.contains(&3));
}

#[test]
fn malformed_pre_auth_traffic_and_wrong_versions_are_fatal() {
    let mut sim = Sim::new(|_| {});
    for (peer, payload) in [
        (1u64, b"not json".to_vec()),
        (2, br#"{"v":2,"id":1,"body":{"type":"ack"}}"#.to_vec()),
        (3, vec![0u8; 70_000]),
        (
            4,
            br#"{"v":1,"id":1,"body":{"type":"hello","token":"AA","extra":1}}"#.to_vec(),
        ),
    ] {
        sim.admit(peer, &format!("p{peer}"));
        sim.send_raw(peer, &payload);
        assert!(sim.disconnected.contains(&peer), "peer {peer}");
    }
}

#[test]
fn newest_connection_wins_and_banned_subjects_are_refused() {
    let mut sim = Sim::new(|_| {});
    sim.login(1, "alice");
    let (session, _) = create(&mut sim, 1, "room", false, 4);
    // Alice reconnects (new transport peer, new token) before the old link is reaped.
    sim.admit(2, "alice");
    let token = sim.connect_token("alice", "alice", 600);
    assert!(matches!(
        sim.request(2, ClientBody::Hello { token: Blob(token) }),
        ServerBody::HelloOk { .. }
    ));
    assert!(sim.disconnected.contains(&1), "old link is closed");
    assert_eq!(sim.service.authenticated_peers(), 1);
    assert_eq!(
        sim.service.lobby().session_count(),
        0,
        "old link left its session"
    );
    let _ = session;

    let mut directory = InMemoryDirectory::new(8);
    assert!(directory.ban("mallory"));
    let mut banned = Sim::with_directory(|_| {}, directory);
    banned.admit(5, "mallory");
    let token = banned.connect_token("mallory", "mallory", 600);
    assert_eq!(
        expect_error(banned.request(5, ClientBody::Hello { token: Blob(token) })),
        ErrorCode::Forbidden
    );
    assert!(banned.disconnected.contains(&5));
}

#[test]
fn service_refuses_join_token_lifetimes_the_issuer_would_never_sign() {
    let auth = Authority::new("k");
    let mut cfg = service_config(|_| {});
    cfg.join_token_ttl_secs = 25 * 3600;
    assert!(engine_online::OnlineService::new(cfg, auth.issuer(), auth.verifier()).is_err());
}

#[test]
fn admitting_token_expiry_disconnects_the_peer() {
    let mut sim = Sim::new(|_| {});
    sim.admit(1, "alice");
    let token = sim.connect_token("alice", "alice", 100);
    assert!(matches!(
        sim.request(1, ClientBody::Hello { token: Blob(token) }),
        ServerBody::HelloOk { .. }
    ));
    sim.advance(99_000);
    assert!(!sim.disconnected.contains(&1));
    sim.advance(1_000);
    assert!(sim.disconnected.contains(&1));
}

#[test]
fn lobby_flow_with_notifications_and_cert_bound_join_tokens() {
    let mut sim = Sim::new(|_| {});
    for (peer, name) in [(1, "alice"), (2, "bob"), (3, "carol")] {
        sim.login(peer, name);
    }
    let (public, alice_token) = create(&mut sim, 1, "Friday", false, 3);
    assert_eq!((public.epoch, public.host.as_deref()), (1, Some("alice")));
    assert!(public.code.is_none(), "public sessions have no code");
    // The creator's join token is bound to alice's certificate, session and epoch.
    assert_eq!(
        verify_join(&sim, &alice_token, "alice", public.id, 1),
        Ok(())
    );

    // Listing and filtering.
    match sim.request(
        2,
        ClientBody::ListSessions {
            filter: SessionFilter {
                mode: Some("duel".into()),
                attributes: Attributes::from([("map".into(), "dust".into())]),
                has_space: true,
                name_prefix: Some("Fri".into()),
            },
            after: None,
            limit: 10,
        },
    ) {
        ServerBody::SessionList { sessions, next } => {
            assert_eq!(sessions.len(), 1);
            assert_eq!((sessions[0].members, sessions[0].capacity), (1, 3));
            assert_eq!(next, None);
        }
        other => panic!("{other:?}"),
    }

    // Join by id; the host is notified; the joiner gets a token bound to their certificate.
    let ServerBody::Joined {
        session,
        join_token,
    } = join_by_id(&mut sim, 2, public.id)
    else {
        panic!("join failed");
    };
    assert_eq!(session.members.len(), 2);
    assert_eq!(
        verify_join(&sim, &join_token.0, "bob", public.id, 1),
        Ok(())
    );
    // Bob's token is useless to carol's certificate and for a different epoch.
    assert_eq!(
        verify_join(&sim, &join_token.0, "carol", public.id, 1),
        Err(TokenError::WrongCertificate)
    );
    assert_eq!(
        verify_join(&sim, &join_token.0, "bob", public.id, 2),
        Err(TokenError::MissingScope)
    );
    assert!(sim.take(1).iter().any(|n| matches!(
        n,
        ServerBody::MemberJoined { member, .. } if member.player == "bob"
            && member.candidates == Some(Blob(vec![2; 8]))
    )));

    // Ready and candidate updates reach the other member only.
    assert_eq!(
        sim.request(
            2,
            ClientBody::SetReady {
                session_id: public.id,
                ready: true
            }
        ),
        ServerBody::Ack {}
    );
    assert_eq!(
        sim.request(
            2,
            ClientBody::SetCandidates {
                session_id: public.id,
                candidates: Blob(vec![0xEE; 16])
            }
        ),
        ServerBody::Ack {}
    );
    let notes = sim.take(1);
    assert!(notes.iter().any(
        |n| matches!(n, ServerBody::MemberReady { player, ready: true, .. } if player == "bob")
    ));
    assert!(notes.iter().any(|n| matches!(n, ServerBody::MemberCandidates { candidates, .. } if candidates.0 == vec![0xEE; 16])));
    assert!(sim.take(2).is_empty());

    // Host-only operations: member is refused, host succeeds.
    assert_eq!(
        expect_error(sim.request(
            2,
            ClientBody::Kick {
                session_id: public.id,
                epoch: 1,
                player: "alice".into()
            }
        )),
        ErrorCode::Forbidden
    );
    assert_eq!(
        expect_error(sim.request(
            2,
            ClientBody::SetAttributes {
                session_id: public.id,
                epoch: 1,
                attributes: Attributes::new()
            }
        )),
        ErrorCode::Forbidden
    );
    assert_eq!(
        sim.request(
            1,
            ClientBody::SetAttributes {
                session_id: public.id,
                epoch: 1,
                attributes: Attributes::from([("map".into(), "ice".into())])
            }
        ),
        ServerBody::Ack {}
    );
    assert!(sim.take(2).iter().any(|n| matches!(n, ServerBody::AttributesChanged { attributes, .. } if attributes["map"] == "ice")));
    // Capacity: carol fills the last slot, a fourth player is turned away.
    assert!(matches!(
        join_by_id(&mut sim, 3, public.id),
        ServerBody::Joined { .. }
    ));
    sim.login(4, "dave");
    assert_eq!(
        expect_error(join_by_id(&mut sim, 4, public.id)),
        ErrorCode::Full
    );
    // Kick, then the kicked player is told and removed.
    sim.take(3);
    assert_eq!(
        sim.request(
            1,
            ClientBody::Kick {
                session_id: public.id,
                epoch: 1,
                player: "carol".into()
            }
        ),
        ServerBody::Ack {}
    );
    let carol = sim.take(3);
    assert!(carol.iter().any(|n| matches!(n, ServerBody::Kicked { .. })));
    assert!(sim.take(2).iter().any(|n| matches!(n, ServerBody::MemberLeft { reason: LeaveReason::Kicked, player, .. } if player == "carol")));
    // Leaving and a refreshed token.
    assert!(matches!(
        sim.request(
            2,
            ClientBody::RequestJoinToken {
                session_id: public.id
            }
        ),
        ServerBody::Token {
            kind: TokenKind::Join,
            epoch: 1,
            ..
        }
    ));
    assert_eq!(
        sim.request(
            2,
            ClientBody::LeaveSession {
                session_id: public.id
            }
        ),
        ServerBody::Left {
            session_id: public.id
        }
    );
    assert_eq!(
        expect_error(sim.request(
            2,
            ClientBody::RequestJoinToken {
                session_id: public.id
            }
        )),
        ErrorCode::NotFound,
        "non-members cannot mint tokens"
    );
}

#[test]
fn private_sessions_are_hidden_and_join_by_code_is_throttled() {
    let mut sim = Sim::new(|c| {
        c.limits.join_code_failures_per_peer = 3;
        c.limits.join_code_window_ms = 60_000;
    });
    for (peer, name) in [(1, "host"), (2, "guess"), (3, "friend")] {
        sim.login(peer, name);
    }
    let (private, _) = create(&mut sim, 1, "secret", true, 4);
    let code = private.code.clone().expect("host sees the code");
    // Hidden from browsing, and joining by id is indistinguishable from "no such session".
    match sim.request(
        2,
        ClientBody::ListSessions {
            filter: SessionFilter::default(),
            after: None,
            limit: 10,
        },
    ) {
        ServerBody::SessionList { sessions, .. } => assert!(sessions.is_empty()),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        expect_error(join_by_id(&mut sim, 2, private.id)),
        ErrorCode::NotFound
    );
    assert_eq!(
        expect_error(join_by_id(&mut sim, 2, 9999)),
        ErrorCode::NotFound
    );
    // Three wrong guesses, then even the correct code is refused until the window passes.
    for _ in 0..3 {
        assert_eq!(
            expect_error(join_by_code(&mut sim, 2, "ABCDEFGHJKLMNPQR")),
            ErrorCode::NotFound
        );
    }
    assert_eq!(
        expect_error(join_by_code(&mut sim, 2, &code)),
        ErrorCode::RateLimited
    );
    // Another player is not locked out by the guesser.
    let ServerBody::Joined { session, .. } = join_by_code(&mut sim, 3, &code) else {
        panic!("friend should join");
    };
    assert_eq!(session.members.len(), 2);
    assert!(session.code.is_none(), "only the host sees the code");
    assert!(sim.disconnected.is_empty());
    assert!(sim.service.stats().join_code_failures >= 3);
    // Persisting after the lockout (twice the allowed failures in total) ends the connection.
    assert_eq!(
        expect_error(join_by_code(&mut sim, 2, &code)),
        ErrorCode::RateLimited
    );
    assert!(!sim.disconnected.contains(&2));
    assert_eq!(
        expect_error(join_by_code(&mut sim, 2, &code)),
        ErrorCode::RateLimited
    );
    assert!(sim.disconnected.contains(&2));
    sim.advance(61_000);
    assert!(!sim.disconnected.contains(&3));
}

#[test]
fn request_rate_limit_throttles_then_disconnects() {
    let mut sim = Sim::new(|c| {
        c.limits.request_burst = 5;
        c.limits.requests_per_second = 5;
        c.limits.max_rate_violations = 10;
    });
    sim.login(1, "flood");
    let list = br#"{"v":1,"id":1,"body":{"type":"list_sessions","limit":5}}"#;
    for _ in 0..5 {
        sim.send_raw(1, list);
    }
    let served = sim.take(1).len();
    assert_eq!(served, 5);
    for _ in 0..9 {
        sim.send_raw(1, list);
    }
    let throttled = sim.take(1);
    assert_eq!(throttled.len(), 9);
    assert!(throttled.iter().all(|m| matches!(
        m,
        ServerBody::Error {
            code: ErrorCode::RateLimited,
            ..
        }
    )));
    assert!(!sim.disconnected.contains(&1));
    sim.send_raw(1, list);
    assert!(
        sim.disconnected.contains(&1),
        "persistent abuse disconnects"
    );
    // Time restores service for well-behaved peers.
    sim.login(2, "calm");
    sim.advance(2000);
    assert!(matches!(
        sim.request(
            2,
            ClientBody::ListSessions {
                filter: SessionFilter::default(),
                after: None,
                limit: 5
            }
        ),
        ServerBody::SessionList { .. }
    ));
}

#[test]
fn malformed_and_oversized_messages_after_login_are_answered_then_fatal() {
    let mut sim = Sim::new(|c| c.limits.max_malformed_messages = 4);
    sim.login(1, "alice");
    let (session, _) = create(&mut sim, 1, "room", false, 2);
    for junk in [
        &b"{"[..],
        br#"{"v":1,"id":1,"body":{"type":"create_session","name":"x","mode":"duel","capacity":2,"zzz":true}}"#,
        br#"{"v":1,"id":1,"body":{"type":"kick","session_id":1,"epoch":1,"player":"a\u0000b"}}"#,
    ] {
        sim.send_raw(1, junk);
        assert!(!sim.disconnected.contains(&1));
    }
    let replies = sim.take(1);
    assert_eq!(replies.len(), 3);
    assert!(replies.iter().all(|m| matches!(
        m,
        ServerBody::Error {
            code: ErrorCode::BadRequest,
            ..
        }
    )));
    // Valid traffic still works between strikes.
    assert!(matches!(
        sim.request(
            1,
            ClientBody::RequestJoinToken {
                session_id: session.id
            }
        ),
        ServerBody::Token { .. }
    ));
    sim.send_raw(1, b"\xff\xfe");
    assert!(sim.disconnected.contains(&1), "fourth strike");
    // The session of the dropped peer was cleaned up.
    assert_eq!(sim.service.lobby().session_count(), 0);
}

#[test]
fn per_peer_membership_and_global_session_limits_hold() {
    let mut sim = Sim::new(|c| {
        c.limits.max_sessions = 2;
        c.limits.max_memberships_per_peer = 1;
    });
    for (peer, name) in [(1, "a"), (2, "b"), (3, "c")] {
        sim.login(peer, name);
    }
    let (first, _) = create(&mut sim, 1, "one", false, 2);
    let again = sim.request(
        1,
        ClientBody::CreateSession(CreateSession {
            name: "two".into(),
            mode: "duel".into(),
            capacity: 2,
            private: false,
            attributes: Attributes::new(),
            candidates: None,
        }),
    );
    assert_eq!(
        expect_error(again),
        ErrorCode::LimitExceeded,
        "one session per peer"
    );
    assert_eq!(
        expect_error(join_by_id(&mut sim, 1, first.id)),
        ErrorCode::LimitExceeded
    );
    create(&mut sim, 2, "two", false, 2);
    let third = sim.request(
        3,
        ClientBody::CreateSession(CreateSession {
            name: "three".into(),
            mode: "duel".into(),
            capacity: 2,
            private: false,
            attributes: Attributes::new(),
            candidates: None,
        }),
    );
    assert_eq!(
        expect_error(third),
        ErrorCode::LimitExceeded,
        "server-wide session cap"
    );
    // Out-of-range capacity is rejected by validation, and the error echoes the request ID.
    for capacity in [0u16, 33] {
        let reply = sim.request(
            3,
            ClientBody::CreateSession(CreateSession {
                name: "x".into(),
                mode: "duel".into(),
                capacity,
                private: false,
                attributes: Attributes::new(),
                candidates: None,
            }),
        );
        assert_eq!(expect_error(reply), ErrorCode::BadRequest);
    }
    assert_eq!(sim.service.lobby().session_count(), 2);
}

#[test]
fn matchmaking_forms_a_session_and_issues_cert_bound_tokens() {
    let mut sim = Sim::new(|_| {});
    for (peer, name) in [(1, "dave"), (2, "erin"), (3, "frank")] {
        sim.login(peer, name);
    }
    let queued = |r: ServerBody| match r {
        ServerBody::TicketQueued { ticket } => ticket,
        other => panic!("{other:?}"),
    };
    let t1 = queued(sim.request(1, ticket("duel", 1500)));
    let t2 = queued(sim.request(2, ticket("duel", 1530)));
    let t3 = queued(sim.request(3, ticket("duel", 2900)));
    assert_eq!(
        expect_error(sim.request(1, ticket("duel", 1500))),
        ErrorCode::Conflict,
        "one ticket per peer"
    );
    assert_eq!(
        expect_error(sim.request(3, ClientBody::CancelTicket { ticket: t1 })),
        ErrorCode::NotFound
    );
    sim.advance(100);
    let mut found = Vec::new();
    for peer in [1u64, 2] {
        let notes = sim.take(peer);
        let Some(ServerBody::MatchFound {
            ticket,
            session,
            join_token,
        }) = notes
            .into_iter()
            .find(|n| matches!(n, ServerBody::MatchFound { .. }))
        else {
            panic!("peer {peer} was not matched");
        };
        found.push((peer, ticket, session, join_token));
    }
    assert_eq!([found[0].1, found[1].1], [t1, t2]);
    let session = &found[0].2;
    assert_eq!(session.members.len(), 2);
    assert!(session.private && session.code.is_none());
    let mut teams: Vec<_> = session.members.iter().map(|m| m.team).collect();
    teams.sort();
    assert_eq!(teams, vec![Some(0), Some(1)], "one player per team");
    for (peer, _, view, token) in &found {
        let name = if *peer == 1 { "dave" } else { "erin" };
        assert_eq!(verify_join(&sim, &token.0, name, view.id, 1), Ok(()));
        let other = if *peer == 1 { "erin" } else { "dave" };
        assert_eq!(
            verify_join(&sim, &token.0, other, view.id, 1),
            Err(TokenError::WrongCertificate)
        );
    }
    // The outlier keeps waiting; cancelling works exactly once.
    assert_eq!(sim.service.matchmaker().len(), 1);
    assert_eq!(
        sim.request(3, ClientBody::CancelTicket { ticket: t3 }),
        ServerBody::TicketCancelled { ticket: t3 }
    );
    assert_eq!(
        expect_error(sim.request(3, ClientBody::CancelTicket { ticket: t3 })),
        ErrorCode::NotFound
    );
    // Matched players cannot queue again while in the matched session.
    assert_eq!(
        expect_error(sim.request(1, ticket("duel", 1500))),
        ErrorCode::LimitExceeded
    );
}

#[test]
fn tickets_time_out_validate_modes_and_prefer_directory_ratings() {
    let mut directory = InMemoryDirectory::new(16);
    assert!(directory.set_rating("honest", 1000));
    assert!(directory.set_rating("other", 1010));
    let mut sim = Sim::with_directory(
        |c| {
            for mode in &mut c.modes {
                mode.max_wait_ms = 5000;
            }
        },
        directory,
    );
    sim.login(1, "honest");
    sim.login(2, "other");
    sim.login(3, "lonely");
    assert_eq!(
        expect_error(sim.request(3, ticket("nonexistent", 1))),
        ErrorCode::BadRequest
    );
    // Both claim wildly different ratings; the directory says they are 10 apart.
    assert!(matches!(
        sim.request(1, ticket("duel", 99_000)),
        ServerBody::TicketQueued { .. }
    ));
    assert!(matches!(
        sim.request(2, ticket("duel", 0)),
        ServerBody::TicketQueued { .. }
    ));
    assert!(matches!(
        sim.request(3, ticket("duel", 3000)),
        ServerBody::TicketQueued { .. }
    ));
    sim.advance(100);
    assert!(
        sim.take(1)
            .iter()
            .any(|n| matches!(n, ServerBody::MatchFound { .. }))
    );
    assert!(
        sim.take(2)
            .iter()
            .any(|n| matches!(n, ServerBody::MatchFound { .. }))
    );
    sim.advance(5000);
    assert!(
        sim.take(3)
            .iter()
            .any(|n| matches!(n, ServerBody::TicketExpired { reason, .. } if reason == "timeout"))
    );
    assert!(sim.service.matchmaker().is_empty());
    // A disconnecting player's ticket is dropped.
    assert!(matches!(
        sim.request(3, ticket("duel", 3000)),
        ServerBody::TicketQueued { .. }
    ));
    sim.drop_peer(3);
    assert!(sim.service.matchmaker().is_empty());
}

#[test]
fn entering_a_lobby_session_leaves_the_matchmaking_queue() {
    let mut sim = Sim::new(|_| {});
    sim.login(1, "alice");
    assert!(matches!(
        sim.request(1, ticket("duel", 1500)),
        ServerBody::TicketQueued { .. }
    ));
    create(&mut sim, 1, "room", false, 2);
    assert!(sim.service.matchmaker().is_empty());
    assert!(sim.take(1).iter().any(|n| matches!(
        n,
        ServerBody::TicketExpired { reason, .. } if reason == "joined a session"
    )));
}

#[test]
fn relay_tokens_are_scoped_audience_checked_and_optional() {
    let mut sim = Sim::new(|_| {});
    sim.login(1, "alice");
    let (session, _) = create(&mut sim, 1, "room", false, 2);
    let ServerBody::Token { token, kind, .. } = sim.request(
        1,
        ClientBody::RequestRelayToken {
            session_id: session.id,
        },
    ) else {
        panic!("relay token expected");
    };
    assert_eq!(kind, TokenKind::Relay);
    let relay = sim.auth.verifier();
    let scope = engine_online::Scope::Relay {
        session: session.id,
    };
    let print = Sim::print_for("alice");
    let verify = |audience: &str, print: &_| {
        relay.verify(
            &token.0,
            &engine_online::token::Expect {
                audience,
                scope: &scope,
                cert_sha256: print,
                single_use: false,
            },
            sim.now().unix_secs,
        )
    };
    assert!(verify("engine-relay", &print).is_ok());
    assert_eq!(verify(GAME, &print).unwrap_err(), TokenError::WrongAudience);
    assert_eq!(
        verify("engine-relay", &Sim::print_for("bob")).unwrap_err(),
        TokenError::WrongCertificate
    );
    // Without a configured relay audience the request is refused.
    let mut plain = Sim::new(|c| c.relay_audience = None);
    plain.login(1, "alice");
    let (session, _) = create(&mut plain, 1, "room", false, 2);
    assert_eq!(
        expect_error(plain.request(
            1,
            ClientBody::RequestRelayToken {
                session_id: session.id
            }
        )),
        ErrorCode::Unsupported
    );
}

#[test]
fn host_loss_migrates_by_rtt_with_state_epoch_tokens_and_fencing() {
    let mut sim = Sim::new(|_| {});
    for (peer, name) in [(1, "host"), (2, "slow"), (3, "fast"), (4, "unmeasured")] {
        sim.login(peer, name);
    }
    let (session, old_token) = create(&mut sim, 1, "room", true, 8);
    let code = session.code.clone().unwrap();
    for peer in [2u64, 3, 4] {
        assert!(matches!(
            join_by_code(&mut sim, peer, &code),
            ServerBody::Joined { .. }
        ));
    }
    sim.service.set_rtt(2, 140);
    sim.service.set_rtt(3, 18);
    // The host uploads game state; a member cannot, and a bad epoch is refused.
    assert_eq!(
        sim.request(
            1,
            ClientBody::UploadMigrationState {
                session_id: session.id,
                epoch: 1,
                state: Blob(b"tick=4242".to_vec())
            }
        ),
        ServerBody::Ack {}
    );
    assert_eq!(
        expect_error(sim.request(
            2,
            ClientBody::UploadMigrationState {
                session_id: session.id,
                epoch: 1,
                state: Blob(b"evil".to_vec())
            }
        )),
        ErrorCode::Forbidden
    );
    assert_eq!(
        expect_error(sim.request(
            1,
            ClientBody::UploadMigrationState {
                session_id: session.id,
                epoch: 7,
                state: Blob(vec![])
            }
        )),
        ErrorCode::StaleEpoch
    );
    for peer in 1..=4 {
        sim.take(peer);
    }

    sim.drop_peer(1); // the host vanishes
    let mut new_host_state = None;
    for peer in [2u64, 3, 4] {
        let notes = sim.take(peer);
        let changed = notes
            .iter()
            .find_map(|n| match n {
                ServerBody::HostChanged {
                    epoch,
                    host,
                    you_are_host,
                    join_token,
                    state,
                    code,
                    candidates,
                    ..
                } => Some((
                    *epoch,
                    host.clone(),
                    *you_are_host,
                    join_token.clone(),
                    state.clone(),
                    code.clone(),
                    candidates.clone(),
                )),
                _ => None,
            })
            .unwrap_or_else(|| panic!("peer {peer} missed HostChanged"));
        assert_eq!(
            (changed.0, changed.1.as_str()),
            (2, "fast"),
            "lowest RTT wins"
        );
        assert_eq!(changed.2, peer == 3);
        assert_eq!(
            changed.6,
            Some(Blob(vec![3; 8])),
            "new host candidates are forwarded"
        );
        let name = ["", "", "slow", "fast", "unmeasured"][peer as usize];
        // Fresh token for the new epoch; the epoch-1 token no longer opens the new host's server.
        let token = changed.3.expect("join token for the new epoch");
        assert_eq!(verify_join(&sim, &token.0, name, session.id, 2), Ok(()));
        assert_eq!(
            verify_join(&sim, &old_token, "host", session.id, 2),
            Err(TokenError::MissingScope)
        );
        assert_eq!(
            changed.4.is_some(),
            peer == 3,
            "only the new host gets the state"
        );
        assert_eq!(
            changed.5.is_some(),
            peer == 3,
            "only the new host gets the join code"
        );
        if peer == 3 {
            new_host_state = changed.4;
        }
    }
    assert_eq!(new_host_state, Some(Blob(b"tick=4242".to_vec())));
    assert_eq!(sim.service.lobby().epoch(session.id), Some(2));
    // The fenced epoch: a member holding epoch 1 knowledge cannot act as host.
    assert_eq!(
        expect_error(sim.request(
            2,
            ClientBody::Kick {
                session_id: session.id,
                epoch: 1,
                player: "fast".into()
            }
        )),
        ErrorCode::StaleEpoch
    );
    assert_eq!(
        expect_error(sim.request(
            2,
            ClientBody::Kick {
                session_id: session.id,
                epoch: 2,
                player: "fast".into()
            }
        )),
        ErrorCode::Forbidden
    );
    assert_eq!(
        sim.request(
            3,
            ClientBody::Kick {
                session_id: session.id,
                epoch: 2,
                player: "unmeasured".into()
            }
        ),
        ServerBody::Ack {}
    );
}

#[test]
fn forced_migration_deposes_a_live_host_and_fences_it() {
    let mut sim = Sim::new(|_| {});
    sim.login(1, "old");
    sim.login(2, "new");
    let (session, _) = create(&mut sim, 1, "room", false, 4);
    join_by_id(&mut sim, 2, session.id);
    sim.service.set_rtt(1, 5);
    sim.service.set_rtt(2, 50);
    sim.service.force_migrate(session.id, sim.now()).unwrap();
    sim.pump();
    assert_eq!(sim.service.lobby().host(session.id), Some(2));
    // The deposed host is told too (you_are_host = false) and holds a stale epoch.
    assert!(sim.take(1).iter().any(|n| matches!(
        n,
        ServerBody::HostChanged {
            you_are_host: false,
            epoch: 2,
            ..
        }
    )));
    assert_eq!(
        expect_error(sim.request(
            1,
            ClientBody::SetAttributes {
                session_id: session.id,
                epoch: 1,
                attributes: Attributes::new()
            }
        )),
        ErrorCode::StaleEpoch
    );
    assert_eq!(
        expect_error(sim.request(
            1,
            ClientBody::Kick {
                session_id: session.id,
                epoch: 2,
                player: "new".into()
            }
        )),
        ErrorCode::Forbidden
    );
}

#[test]
fn teardown_leaves_no_residual_state() {
    let mut sim = Sim::new(|_| {});
    for peer in 1..=6u64 {
        sim.login(peer, &format!("p{peer}"));
    }
    let (a, _) = create(&mut sim, 1, "a", false, 4);
    let (b, _) = create(&mut sim, 2, "b", true, 4);
    join_by_id(&mut sim, 3, a.id);
    join_by_code(&mut sim, 4, b.code.as_deref().unwrap());
    sim.request(5, ticket("duel", 100));
    for peer in 1..=6u64 {
        sim.drop_peer(peer);
    }
    assert_eq!(sim.service.lobby().session_count(), 0);
    assert!(sim.service.matchmaker().is_empty());
    assert_eq!(sim.service.authenticated_peers(), 0);
    assert_eq!(sim.service.tracked_peers(), 0);
    sim.service.lobby().check_invariants().unwrap();
}
