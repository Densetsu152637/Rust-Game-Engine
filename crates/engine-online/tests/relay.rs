//! Pair-scoped relay credentials end to end: the online service mints them, an
//! `engine-net-nat` relay accepts them through `relay_authorizer`, and real loopback UDP
//! sockets (127.0.0.1:0) allocate, pair and forward. Synthetic identities only.
mod common;

use common::*;
use engine_net_nat::{
    AllocationId, AllocationStatus, RelayFailure, RelayLimits, RelayRefusal, RelayServer,
    RelayService, RelaySocket, RelaySocketConfig,
};
use engine_net_quic::DatagramSocket;
use engine_online::{
    ClientBody, ServerBody,
    protocol::{Attributes, Blob, CreateSession, JoinTarget, TokenKind},
    relay::relay_authorizer_with_clock,
};
use std::{
    net::{SocketAddr, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

fn udp() -> UdpSocket {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    socket
}

fn until(what: &str, mut test: impl FnMut() -> bool) {
    let start = Instant::now();
    while !test() {
        assert!(
            start.elapsed() < Duration::from_secs(8),
            "timed out: {what}"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

struct World {
    sim: Sim,
    session: u64,
    clock: Arc<AtomicU64>,
    relay: RelayService<UdpSocket>,
}
impl World {
    fn new() -> Self {
        let mut sim = Sim::new(|_| {});
        for (peer, name) in [(1, "alice"), (2, "bob"), (3, "carol")] {
            sim.login(peer, name);
        }
        let ServerBody::SessionCreated { session, .. } = sim.request(
            1,
            ClientBody::CreateSession(CreateSession {
                name: "room".into(),
                mode: "duel".into(),
                capacity: 4,
                private: false,
                attributes: Attributes::new(),
                candidates: Some(Blob(vec![1; 8])),
            }),
        ) else {
            panic!("create failed");
        };
        for peer in [2u64, 3] {
            sim.request(
                peer,
                ClientBody::JoinSession {
                    target: JoinTarget::Id {
                        session_id: session.id,
                    },
                    candidates: None,
                },
            );
        }
        let clock = Arc::new(AtomicU64::new(sim.now().unix_secs));
        let ticking = clock.clone();
        let authorizer =
            relay_authorizer_with_clock(sim.auth.verifier(), "engine-relay", move || {
                ticking.load(Ordering::Relaxed)
            });
        let server = RelayServer::new(Instant::now(), RelayLimits::default(), authorizer).unwrap();
        Self {
            session: session.id,
            relay: RelayService::new(udp(), server),
            clock,
            sim,
        }
    }
    fn credential(&mut self, peer: u64, with: &str) -> Vec<u8> {
        match self.sim.request(
            peer,
            ClientBody::RequestRelayToken {
                session_id: self.session,
                peer: with.into(),
            },
        ) {
            ServerBody::Token {
                token,
                kind: TokenKind::Relay,
                ..
            } => token.0,
            other => panic!("relay token expected, got {other:?}"),
        }
    }
    fn socket(&self) -> RelaySocket<UdpSocket> {
        RelaySocket::new(
            udp(),
            self.relay.local_addr().unwrap(),
            RelaySocketConfig::default(),
        )
        .unwrap()
    }
    /// Pumps the relay and the given sockets until `done` holds.
    fn run(
        &mut self,
        what: &str,
        sockets: &mut [&mut RelaySocket<UdpSocket>],
        mut done: impl FnMut(&mut [&mut RelaySocket<UdpSocket>]) -> bool,
    ) {
        until(what, || {
            for socket in sockets.iter_mut() {
                socket.poll_control().unwrap();
            }
            self.relay.poll(256).unwrap();
            done(sockets)
        });
    }
}

fn ready(socket: &RelaySocket<UdpSocket>, id: AllocationId) -> Option<SocketAddr> {
    match socket.status(id)? {
        AllocationStatus::Ready { peer } => Some(peer),
        _ => None,
    }
}

#[test]
fn pair_credentials_allocate_pair_and_forward_through_the_adapter() {
    let mut world = World::new();
    let alice_bob = world.credential(1, "bob");
    let bob_alice = world.credential(2, "alice");
    let (mut a, mut b) = (world.socket(), world.socket());
    let a_id = a.allocate(&alice_bob).unwrap();
    let b_id = b.allocate(&bob_alice).unwrap();
    world.run("pair becomes ready", &mut [&mut a, &mut b], |s| {
        ready(s[0], a_id).is_some() && ready(s[1], b_id).is_some()
    });
    let to_bob = ready(&a, a_id).unwrap();
    a.send_to(b"opaque ciphertext", to_bob).unwrap();
    let mut buffer = [0; 2048];
    let mut received = None;
    world.run("datagram arrives", &mut [&mut a, &mut b], |s| {
        if let Ok((len, from)) = s[1].recv_from(&mut buffer) {
            received = Some((buffer[..len].to_vec(), from));
        }
        received.is_some()
    });
    assert_eq!(
        received,
        Some((b"opaque ciphertext".to_vec(), ready(&b, b_id).unwrap()))
    );
    let stats = world.relay.server().stats();
    assert_eq!(stats.forwarded_packets, 1);
    assert_eq!(stats.allocations_created, 2);
    assert_eq!(stats.refused_unauthorized + stats.refused_party_taken, 0);
}

#[test]
fn a_third_member_cannot_reach_or_squat_a_pairs_channel() {
    let mut world = World::new();
    // Carol is a member of the same session and asks for credentials with both others. Each one
    // names a channel she is party to, so she can allocate there but never in (alice, bob).
    let carol_alice = world.credential(3, "alice");
    let carol_bob = world.credential(3, "bob");
    let (mut c1, mut c2) = (world.socket(), world.socket());
    let (i1, i2) = (
        c1.allocate(&carol_alice).unwrap(),
        c2.allocate(&carol_bob).unwrap(),
    );
    world.run("carol's own channels", &mut [&mut c1, &mut c2], |s| {
        s[0].status(i1) == Some(AllocationStatus::WaitingForPeer)
            && s[1].status(i2) == Some(AllocationStatus::WaitingForPeer)
    });
    // The legitimate pair still gets both of its slots.
    let alice_bob = world.credential(1, "bob");
    let bob_alice = world.credential(2, "alice");
    let (mut a, mut b) = (world.socket(), world.socket());
    let (a_id, b_id) = (
        a.allocate(&alice_bob).unwrap(),
        b.allocate(&bob_alice).unwrap(),
    );
    world.run(
        "pair becomes ready",
        &mut [&mut a, &mut b, &mut c1, &mut c2],
        |s| ready(s[0], a_id).is_some() && ready(s[1], b_id).is_some(),
    );
    // Neither of carol's allocations was paired with anybody: no cross-channel counterpart.
    assert_eq!(c1.status(i1), Some(AllocationStatus::WaitingForPeer));
    assert_eq!(c2.status(i2), Some(AllocationStatus::WaitingForPeer));
    assert_eq!(world.relay.server().stats().allocations_created, 4);

    // A stolen bearer credential cannot take a slot the owner already holds, and it only ever
    // names that one slot: alice's credential has no way to ask for bob's.
    let mut thief = world.socket();
    let stolen = thief.allocate(&alice_bob).unwrap();
    world.run("squat refused", &mut [&mut thief, &mut a, &mut b], |s| {
        matches!(s[0].status(stolen), Some(AllocationStatus::Failed(_)))
    });
    assert_eq!(
        thief.status(stolen),
        Some(AllocationStatus::Failed(RelayFailure::Refused(
            RelayRefusal::PartyTaken
        )))
    );
    assert!(ready(&a, a_id).is_some() && ready(&b, b_id).is_some());
}

#[test]
fn credentials_of_the_wrong_audience_kind_or_age_are_refused() {
    let mut world = World::new();
    let alice_bob = world.credential(1, "bob");
    // A join token (game audience, join scope) is not a relay credential.
    let ServerBody::Token { token: join, .. } = world.sim.request(
        1,
        ClientBody::RequestJoinToken {
            session_id: world.session,
        },
    ) else {
        panic!("join token expected");
    };
    let mut socket = world.socket();
    let join_id = socket.allocate(&join.0).unwrap();
    let junk_id = socket.allocate(b"garbage-credential-bytes").unwrap();
    world.run("refusals", &mut [&mut socket], |s| {
        matches!(s[0].status(join_id), Some(AllocationStatus::Failed(_)))
            && matches!(s[0].status(junk_id), Some(AllocationStatus::Failed(_)))
    });
    for id in [join_id, junk_id] {
        assert_eq!(
            socket.status(id),
            Some(AllocationStatus::Failed(RelayFailure::Refused(
                RelayRefusal::Unauthorized
            )))
        );
    }
    // After the credential expires (default relay lifetime 3600 s plus verifier skew) it is dead.
    world.clock.fetch_add(3600 + 60, Ordering::Relaxed);
    let mut late = world.socket();
    let late_id = late.allocate(&alice_bob).unwrap();
    world.run("expired refused", &mut [&mut late], |s| {
        matches!(s[0].status(late_id), Some(AllocationStatus::Failed(_)))
    });
    assert_eq!(
        late.status(late_id),
        Some(AllocationStatus::Failed(RelayFailure::Refused(
            RelayRefusal::Unauthorized
        )))
    );
}
