//! Shared [`MessageEndpoint`] contract checks for transport crates (feature `test-support`).
//!
//! A transport's tests implement [`ContractFixture`] for a connected-on-demand server/client
//! pair and call [`run_all`] (or one check). The checks assert the delivery guarantees in
//! [`crate::channel`]: connect events, per-lane ordering, sequenced stale drop, `PollBudget`
//! bounds, `QueueFull`, oversize/invalid rejection and disconnect events. They panic with a
//! descriptive message on violation.
use crate::{
    Delivery, Error, Lane, LinkEvent, MessageEndpoint, PeerId, PollBudget, SERVER_PEER,
    TransportSecurity,
};

/// A server endpoint and a client endpoint of one transport, not yet connected.
pub trait ContractFixture {
    fn server(&mut self) -> &mut dyn MessageEndpoint;
    fn client(&mut self) -> &mut dyn MessageEndpoint;
    /// Lets the transport make progress between polls (sleep, or in-process delivery).
    fn advance(&mut self) {}
    /// Makes unreliable payloads sent while enabled arrive in reverse order. Returns whether
    /// the transport can do this; the stale-drop check runs only when it can.
    fn reorder_unreliable(&mut self, _enabled: bool) -> bool {
        false
    }
}

const ROUNDS: usize = 4000;

fn full() -> PollBudget {
    PollBudget {
        max_messages: usize::MAX,
        max_bytes: usize::MAX,
        max_accepts: 16,
    }
}

/// Events collected from both sides.
#[derive(Default)]
pub struct Seen {
    pub server: Vec<LinkEvent>,
    pub client: Vec<LinkEvent>,
}

fn poll_both(fx: &mut dyn ContractFixture, budget: PollBudget, seen: &mut Seen) {
    fx.advance();
    seen.server
        .extend(fx.server().poll(budget).expect("server poll"));
    seen.client
        .extend(fx.client().poll(budget).expect("client poll"));
}

fn messages(events: &[LinkEvent]) -> Vec<(PeerId, Lane, Delivery, &[u8])> {
    events
        .iter()
        .filter_map(|e| match e {
            LinkEvent::Message {
                peer,
                lane,
                delivery,
                payload,
            } => Some((*peer, *lane, *delivery, payload.as_slice())),
            _ => None,
        })
        .collect()
}

/// Polls until `done` holds for the collected events or panics after a bounded number of rounds.
fn until(fx: &mut dyn ContractFixture, seen: &mut Seen, what: &str, done: impl Fn(&Seen) -> bool) {
    for _ in 0..ROUNDS {
        if done(seen) {
            return;
        }
        poll_both(fx, full(), seen);
    }
    assert!(done(seen), "timed out waiting for {what}");
}

/// Connects the pair and returns the server's peer ID for the client plus the events seen.
pub fn connect(fx: &mut dyn ContractFixture) -> (PeerId, Seen) {
    let mut seen = Seen::default();
    until(fx, &mut seen, "both sides to connect", |s| {
        s.server
            .iter()
            .any(|e| matches!(e, LinkEvent::Connected(_)))
            && s.client
                .iter()
                .any(|e| matches!(e, LinkEvent::Connected(_)))
    });
    let peers = fx.server().connected_peers();
    assert_eq!(peers.len(), 1, "server admits exactly the one client");
    let peer = peers[0];
    assert_ne!(peer, SERVER_PEER, "servers never assign peer 0 to a client");
    assert_eq!(fx.client().connected_peers(), vec![SERVER_PEER]);
    assert_eq!(fx.server().transport(), fx.client().transport());
    let secure = fx.server().security() != TransportSecurity::TrustedLanPlaintext;
    for (events, id) in [(&seen.server, peer), (&seen.client, SERVER_PEER)] {
        let connected = events
            .iter()
            .position(|e| *e == LinkEvent::Connected(id))
            .expect("Connected for the admitted peer");
        let authenticated = events
            .iter()
            .position(|e| *e == LinkEvent::Authenticated(id));
        if secure {
            let at = authenticated.expect("secure endpoints report Authenticated");
            assert!(at < connected, "Authenticated precedes Connected");
        }
    }
    (peer, seen)
}

/// Reliable ordered payloads on several lanes arrive in send order per lane, both directions.
pub fn lane_ordering(fx: &mut dyn ContractFixture) {
    let (peer, mut seen) = connect(fx);
    let lanes = [Lane::REPLICATION, Lane::RPC, Lane::INPUT];
    for i in 0u16..30 {
        for lane in lanes {
            let payload = [lane.0, (i >> 8) as u8, i as u8];
            fx.client()
                .send(SERVER_PEER, lane, Delivery::ReliableOrdered, &payload)
                .expect("client send");
            fx.server()
                .send(peer, lane, Delivery::ReliableOrdered, &payload)
                .expect("server send");
        }
    }
    until(fx, &mut seen, "all ordered payloads", |s| {
        messages(&s.server).len() >= 90 && messages(&s.client).len() >= 90
    });
    for (events, from) in [(&seen.server, SERVER_PEER), (&seen.client, peer)] {
        let _ = from;
        for lane in lanes {
            let got: Vec<u16> = messages(events)
                .into_iter()
                .filter(|(_, l, d, _)| *l == lane && *d == Delivery::ReliableOrdered)
                .map(|(_, _, _, p)| (u16::from(p[1]) << 8) | u16::from(p[2]))
                .collect();
            assert_eq!(
                got,
                (0u16..30).collect::<Vec<_>>(),
                "lane {} delivered in send order",
                lane.0
            );
        }
    }
}

/// Sequenced payloads are delivered in increasing order; a stale one is dropped and counted.
pub fn sequenced_stale_drop(fx: &mut dyn ContractFixture) {
    let (peer, mut seen) = connect(fx);
    let lane = Lane::INPUT;
    for i in 0u8..10 {
        fx.client()
            .send(SERVER_PEER, lane, Delivery::UnreliableSequenced, &[i])
            .expect("send");
    }
    until(fx, &mut seen, "sequenced payloads", |s| {
        !messages(&s.server).is_empty()
    });
    for _ in 0..20 {
        poll_both(fx, full(), &mut seen);
    }
    let got: Vec<u8> = messages(&seen.server)
        .into_iter()
        .filter(|(_, _, d, _)| *d == Delivery::UnreliableSequenced)
        .map(|(_, _, _, p)| p[0])
        .collect();
    assert!(!got.is_empty(), "in-order sequenced payloads are delivered");
    assert!(
        got.windows(2).all(|w| w[0] < w[1]),
        "sequenced delivery is increasing: {got:?}"
    );
    if !fx.reorder_unreliable(true) {
        return;
    }
    seen.server.clear();
    let before = fx
        .server()
        .stats(peer)
        .expect("server stats")
        .dropped_unreliable;
    for i in 100u8..110 {
        fx.client()
            .send(SERVER_PEER, lane, Delivery::UnreliableSequenced, &[i])
            .expect("send");
    }
    for _ in 0..20 {
        poll_both(fx, full(), &mut seen);
    }
    fx.reorder_unreliable(false);
    let got: Vec<u8> = messages(&seen.server)
        .into_iter()
        .map(|(_, _, _, p)| p[0])
        .collect();
    assert!(
        got.windows(2).all(|w| w[0] < w[1]),
        "reordered sequenced payloads still arrive increasing: {got:?}"
    );
    assert!(got.len() < 10, "stale payloads were discarded: {got:?}");
    let after = fx
        .server()
        .stats(peer)
        .expect("server stats")
        .dropped_unreliable;
    assert!(after > before, "stale drops are counted");
}

/// `poll` never returns more messages or payload bytes than the budget allows.
pub fn budget_bounds(fx: &mut dyn ContractFixture) {
    let (_peer, mut seen) = connect(fx);
    for i in 0u8..40 {
        fx.client()
            .send(SERVER_PEER, Lane::RPC, Delivery::ReliableOrdered, &[i; 40])
            .expect("send");
    }
    // Let everything arrive in the transport without consuming it on the server.
    for _ in 0..50 {
        fx.advance();
        fx.client().poll(full()).expect("client poll");
    }
    let zero = PollBudget {
        max_messages: 0,
        max_bytes: 0,
        max_accepts: 0,
    };
    let none = fx.server().poll(zero).expect("zero-budget poll");
    assert!(messages(&none).is_empty(), "a zero budget delivers nothing");

    let by_count = PollBudget {
        max_messages: 5,
        max_bytes: usize::MAX,
        max_accepts: 16,
    };
    let mut total = 0;
    for _ in 0..ROUNDS {
        fx.advance();
        let events = fx.server().poll(by_count).expect("poll");
        let count = messages(&events).len();
        assert!(count <= 5, "message budget exceeded: {count}");
        total += count;
        if total >= 20 {
            break;
        }
    }
    assert!(total >= 20, "budgeted polls make progress");

    let by_bytes = PollBudget {
        max_messages: usize::MAX,
        max_bytes: 100,
        max_accepts: 16,
    };
    let mut rest = 0;
    for _ in 0..ROUNDS {
        fx.advance();
        let events = fx.server().poll(by_bytes).expect("poll");
        let bytes: usize = messages(&events).iter().map(|m| m.3.len()).sum();
        assert!(bytes <= 100, "byte budget exceeded: {bytes}");
        rest += messages(&events).len();
        if total + rest >= 40 {
            break;
        }
    }
    assert_eq!(total + rest, 40, "no payload is lost by budgeting");
    let _ = &mut seen;
}

/// A full reliable queue returns `QueueFull` and queues nothing for that call.
pub fn queue_full(fx: &mut dyn ContractFixture) {
    let (_peer, mut seen) = connect(fx);
    let payload = [7u8; 1000];
    let mut accepted = 0usize;
    let mut hit = false;
    for _ in 0..100_000 {
        match fx.client().send(
            SERVER_PEER,
            Lane::DEFAULT,
            Delivery::ReliableOrdered,
            &payload,
        ) {
            Ok(()) => accepted += 1,
            Err(Error::QueueFull) => {
                hit = true;
                break;
            }
            Err(other) => panic!("unexpected send error: {other}"),
        }
    }
    assert!(hit, "a bounded reliable queue reports QueueFull");
    assert!(accepted > 0, "some payloads fit before the queue filled");
    until(fx, &mut seen, "queued payloads", |s| {
        messages(&s.server).len() >= accepted
    });
    for _ in 0..20 {
        poll_both(fx, full(), &mut seen);
    }
    assert_eq!(
        messages(&seen.server).len(),
        accepted,
        "QueueFull queued nothing"
    );
    fx.client()
        .send(
            SERVER_PEER,
            Lane::DEFAULT,
            Delivery::ReliableOrdered,
            &payload,
        )
        .expect("sending works again once the queue drains");
}

/// Oversize payloads, bad lanes and unknown peers are `Error::Invalid`.
pub fn invalid_sends(fx: &mut dyn ContractFixture) {
    let (peer, mut seen) = connect(fx);
    for delivery in [
        Delivery::ReliableOrdered,
        Delivery::ReliableUnordered,
        Delivery::UnreliableSequenced,
        Delivery::Unreliable,
    ] {
        let max = fx
            .client()
            .max_payload(SERVER_PEER, delivery)
            .expect("max payload for the server");
        assert!(max > 0);
        let too_big = vec![0u8; max + 1];
        assert!(
            matches!(
                fx.client()
                    .send(SERVER_PEER, Lane::DEFAULT, delivery, &too_big),
                Err(Error::Invalid(_))
            ),
            "{delivery:?}: oversize is rejected"
        );
    }
    let max = fx
        .client()
        .max_payload(SERVER_PEER, Delivery::ReliableOrdered)
        .expect("max");
    assert!(matches!(
        fx.client().send(
            SERVER_PEER,
            Lane(Lane::MAX + 1),
            Delivery::ReliableOrdered,
            b"x"
        ),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        fx.client()
            .send(9999, Lane::DEFAULT, Delivery::ReliableOrdered, b"x"),
        Err(Error::Invalid(_))
    ));
    assert_eq!(fx.client().max_payload(9999, Delivery::Unreliable), None);
    assert!(fx.client().stats(9999).is_none());
    let exact = vec![9u8; max];
    fx.client()
        .send(
            SERVER_PEER,
            Lane::DEFAULT,
            Delivery::ReliableOrdered,
            &exact,
        )
        .expect("a maximum-size payload is accepted");
    until(fx, &mut seen, "the maximum-size payload", |s| {
        messages(&s.server).iter().any(|m| m.3.len() == max)
    });
    let _ = peer;
}

/// A local disconnect reaches the other side as `Disconnected`, and the peer is gone.
pub fn disconnect_events(fx: &mut dyn ContractFixture) {
    let (peer, mut seen) = connect(fx);
    assert!(fx.client().disconnect(SERVER_PEER));
    assert!(!fx.client().disconnect(SERVER_PEER), "already gone");
    assert!(fx.client().connected_peers().is_empty());
    assert!(matches!(
        fx.client()
            .send(SERVER_PEER, Lane::DEFAULT, Delivery::ReliableOrdered, b"x"),
        Err(Error::Invalid(_))
    ));
    until(fx, &mut seen, "the server to see the disconnect", |s| {
        s.server
            .iter()
            .any(|e| matches!(e, LinkEvent::Disconnected { peer: p, .. } if *p == peer))
    });
    assert!(fx.server().connected_peers().is_empty());
    assert!(!fx.server().disconnect(peer));
    assert!(fx.server().stats(peer).is_none());
}

/// Server-initiated disconnect reaches the client.
pub fn server_disconnect_events(fx: &mut dyn ContractFixture) {
    let (peer, mut seen) = connect(fx);
    assert!(fx.server().disconnect(peer));
    until(fx, &mut seen, "the client to see the disconnect", |s| {
        s.client
            .iter()
            .any(|e| matches!(e, LinkEvent::Disconnected { peer: p, .. } if *p == SERVER_PEER))
    });
    assert!(fx.client().connected_peers().is_empty());
}

/// Runs every check on a fresh fixture each time.
pub fn run_all<F: ContractFixture>(mut make: impl FnMut() -> F) {
    lane_ordering(&mut make());
    sequenced_stale_drop(&mut make());
    budget_bounds(&mut make());
    queue_full(&mut make());
    invalid_sends(&mut make());
    disconnect_events(&mut make());
    server_disconnect_events(&mut make());
}
