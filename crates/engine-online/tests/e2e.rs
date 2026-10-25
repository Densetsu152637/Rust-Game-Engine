//! End-to-end behaviour over real loopback sockets (127.0.0.1:0) with synthetic in-memory
//! identities: mutual-TLS QUIC and TLS-over-TCP, token auth, lobby, matchmaking, migration.
mod common;

use common::*;
use engine_network::{
    Client as TcpClient, ClientConfig as TcpClientConfig, ClientSecurity, Delivery, Lane,
    MessageEndpoint, SERVER_PEER, Server as TcpServer, ServerConfig as TcpServerConfig,
    ServerSecurity,
};
use engine_online::{
    CertificateBinding, ClientBody, ClientEvent, OnlineClient, OnlineServer, ServerBody,
    TokenError,
    protocol::{Attributes, Blob, CreateSession, ErrorCode, JoinTarget, SessionFilter, TicketSpec},
};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    thread,
    time::{Duration, Instant},
};

const GAME: &str = "engine-game";

fn game_verify(
    net: &Net,
    token: &[u8],
    session: u64,
    epoch: u64,
    client: usize,
) -> Result<(), TokenError> {
    net.auth
        .verifier()
        .verify_join(
            token,
            GAME,
            session,
            epoch,
            net.ids[client].cert.as_ref(),
            Net::unix_now(),
        )
        .map(|_| ())
}
fn create_body(name: &str, private: bool, capacity: u16) -> ClientBody {
    ClientBody::CreateSession(CreateSession {
        name: name.into(),
        mode: "duel".into(),
        capacity,
        private,
        attributes: Attributes::from([("map".into(), "dust".into())]),
        candidates: Some(Blob(b"candidate-blob".to_vec())),
    })
}
fn ticket(rating: i32) -> ClientBody {
    ClientBody::EnqueueTicket(TicketSpec {
        mode: "duel".into(),
        region: "eu".into(),
        rating,
        party_size: 1,
        attributes: Attributes::new(),
        latencies: BTreeMap::new(),
        candidates: None,
    })
}
fn world(clients: usize) -> (Net, Vec<Conn>) {
    let mut net = Net::new(clients, |_| {});
    let mut conns: Vec<Conn> = (0..clients).map(|i| net.connect(i)).collect();
    net.connect_all(&mut conns);
    for i in 0..clients {
        match net.login(&mut conns, i) {
            ServerBody::HelloOk { subject, .. } => assert_eq!(subject, format!("player{i}")),
            other => panic!("login {i}: {other:?}"),
        }
    }
    (net, conns)
}

#[test]
fn full_journey_authenticate_browse_join_match_and_game_server_verification() {
    let (mut net, mut c) = world(6);

    // Private session created by player0; its join token is bound to player0's certificate.
    let ServerBody::SessionCreated {
        session: private,
        join_token,
    } = net.call(&mut c, 0, create_body("secret", true, 4))
    else {
        panic!("create failed");
    };
    let code = private.code.clone().expect("host sees the join code");
    assert_eq!(
        game_verify(&net, &join_token.0, private.id, 1, 1),
        Err(TokenError::WrongCertificate),
        "another player's certificate cannot use it"
    );
    let game = net.auth.verifier();
    let ok = game.verify_join(
        &join_token.0,
        GAME,
        private.id,
        1,
        net.ids[0].cert.as_ref(),
        Net::unix_now(),
    );
    assert!(ok.is_ok(), "{ok:?}");
    let replay = game.verify_join(
        &join_token.0,
        GAME,
        private.id,
        1,
        net.ids[0].cert.as_ref(),
        Net::unix_now(),
    );
    assert_eq!(replay.unwrap_err(), TokenError::Replayed);

    // A public session is browsable; the private one is not.
    assert!(matches!(
        net.call(&mut c, 2, create_body("open", false, 3)),
        ServerBody::SessionCreated { .. }
    ));
    let ServerBody::SessionList { sessions, next } = net.call(
        &mut c,
        3,
        ClientBody::ListSessions {
            filter: SessionFilter::default(),
            after: None,
            limit: 10,
        },
    ) else {
        panic!("list failed");
    };
    assert_eq!((sessions.len(), next), (1, None));
    assert_eq!(sessions[0].name, "open");
    assert_eq!(sessions[0].host.as_deref(), Some("player2"));

    // Join the private session with its code; the host is notified over the wire.
    let ServerBody::Joined {
        session,
        join_token,
    } = net.call(
        &mut c,
        1,
        ClientBody::JoinSession {
            target: JoinTarget::Code { code },
            candidates: Some(Blob(b"bob-candidates".to_vec())),
        },
    )
    else {
        panic!("join by code failed");
    };
    assert_eq!(session.members.len(), 2);
    assert!(game_verify(&net, &join_token.0, private.id, 1, 1).is_ok());
    net.drive(&mut c, |conns| {
        conns[0].events.iter().any(|e| {
            matches!(e, ClientEvent::Notification(ServerBody::MemberJoined { member, .. })
                if member.player == "player1" && member.candidates == Some(Blob(b"bob-candidates".to_vec())))
        })
    });

    // A non-host cannot kick; the host can.
    let reply = net.call(
        &mut c,
        1,
        ClientBody::Kick {
            session_id: private.id,
            epoch: 1,
            player: "player0".into(),
        },
    );
    assert!(matches!(
        reply,
        ServerBody::Error {
            code: ErrorCode::Forbidden,
            ..
        }
    ));
    assert_eq!(
        net.call(
            &mut c,
            0,
            ClientBody::Kick {
                session_id: private.id,
                epoch: 1,
                player: "player1".into()
            }
        ),
        ServerBody::Ack {}
    );

    // Match two tickets: both receive a session and their own cert-bound join token.
    assert!(matches!(
        net.call(&mut c, 4, ticket(1500)),
        ServerBody::TicketQueued { .. }
    ));
    assert!(matches!(
        net.call(&mut c, 5, ticket(1520)),
        ServerBody::TicketQueued { .. }
    ));
    net.drive(&mut c, |conns| {
        [4, 5].iter().all(|&i| {
            conns[i]
                .events
                .iter()
                .any(|e| matches!(e, ClientEvent::Notification(ServerBody::MatchFound { .. })))
        })
    });
    let mut matched = Vec::new();
    for i in [4usize, 5] {
        let found = c[i].take_notifications().into_iter().find_map(|n| match n {
            ServerBody::MatchFound {
                session,
                join_token,
                ..
            } => Some((session, join_token)),
            _ => None,
        });
        matched.push(found.expect("match notification"));
    }
    assert_eq!(matched[0].0.id, matched[1].0.id);
    assert_eq!(matched[0].0.members.len(), 2);
    for (k, i) in [4usize, 5].into_iter().enumerate() {
        let (session, token) = &matched[k];
        assert!(game_verify(&net, &token.0, session.id, 1, i).is_ok());
        let other = 9 - i;
        assert_eq!(
            game_verify(&net, &token.0, session.id, 1, other),
            Err(TokenError::WrongCertificate)
        );
    }

    // Relay credentials are scoped tokens too.
    let ServerBody::Token { token, .. } = net.call(
        &mut c,
        0,
        ClientBody::RequestRelayToken {
            session_id: private.id,
        },
    ) else {
        panic!("relay token");
    };
    let relay = net.auth.verifier();
    let scope = engine_online::Scope::Relay {
        session: private.id,
    };
    assert!(
        relay
            .verify(
                &token.0,
                &engine_online::token::Expect {
                    audience: "engine-relay",
                    scope: &scope,
                    cert_sha256: &net.ids[0].fingerprint(),
                    single_use: true,
                },
                Net::unix_now()
            )
            .is_ok()
    );
    assert_eq!(net.server.service().stats().auth_failures, 0);
}

#[test]
fn no_token_before_the_deadline_means_disconnect_and_no_service() {
    let mut net = Net::new(2, |c| c.limits.auth_timeout_ms = 300);
    let mut conns = vec![net.connect(0), net.connect(1)];
    net.connect_all(&mut conns);
    // Player 1 authenticates; player 0 stays silent.
    assert!(matches!(
        net.login(&mut conns, 1),
        ServerBody::HelloOk { .. }
    ));
    net.drive(&mut conns, |c| c[0].disconnected());
    assert!(!conns[1].disconnected());
    assert!(matches!(
        net.call(
            &mut conns,
            1,
            ClientBody::ListSessions {
                filter: SessionFilter::default(),
                after: None,
                limit: 5
            }
        ),
        ServerBody::SessionList { .. }
    ));
    assert_eq!(net.server.service().authenticated_peers(), 1);
}

#[test]
fn bad_tokens_are_rejected_over_the_wire_and_the_server_stays_healthy() {
    let mut net = Net::new(3, |_| {});
    let now = Net::unix_now();
    let stranger = Authority::new("net-key"); // same key id, other key
    let me = net.ids[0].fingerprint();
    let tokens: Vec<(&str, Vec<u8>)> = vec![
        (
            "expired",
            net.auth
                .token("engine-online", "player0", me, 30, now - 600),
        ),
        (
            "wrong audience",
            net.auth.token("engine-game", "player0", me, 60, now),
        ),
        (
            "wrong key",
            stranger.token("engine-online", "player0", me, 60, now),
        ),
        (
            "another certificate",
            net.auth.token(
                "engine-online",
                "player0",
                net.ids[1].fingerprint(),
                60,
                now,
            ),
        ),
        ("tampered", {
            let mut t = net.token_for(0, "player0", 60);
            t[10] ^= 1;
            t
        }),
        ("truncated", net.token_for(0, "player0", 60)[..40].to_vec()),
    ];
    for (label, token) in tokens {
        let mut conns = vec![net.connect(0)];
        net.connect_all(&mut conns);
        let id = conns[0].client.hello(&token).unwrap();
        let mut answer = None;
        net.drive(&mut conns, |c| {
            answer = answer.take().or_else(|| c[0].take_response(id));
            answer.is_some() && c[0].disconnected()
        });
        match answer.unwrap() {
            ServerBody::Error { code, message } => {
                assert_eq!(code, ErrorCode::Unauthorized, "{label}");
                assert_eq!(message, "authentication failed", "{label}");
            }
            other => panic!("{label}: {other:?}"),
        }
    }
    assert_eq!(net.server.service().authenticated_peers(), 0);
    assert!(net.server.service().stats().auth_failures >= 6);
    // A legitimate player still gets in afterwards.
    let mut conns = vec![net.connect(0)];
    net.connect_all(&mut conns);
    assert!(matches!(
        net.login(&mut conns, 0),
        ServerBody::HelloOk { .. }
    ));
}

#[test]
fn a_captured_token_cannot_be_replayed_on_a_new_connection() {
    let mut net = Net::new(1, |_| {});
    let token = net.token_for(0, "player0", 600);
    let mut first = vec![net.connect(0)];
    net.connect_all(&mut first);
    let id = first[0].client.hello(&token).unwrap();
    let mut answer = None;
    net.drive(&mut first, |c| {
        answer = c[0].take_response(id);
        answer.is_some()
    });
    assert!(matches!(answer, Some(ServerBody::HelloOk { .. })));
    first[0].client.endpoint_mut().disconnect(SERVER_PEER);
    let mut second = vec![net.connect(0)];
    net.connect_all(&mut second);
    let id = second[0].client.hello(&token).unwrap();
    let mut answer = None;
    net.drive(&mut second, |c| {
        answer = c[0].take_response(id);
        answer.is_some()
    });
    assert!(matches!(
        answer,
        Some(ServerBody::Error {
            code: ErrorCode::Unauthorized,
            ..
        })
    ));
}

#[test]
fn abusive_traffic_is_contained_to_the_abuser() {
    let mut net = Net::new(3, |c| c.limits.max_malformed_messages = 3);
    let mut conns: Vec<Conn> = (0..3).map(|i| net.connect(i)).collect();
    net.connect_all(&mut conns);
    for i in 0..3 {
        assert!(matches!(
            net.login(&mut conns, i),
            ServerBody::HelloOk { .. }
        ));
    }
    // Oversized payload (beyond max_message_bytes): answered with an error and closed.
    conns[0]
        .client
        .endpoint_mut()
        .send(
            SERVER_PEER,
            Lane::SESSION,
            Delivery::ReliableOrdered,
            &vec![b' '; 70_000],
        )
        .unwrap();
    // Garbage JSON repeatedly: strikes, then disconnect.
    for _ in 0..4 {
        conns[1]
            .client
            .endpoint_mut()
            .send(
                SERVER_PEER,
                Lane::SESSION,
                Delivery::ReliableOrdered,
                b"{\"v\":1,\"id\":1,\"body\":{\"type\":\"oops\"}}",
            )
            .unwrap();
    }
    // Traffic on other lanes is ignored, not interpreted.
    conns[2]
        .client
        .endpoint_mut()
        .send(
            SERVER_PEER,
            Lane::RPC,
            Delivery::ReliableOrdered,
            b"{\"v\":1}",
        )
        .unwrap();
    net.drive(&mut conns, |c| c[0].disconnected() && c[1].disconnected());
    assert!(!conns[2].disconnected());
    assert!(matches!(
        net.call(
            &mut conns,
            2,
            ClientBody::ListSessions {
                filter: SessionFilter::default(),
                after: None,
                limit: 5
            }
        ),
        ServerBody::SessionList { .. }
    ));
    assert!(net.server.stats().ignored_messages >= 1);
    assert_eq!(net.server.service().authenticated_peers(), 1);
}

#[test]
fn host_migration_over_the_wire_reelects_bumps_the_epoch_and_fences_old_tokens() {
    let (mut net, mut c) = world(3);
    let ServerBody::SessionCreated {
        session,
        join_token: epoch1_token,
    } = net.call(&mut c, 0, create_body("room", false, 4))
    else {
        panic!("create");
    };
    for i in [1usize, 2] {
        assert!(matches!(
            net.call(
                &mut c,
                i,
                ClientBody::JoinSession {
                    target: JoinTarget::Id {
                        session_id: session.id
                    },
                    candidates: None
                }
            ),
            ServerBody::Joined { .. }
        ));
    }
    assert_eq!(
        net.call(
            &mut c,
            0,
            ClientBody::UploadMigrationState {
                session_id: session.id,
                epoch: 1,
                state: Blob(b"snapshot-17".to_vec())
            }
        ),
        ServerBody::Ack {}
    );
    // The host's connection dies.
    c[0].client.endpoint_mut().disconnect(SERVER_PEER);
    net.drive(&mut c, |conns| {
        [1usize, 2].iter().all(|&i| {
            conns[i]
                .events
                .iter()
                .any(|e| matches!(e, ClientEvent::Notification(ServerBody::HostChanged { .. })))
        })
    });
    let mut hosts = 0;
    for i in [1usize, 2] {
        let changed = c[i]
            .take_notifications()
            .into_iter()
            .find_map(|n| match n {
                ServerBody::HostChanged {
                    epoch,
                    host,
                    you_are_host,
                    join_token,
                    state,
                    ..
                } => Some((epoch, host, you_are_host, join_token, state)),
                _ => None,
            })
            .unwrap();
        assert_eq!(changed.0, 2);
        assert!(changed.1 == "player1" || changed.1 == "player2");
        let token = changed.3.expect("token for the new epoch");
        assert!(game_verify(&net, &token.0, session.id, 2, i).is_ok());
        if changed.2 {
            hosts += 1;
            assert_eq!(changed.4, Some(Blob(b"snapshot-17".to_vec())));
            assert_eq!(changed.1, format!("player{i}"));
        } else {
            assert_eq!(changed.4, None);
        }
    }
    assert_eq!(hosts, 1, "exactly one new host");
    // The old host's epoch-1 token no longer admits anyone to the new host's game.
    assert_eq!(
        game_verify(&net, &epoch1_token.0, session.id, 2, 0),
        Err(TokenError::MissingScope)
    );
}

// ---------------------------------------------------------------------------------------------
// The same service over TLS-over-TCP: the drivers only need MessageEndpoint.
// ---------------------------------------------------------------------------------------------

#[test]
fn the_service_runs_unchanged_over_mutual_tls_tcp() {
    let server_id = Identity::new("localhost");
    let client_id = Identity::new("player0");
    let auth = Authority::new("tcp-key");
    let binding = CertificateBinding::new(16, Duration::from_secs(30));
    let security = ServerSecurity::new(
        vec![server_id.cert.clone()],
        server_id.key.clone_key(),
        [client_id.cert.clone()],
        binding.authorize_verified(),
    )
    .unwrap();
    let endpoint = TcpServer::bind_secure(
        TcpServerConfig {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            ..TcpServerConfig::default()
        },
        security,
    )
    .unwrap();
    let addr = endpoint.local_addr().unwrap();
    let cfg = service_config(|_| {});
    let limits = cfg.limits.clone();
    let service = engine_online::OnlineService::new(cfg, auth.issuer(), auth.verifier()).unwrap();
    let mut server = OnlineServer::new(endpoint, service, binding);
    let client_security = ClientSecurity::new(
        "localhost",
        [server_id.cert.clone()],
        vec![client_id.cert.clone()],
        client_id.key.clone_key(),
    )
    .unwrap();
    let tcp = TcpClient::connect_secure(TcpClientConfig::new(addr), client_security).unwrap();
    let mut client = OnlineClient::new(tcp, limits).unwrap();
    let token = auth.token(
        "engine-online",
        "tcp-player",
        client_id.fingerprint(),
        600,
        Net::unix_now(),
    );
    let end = Instant::now() + Duration::from_secs(10);
    let mut sent_hello = None;
    let mut sent_create = None;
    let mut created = false;
    while !created {
        assert!(Instant::now() < end, "TCP world timed out");
        server.poll().unwrap();
        for event in client.poll().unwrap() {
            match event {
                ClientEvent::Connected => sent_hello = Some(client.hello(&token).unwrap()),
                ClientEvent::Response { id, body } if Some(id) == sent_hello => {
                    assert!(matches!(body, ServerBody::HelloOk { .. }), "{body:?}");
                    sent_create = Some(client.request(create_body("tcp room", false, 2)).unwrap());
                }
                ClientEvent::Response { id, body } if Some(id) == sent_create => {
                    let ServerBody::SessionCreated {
                        session,
                        join_token,
                    } = body
                    else {
                        panic!("{body:?}");
                    };
                    let verified = auth.verifier().verify_join(
                        &join_token.0,
                        GAME,
                        session.id,
                        1,
                        client_id.cert.as_ref(),
                        Net::unix_now(),
                    );
                    assert!(verified.is_ok(), "{verified:?}");
                    created = true;
                }
                ClientEvent::Disconnected(why) => panic!("disconnected: {why}"),
                _ => {}
            }
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(server.service().stats().sessions_created, 1);
}
