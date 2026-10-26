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

fn relay_token(sim: &mut Sim, peer: u64, session: u64, with: &str) -> ServerBody {
    sim.request(
        peer,
        ClientBody::RequestRelayToken {
            session_id: session,
            peer: with.into(),
        },
    )
}
fn token_bytes(body: ServerBody) -> Vec<u8> {
    match body {
        ServerBody::Token {
            token,
            kind: TokenKind::Relay,
            ..
        } => token.0,
        other => panic!("relay token expected, got {other:?}"),
    }
}

/// M4: a session-wide relay token named no channel or party, so any member could squat either
/// slot of any pair. Credentials now name exactly one pair and one slot.
#[test]
fn relay_tokens_are_pair_scoped_audience_checked_and_optional() {
    use engine_online::{RelayParty, relay::relay_channel};
    let mut sim = Sim::new(|_| {});
    for (peer, name) in [(1, "alice"), (2, "bob"), (3, "carol"), (4, "dave")] {
        sim.login(peer, name);
    }
    let (session, _) = create(&mut sim, 1, "room", false, 4);
    for peer in [2u64, 3] {
        assert!(matches!(
            join_by_id(&mut sim, peer, session.id),
            ServerBody::Joined { .. }
        ));
    }
    let (other_session, _) = {
        sim.login(5, "erin");
        create(&mut sim, 5, "elsewhere", false, 2)
    };
    let verifier = sim.auth.verifier();
    let now = sim.now().unix_secs;
    let claims = |token: &[u8]| verifier.verify_relay(token, "engine-relay", now).unwrap();

    let alice_bob = token_bytes(relay_token(&mut sim, 1, session.id, "bob"));
    let bob_alice = token_bytes(relay_token(&mut sim, 2, session.id, "alice"));
    let carol_alice = token_bytes(relay_token(&mut sim, 3, session.id, "alice"));
    let carol_bob = token_bytes(relay_token(&mut sim, 3, session.id, "bob"));
    let channel = relay_channel(session.id, "alice", "bob");
    let (a, b) = (claims(&alice_bob), claims(&bob_alice));
    assert_eq!(
        (a.channel, a.party, a.subject.as_str()),
        (channel, RelayParty::A, "alice")
    );
    assert_eq!(
        (b.channel, b.party, b.subject.as_str()),
        (channel, RelayParty::B, "bob")
    );
    assert_eq!((a.session, a.expires_at), (session.id, now + 3600));
    // A third member can only ever obtain credentials for channels it is party to.
    let (ca, cb) = (claims(&carol_alice), claims(&carol_bob));
    assert_ne!(ca.channel, channel);
    assert_ne!(cb.channel, channel);
    assert_ne!(ca.channel, cb.channel);
    assert_eq!(ca.channel, relay_channel(session.id, "carol", "alice"));
    // The scope in the signed token is exactly the pair's, nothing broader.
    let scope = engine_online::Scope::Relay {
        session: session.id,
        channel,
        party: RelayParty::A,
    };
    let verify = |audience: &str, print: &_, scope: &_| {
        verifier.verify(
            &alice_bob,
            &engine_online::token::Expect {
                audience,
                scope,
                cert_sha256: print,
                single_use: false,
            },
            now,
        )
    };
    let alice_print = Sim::print_for("alice");
    assert!(verify("engine-relay", &alice_print, &scope).is_ok());
    assert_eq!(
        verify(GAME, &alice_print, &scope).unwrap_err(),
        TokenError::WrongAudience
    );
    assert_eq!(
        verify("engine-relay", &Sim::print_for("bob"), &scope).unwrap_err(),
        TokenError::WrongCertificate
    );
    let as_party_b = engine_online::Scope::Relay {
        session: session.id,
        channel,
        party: RelayParty::B,
    };
    assert_eq!(
        verify("engine-relay", &alice_print, &as_party_b).unwrap_err(),
        TokenError::MissingScope,
        "alice's credential is not valid for bob's slot"
    );
    assert_eq!(
        verifier.verify_relay(&alice_bob, GAME, now).unwrap_err(),
        TokenError::WrongAudience
    );

    // Requests that do not name a member of the requester's own session are refused.
    for (peer, with, session_id, expected) in [
        (1, "nobody", session.id, ErrorCode::NotFound),
        (1, "erin", session.id, ErrorCode::NotFound),
        (1, "alice", session.id, ErrorCode::BadRequest),
        (4, "alice", session.id, ErrorCode::NotFound),
        (1, "bob", other_session.id, ErrorCode::NotFound),
        (1, "bob", 424_242, ErrorCode::NotFound),
    ] {
        assert_eq!(
            expect_error(relay_token(&mut sim, peer, session_id, with)),
            expected,
            "{peer} -> {with} in {session_id}"
        );
    }
    // Without a configured relay audience the request is refused.
    let mut plain = Sim::new(|c| c.relay_audience = None);
    plain.login(1, "alice");
    plain.login(2, "bob");
    let (session, _) = create(&mut plain, 1, "room", false, 2);
    join_by_id(&mut plain, 2, session.id);
    assert_eq!(
        expect_error(relay_token(&mut plain, 1, session.id, "bob")),
        ErrorCode::Unsupported
    );
}

/// L2: nothing distributed the punch secret except the broadcast candidates blob. Now the
/// server derives a secret per pair and hands it to the two members only.
#[test]
fn a_third_member_never_receives_a_pairs_punch_secret() {
    let mut sim = Sim::new(|_| {});
    for (peer, name) in [(1, "alice"), (2, "bob"), (3, "carol")] {
        sim.login(peer, name);
    }
    let (session, _) = create(&mut sim, 1, "room", false, 4);
    for peer in [2u64, 3] {
        join_by_id(&mut sim, peer, session.id);
    }
    let intro = |sim: &mut Sim, peer: u64, with: &str| match sim.request(
        peer,
        ClientBody::RequestPeerIntro {
            session_id: session.id,
            peer: with.into(),
        },
    ) {
        ServerBody::PeerIntro {
            peer,
            candidates,
            punch_secret,
            controlling,
            ..
        } => (peer, candidates, punch_secret.0, controlling),
        other => panic!("peer intro expected, got {other:?}"),
    };
    for peer in 1..=3 {
        sim.take(peer);
    }
    let (peer_of_alice, bob_candidates, alice_secret, alice_controls) = intro(&mut sim, 1, "bob");
    let (peer_of_bob, alice_candidates, bob_secret, bob_controls) = intro(&mut sim, 2, "alice");
    assert_eq!(
        (peer_of_alice.as_str(), peer_of_bob.as_str()),
        ("bob", "alice")
    );
    assert_eq!(alice_secret.len(), 32);
    assert_eq!(alice_secret, bob_secret, "the pair shares one secret");
    assert_ne!(alice_controls, bob_controls, "and takes opposite roles");
    assert_eq!(
        bob_candidates,
        Some(Blob(vec![2; 8])),
        "the peer's candidates ride along"
    );
    assert_eq!(alice_candidates, Some(Blob(vec![1; 8])));
    // Carol is a member too, but everything she can obtain involves herself, so it differs.
    let (_, _, carol_with_alice, _) = intro(&mut sim, 3, "alice");
    let (_, _, carol_with_bob, _) = intro(&mut sim, 3, "bob");
    assert_ne!(carol_with_alice, alice_secret);
    assert_ne!(carol_with_bob, alice_secret);
    assert_ne!(carol_with_alice, carol_with_bob);
    // The secret travels in the responses to the two members only: nothing was broadcast, and
    // the third member's inbox holds no copy of it.
    for note in sim.take(3) {
        let text = format!("{note:?}");
        assert!(!text.contains("punch_secret"), "{text}");
    }
    assert!(sim.take(1).is_empty() && sim.take(2).is_empty());
    // Strangers and non-members get nothing.
    for (peer, with, expected) in [
        (1, "nobody", ErrorCode::NotFound),
        (1, "alice", ErrorCode::BadRequest),
    ] {
        assert_eq!(
            expect_error(sim.request(
                peer,
                ClientBody::RequestPeerIntro {
                    session_id: session.id,
                    peer: with.into()
                }
            )),
            expected
        );
    }
    sim.login(4, "dave");
    assert_eq!(
        expect_error(sim.request(
            4,
            ClientBody::RequestPeerIntro {
                session_id: session.id,
                peer: "alice".into()
            }
        )),
        ErrorCode::NotFound
    );
}

/// L2: joiners had no way to learn which certificate the host's game server presents.
#[test]
fn the_hosts_game_certificate_fingerprint_reaches_joiners_and_follows_migration() {
    let mut sim = Sim::new(|_| {});
    for (peer, name) in [(1, "alice"), (2, "bob"), (3, "carol")] {
        sim.login(peer, name);
    }
    let (session, _) = create(&mut sim, 1, "room", false, 4);
    assert_eq!(session.host_fingerprint, None, "nothing registered yet");
    let register = |sim: &mut Sim, peer: u64, bytes: Vec<u8>| {
        sim.request(
            peer,
            ClientBody::SetGameIdentity {
                session_id: session.id,
                cert_sha256: Blob(bytes),
            },
        )
    };
    assert_eq!(register(&mut sim, 1, vec![0xA1; 32]), ServerBody::Ack {});
    let ServerBody::Joined { session: view, .. } = join_by_id(&mut sim, 2, session.id) else {
        panic!("join failed");
    };
    assert_eq!(view.host_fingerprint, Some(Blob(vec![0xA1; 32])));
    join_by_id(&mut sim, 3, session.id);
    // Bob and carol register what their servers would present if they were elected.
    assert_eq!(register(&mut sim, 2, vec![0xB2; 32]), ServerBody::Ack {});
    assert_eq!(register(&mut sim, 3, vec![0xC3; 32]), ServerBody::Ack {});
    // The wrong size is refused at the protocol boundary, and outsiders cannot register.
    sim.login(4, "dave");
    assert_eq!(
        expect_error(register(&mut sim, 4, vec![1; 32])),
        ErrorCode::NotFound
    );
    let payload = format!(
        r#"{{"v":1,"id":9,"body":{{"type":"set_game_identity","session_id":{},"cert_sha256":"AAAA"}}}}"#,
        session.id
    );
    sim.send_raw(1, payload.as_bytes());
    assert!(matches!(
        sim.take(1).last(),
        Some(ServerBody::Error {
            code: ErrorCode::BadRequest,
            ..
        })
    ));
    // The host changes its certificate: everybody else is told.
    sim.take(2);
    sim.take(3);
    assert_eq!(register(&mut sim, 1, vec![0xA2; 32]), ServerBody::Ack {});
    for peer in [2u64, 3] {
        assert!(sim.take(peer).iter().any(|n| matches!(
            n,
            ServerBody::HostIdentityChanged { host_fingerprint, .. }
                if *host_fingerprint == Blob(vec![0xA2; 32])
        )));
    }
    // The host leaves: HostChanged carries the new host's fingerprint, never the old host's.
    sim.service.set_rtt(2, 10);
    sim.service.set_rtt(3, 90);
    sim.drop_peer(1);
    for (peer, expected) in [(2u64, vec![0xB2; 32]), (3, vec![0xB2; 32])] {
        let changed = sim
            .take(peer)
            .into_iter()
            .find_map(|n| match n {
                ServerBody::HostChanged {
                    host,
                    host_fingerprint,
                    ..
                } => Some((host, host_fingerprint)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("peer {peer} missed HostChanged"));
        assert_eq!(changed.0, "bob");
        assert_eq!(changed.1, Some(Blob(expected)));
    }
    // A joiner arriving now sees the new host's pin in the session view.
    sim.login(5, "erin");
    let ServerBody::Joined { session: view, .. } = join_by_id(&mut sim, 5, session.id) else {
        panic!("join failed");
    };
    assert_eq!(view.host.as_deref(), Some("bob"));
    assert_eq!(view.host_fingerprint, Some(Blob(vec![0xB2; 32])));
}

/// L3: the host can rotate the private code on demand, and every host change rotates it.
#[test]
fn rotating_the_join_code_invalidates_the_old_one() {
    let mut sim = Sim::new(|_| {});
    for (peer, name) in [(1, "host"), (2, "friend"), (3, "latecomer")] {
        sim.login(peer, name);
    }
    let (private, _) = create(&mut sim, 1, "secret", true, 4);
    let old = private.code.clone().unwrap();
    assert!(matches!(
        join_by_code(&mut sim, 2, &old),
        ServerBody::Joined { .. }
    ));
    // A member cannot rotate it; a stale epoch is told so.
    for (peer, epoch, expected) in [
        (2, 1, ErrorCode::Forbidden),
        (1, 9, ErrorCode::StaleEpoch),
        (3, 1, ErrorCode::NotFound),
    ] {
        assert_eq!(
            expect_error(sim.request(
                peer,
                ClientBody::RotateCode {
                    session_id: private.id,
                    epoch
                }
            )),
            expected
        );
    }
    let ServerBody::CodeRotated { code: new, .. } = sim.request(
        1,
        ClientBody::RotateCode {
            session_id: private.id,
            epoch: 1,
        },
    ) else {
        panic!("rotation failed");
    };
    assert_ne!(new, old);
    assert_eq!(
        expect_error(join_by_code(&mut sim, 3, &old)),
        ErrorCode::NotFound,
        "the old code is dead"
    );
    assert!(matches!(
        join_by_code(&mut sim, 3, &new),
        ServerBody::Joined { .. }
    ));
    // Kicking tells the host the new code; the kicked member is refused either way.
    sim.take(1);
    assert_eq!(
        sim.request(
            1,
            ClientBody::Kick {
                session_id: private.id,
                epoch: 1,
                player: "friend".into()
            }
        ),
        ServerBody::Ack {}
    );
    let kicked_note = sim.take(1).into_iter().find_map(|n| match n {
        ServerBody::CodeChanged { code, .. } => Some(code),
        _ => None,
    });
    let after_kick = kicked_note.expect("the host is told the rotated code");
    assert_ne!(after_kick, new);
    assert_eq!(
        expect_error(join_by_code(&mut sim, 2, &after_kick)),
        ErrorCode::Forbidden
    );
    // A public session has no code to rotate.
    sim.login(4, "other");
    let (public, _) = create(&mut sim, 4, "open", false, 2);
    assert_eq!(
        expect_error(sim.request(
            4,
            ClientBody::RotateCode {
                session_id: public.id,
                epoch: 1
            }
        )),
        ErrorCode::BadRequest
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

// ---------------------------------------------------------------------------------------------
// Security repairs (findings M1, M2, L3)
// ---------------------------------------------------------------------------------------------

/// Sends a join-by-code request and discards whatever comes back (the peer may already have
/// been dropped for persistent guessing).
fn guess_code(sim: &mut Sim, peer: u64, code: &str) {
    let payload = engine_online::ClientEnvelope {
        v: engine_online::protocol::PROTOCOL_VERSION,
        id: 77,
        body: ClientBody::JoinSession {
            target: JoinTarget::Code { code: code.into() },
            candidates: None,
        },
    }
    .encode(&sim.limits)
    .unwrap();
    sim.send_raw(peer, &payload);
    sim.take(peer);
}

/// M1: session IDs were sequential from 1 with epoch 1 everywhere, so a join token from a
/// previous run or a sibling instance named an ID/epoch that existed again and admitted a
/// non-member.
#[test]
fn join_token_from_a_previous_or_sibling_instance_admits_a_non_member() {
    let mut sim = Sim::new(|_| {});
    sim.login(1, "alice");
    let (_alice_session, alice_token) = create(&mut sim, 1, "alice-room", true, 4);
    // A restart (or a sibling instance sharing the signing key) starts again.
    sim.service = service(&sim.auth, |_| {});
    sim.login(2, "bob");
    let (bob, _) = create(&mut sim, 2, "bob-private", true, 4);
    let r = sim.auth.verifier().verify_join(
        &alice_token,
        GAME,
        bob.id,
        bob.epoch,
        b"sim-cert-alice",
        sim.now().unix_secs,
    );
    assert!(r.is_err(), "alice's old token must not open bob's session");
}

#[test]
fn session_ids_are_random_nonzero_and_json_safe() {
    let mut sim = Sim::new(|c| c.limits.max_sessions = 64);
    let mut ids = std::collections::BTreeSet::new();
    for peer in 1..=40u64 {
        sim.login(peer, &format!("p{peer}"));
        let (session, _) = create(&mut sim, peer, "r", false, 2);
        assert!(session.id != 0);
        assert!(
            session.id < (1 << 53),
            "ids stay exactly representable as JSON numbers"
        );
        assert!(ids.insert(session.id));
    }
    let sequential = ids.iter().zip(ids.iter().skip(1)).all(|(a, b)| b - a == 1);
    assert!(!sequential && *ids.iter().max().unwrap() > 1000);
}

/// M2: the per-connection guess window reset on reconnect and a global window of failures
/// blocked everyone, so one account reconnecting could deny every private-code join.
#[test]
fn a_reconnecting_guesser_cannot_lock_everyone_out_of_private_code_joins() {
    let mut sim = Sim::new(|_| {});
    sim.login(1, "host");
    sim.login(2, "alice");
    let (private, _) = create(&mut sim, 1, "secret", true, 4);
    let code = private.code.clone().expect("host sees the code");
    let mut peer = 100;
    for _ in 0..20 {
        sim.login(peer, "mallory");
        for _ in 0..5 {
            guess_code(&mut sim, peer, "ABCDEFGHJKLMNPQR");
        }
        peer += 1;
    }
    let stats = sim.service.stats().clone();
    assert_eq!(
        stats.join_code_failures, 5,
        "only the first window's guesses reached the comparison; every reconnect stays locked out"
    );
    assert!(stats.rate_limited >= 20, "{stats:?}");
    assert_eq!(
        stats.join_code_alarms, 0,
        "the global count is telemetry only"
    );
    let ServerBody::Joined { session, .. } = join_by_code(&mut sim, 2, &code) else {
        panic!("alice must still be able to join with the correct code");
    };
    assert_eq!(session.members.len(), 2);
    // Mallory, though, stays locked out across reconnects even with the right code.
    sim.login(peer, "mallory");
    assert_eq!(
        expect_error(join_by_code(&mut sim, peer, &code)),
        ErrorCode::RateLimited
    );
    // Once the window passes she may try again.
    sim.advance(61_000);
    sim.login(peer + 1, "mallory");
    assert!(matches!(
        join_by_code(&mut sim, peer + 1, &code),
        ServerBody::Joined { .. }
    ));
}

#[test]
fn the_global_guess_count_raises_an_alarm_but_blocks_nobody() {
    let mut sim = Sim::new(|c| {
        c.limits.join_code_failures_global = 3;
        c.limits.join_code_failures_per_peer = 100;
    });
    sim.login(1, "host");
    sim.login(2, "alice");
    sim.login(3, "mallory");
    let (private, _) = create(&mut sim, 1, "secret", true, 4);
    let code = private.code.clone().unwrap();
    for _ in 0..5 {
        guess_code(&mut sim, 3, "ABCDEFGHJKLMNPQR");
    }
    let stats = sim.service.stats();
    assert_eq!((stats.join_code_failures, stats.join_code_alarms), (5, 1));
    assert!(matches!(
        join_by_code(&mut sim, 2, &code),
        ServerBody::Joined { .. }
    ));
}

#[test]
fn guess_tracking_is_bounded_even_with_many_subjects() {
    let mut sim = Sim::new(|c| {
        c.limits.max_peers = 16;
        c.limits.max_tracked_guessers = 8;
        c.limits.requests_per_second = 1_000_000;
        c.limits.request_burst = 1_000_000;
    });
    sim.login(1, "host");
    let (private, _) = create(&mut sim, 1, "secret", true, 4);
    let code = private.code.clone().unwrap();
    // Far more distinct guessing subjects than the table holds: the service stays bounded and
    // a fresh honest subject still gets through.
    for n in 0..500u64 {
        let peer = 1000 + n;
        sim.login(peer, &format!("guesser{n}"));
        guess_code(&mut sim, peer, "ABCDEFGHJKLMNPQR");
        sim.drop_peer(peer);
        assert!(sim.service.tracked_code_guessers() <= 8);
    }
    assert_eq!(sim.service.tracked_code_guessers(), 8);
    sim.login(9, "honest");
    assert!(matches!(
        join_by_code(&mut sim, 9, &code),
        ServerBody::Joined { .. }
    ));
}

/// L3: kicking was not sticky and join codes never rotated, so a kicked member walked straight
/// back in.
#[test]
fn a_kicked_member_cannot_rejoin_and_the_old_code_stops_working() {
    let mut sim = Sim::new(|_| {});
    for (peer, name) in [(1, "host"), (2, "bob"), (3, "carol")] {
        sim.login(peer, name);
    }
    // Public session: the kick is sticky.
    let (public, _) = create(&mut sim, 1, "open", false, 4);
    assert!(matches!(
        join_by_id(&mut sim, 2, public.id),
        ServerBody::Joined { .. }
    ));
    assert_eq!(
        sim.request(
            1,
            ClientBody::Kick {
                session_id: public.id,
                epoch: 1,
                player: "bob".into()
            }
        ),
        ServerBody::Ack {}
    );
    assert_eq!(
        expect_error(join_by_id(&mut sim, 2, public.id)),
        ErrorCode::Forbidden,
        "kicked members stay out"
    );
    // A different member is unaffected.
    assert!(matches!(
        join_by_id(&mut sim, 3, public.id),
        ServerBody::Joined { .. }
    ));
    sim.request(
        3,
        ClientBody::LeaveSession {
            session_id: public.id,
        },
    );
    sim.request(
        1,
        ClientBody::LeaveSession {
            session_id: public.id,
        },
    );

    // Private session: kicking also rotates the code, so the kicked member's copy is dead.
    sim.login(4, "dave");
    let (private, _) = create(&mut sim, 1, "secret", true, 4);
    let old_code = private.code.clone().unwrap();
    assert!(matches!(
        join_by_code(&mut sim, 4, &old_code),
        ServerBody::Joined { .. }
    ));
    sim.take(1);
    assert_eq!(
        sim.request(
            1,
            ClientBody::Kick {
                session_id: private.id,
                epoch: 1,
                player: "dave".into()
            }
        ),
        ServerBody::Ack {}
    );
    assert!(
        !matches!(
            join_by_code(&mut sim, 4, &old_code),
            ServerBody::Joined { .. }
        ),
        "dave cannot come back with the code he already had"
    );
    assert!(
        !matches!(
            join_by_code(&mut sim, 3, &old_code),
            ServerBody::Joined { .. }
        ),
        "the old code is dead for everybody"
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

#[test]
fn late_hello_is_refused_even_before_the_next_tick() {
    for lateness in [0, 1] {
        let mut sim = Sim::new(|cfg| cfg.limits.auth_timeout_ms = 100);
        sim.admit(1, "alice");
        let token = sim.connect_token("alice", "alice", 600);
        sim.mono = 100 + lateness; // Deliberately do not tick before delivering the message.
        let payload = engine_online::ClientEnvelope {
            v: 1,
            id: 1,
            body: ClientBody::Hello { token: Blob(token) },
        }
        .encode(&sim.limits)
        .unwrap();
        sim.send_raw(1, &payload);
        assert!(sim.disconnected.contains(&1), "late hello was admitted");
        assert_eq!(sim.service.authenticated_peers(), 0);
        assert!(sim.take(1).is_empty(), "expired peers receive no service");
    }
}

#[test]
fn expired_identity_cannot_request_or_refresh_before_the_next_tick() {
    for refresh in [false, true] {
        let mut sim = Sim::new(|_| {});
        sim.admit(1, "alice");
        let token = sim.connect_token("alice", "alice", 100);
        assert!(matches!(
            sim.request(1, ClientBody::Hello { token: Blob(token) }),
            ServerBody::HelloOk { .. }
        ));
        sim.mono = 100_000; // Token has expired, but the periodic tick has not run.
        let body = if refresh {
            ClientBody::Hello {
                token: Blob(sim.connect_token("alice", "alice", 600)),
            }
        } else {
            ClientBody::CreateSession(CreateSession {
                name: "too late".into(),
                mode: "duel".into(),
                capacity: 2,
                private: false,
                attributes: Attributes::new(),
                candidates: None,
            })
        };
        let payload = engine_online::ClientEnvelope { v: 1, id: 2, body }
            .encode(&sim.limits)
            .unwrap();
        sim.send_raw(1, &payload);
        assert!(sim.disconnected.contains(&1), "expired peer was served");
        assert_eq!(sim.service.authenticated_peers(), 0);
        assert_eq!(sim.service.stats().sessions_created, 0);
        assert!(sim.take(1).is_empty(), "expired peers receive no service");
    }
}
