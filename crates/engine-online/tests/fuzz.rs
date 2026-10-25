//! Seeded fuzzing of every untrusted input path: wire decoders, the token parser/verifier and the
//! whole service state machine. No panics, no unbounded growth, invariants intact.
mod common;

use common::*;
use engine_online::{
    ClientBody, ClientEnvelope, OnlineLimits, ServerEnvelope, TokenError,
    protocol::{
        Attributes, Blob, CreateSession, JoinTarget, PROTOCOL_VERSION, SessionFilter, TicketSpec,
    },
    token::Expect,
};
use std::collections::BTreeMap;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
    fn some_bytes(&mut self, max: u64) -> Vec<u8> {
        let len = self.below(max) as usize;
        self.bytes(len)
    }
    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }
}

fn attrs(rng: &mut Rng, max: u64) -> Attributes {
    (0..rng.below(max + 1))
        .map(|i| {
            (
                format!("k{}", i + rng.below(3)),
                "v".repeat(rng.below(70) as usize),
            )
        })
        .collect()
}
fn blob(rng: &mut Rng, max: u64) -> Option<Blob> {
    rng.chance(60).then(|| Blob(rng.some_bytes(max)))
}

fn random_body(rng: &mut Rng, codes: &[String]) -> ClientBody {
    let session_id = 1 + rng.below(30);
    let epoch = rng.below(4);
    let player = format!("p{}", rng.below(12));
    match rng.below(60) {
        0..=7 => ClientBody::CreateSession(CreateSession {
            name: format!("room{}", rng.below(50)),
            mode: ["duel", "ffa"][rng.below(2) as usize].into(),
            capacity: (rng.below(35)) as u16,
            private: rng.chance(50),
            attributes: attrs(rng, 9),
            candidates: blob(rng, 600),
        }),
        8..=12 => ClientBody::ListSessions {
            filter: SessionFilter {
                mode: rng.chance(30).then(|| "duel".to_owned()),
                attributes: attrs(rng, 5),
                has_space: rng.chance(50),
                name_prefix: rng.chance(30).then(|| "room".to_owned()),
            },
            after: rng.chance(40).then(|| rng.below(30)),
            limit: rng.below(30) as u16,
        },
        13..=18 => ClientBody::JoinSession {
            target: JoinTarget::Id { session_id },
            candidates: blob(rng, 600),
        },
        19..=24 => ClientBody::JoinSession {
            target: JoinTarget::Code {
                code: if !codes.is_empty() && rng.chance(60) {
                    codes[rng.below(codes.len() as u64) as usize].clone()
                } else {
                    (0..rng.below(20))
                        .map(|_| char::from(b'A' + rng.below(26) as u8))
                        .collect()
                },
            },
            candidates: None,
        },
        25..=27 => ClientBody::LeaveSession { session_id },
        28..=30 => ClientBody::SetReady {
            session_id,
            ready: rng.chance(50),
        },
        31..=34 => ClientBody::Kick {
            session_id,
            epoch,
            player,
        },
        35..=37 => ClientBody::SetAttributes {
            session_id,
            epoch,
            attributes: attrs(rng, 9),
        },
        38..=39 => ClientBody::SetCandidates {
            session_id,
            candidates: Blob(rng.some_bytes(600)),
        },
        40..=42 => ClientBody::RequestJoinToken { session_id },
        43..=44 => ClientBody::RequestRelayToken { session_id },
        45..=47 => ClientBody::UploadMigrationState {
            session_id,
            epoch,
            state: Blob(rng.some_bytes(40_000)),
        },
        48..=55 => ClientBody::EnqueueTicket(TicketSpec {
            mode: ["duel", "coop4", "bogus"][rng.below(3) as usize].into(),
            region: ["eu", "us"][rng.below(2) as usize].into(),
            rating: rng.below(3200) as i32 - 100,
            party_size: rng.below(10) as u8,
            attributes: attrs(rng, 3),
            latencies: BTreeMap::new(),
            candidates: blob(rng, 100),
        }),
        56..=58 => ClientBody::CancelTicket {
            ticket: rng.below(100),
        },
        _ => ClientBody::Hello {
            token: Blob(rng.some_bytes(300)),
        },
    }
}

fn encode(body: ClientBody, limits: &OnlineLimits) -> Vec<u8> {
    ClientEnvelope {
        v: PROTOCOL_VERSION,
        id: 1,
        body,
    }
    .encode(limits)
    .unwrap_or_default()
}

fn mutate(rng: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut data = seed.to_vec();
    match rng.below(5) {
        0 if !data.is_empty() => {
            for _ in 0..=rng.below(4) {
                let i = rng.below(data.len() as u64) as usize;
                data[i] ^= 1 << rng.below(8);
            }
        }
        1 => data.truncate(rng.below(data.len() as u64 + 1) as usize),
        2 => {
            let at = rng.below(data.len() as u64 + 1) as usize;
            let junk = rng.some_bytes(20);
            data.splice(at..at, junk);
        }
        3 if data.len() > 2 => {
            let from = rng.below(data.len() as u64) as usize;
            let to = from + rng.below((data.len() - from) as u64) as usize;
            data.drain(from..to);
        }
        _ => {
            let at = rng.below(data.len() as u64 + 1) as usize;
            data.splice(at..at, b"{\"v\":1,".iter().copied());
        }
    }
    data
}

#[test]
fn wire_decoders_survive_20k_hostile_payloads() {
    let limits = OnlineLimits::default();
    let mut rng = Rng(0x00C0_FFEE_D00D_F00D);
    let seeds: Vec<Vec<u8>> = (0..400)
        .map(|_| encode(random_body(&mut rng, &[]), &limits))
        .filter(|payload| !payload.is_empty())
        .collect();
    assert!(seeds.len() > 100);
    let (mut accepted, mut rejected) = (0u32, 0u32);
    for i in 0..20_000 {
        let payload = match i % 4 {
            0 => rng.some_bytes(1500),
            _ => {
                let seed = &seeds[rng.below(seeds.len() as u64) as usize];
                mutate(&mut rng, seed)
            }
        };
        match ClientEnvelope::decode(&payload, &limits) {
            Ok(envelope) => {
                accepted += 1;
                assert!(envelope.body.validate(&limits).is_ok());
                let again = envelope.encode(&limits).unwrap();
                assert_eq!(ClientEnvelope::decode(&again, &limits).unwrap(), envelope);
            }
            Err(_) => rejected += 1,
        }
        // Server-side envelopes get the same treatment (clients parse them).
        let _ = ServerEnvelope::decode(&payload, &limits);
    }
    println!("decoder fuzz: accepted={accepted} rejected={rejected}");
    assert!(accepted > 500 && rejected > 5_000);
}

#[test]
fn token_verifier_rejects_every_mutation_of_a_valid_token() {
    let auth = Authority::new("fuzz-key");
    let cert = [7u8; 32];
    let now = 1_800_000_000;
    let token = auth.token("svc", "alice", cert, 600, now);
    let verifier = auth.verifier();
    let scope = engine_online::Scope::Connect;
    let expect = Expect {
        audience: "svc",
        scope: &scope,
        cert_sha256: &cert,
        single_use: false,
    };
    assert!(verifier.verify(&token, &expect, now).is_ok());
    let mut rng = Rng(0xDEAD_BEEF_1234_5678);
    let mut tested = 0;
    for _ in 0..12_000 {
        let candidate = mutate(&mut rng, &token);
        if candidate == token {
            continue;
        }
        tested += 1;
        let result = verifier.verify(&candidate, &expect, now);
        assert!(
            matches!(result, Err(e) if e != TokenError::Internal),
            "mutated token accepted or crashed the verifier"
        );
    }
    assert!(tested > 9_000);
    assert_eq!(verifier.replay_entries(), 0);
}

const PLAYERS: u64 = 12;

#[test]
fn service_survives_15k_random_operations_with_bounded_state_and_intact_invariants() {
    let mut sim = Sim::new(|c| {
        c.limits.requests_per_second = 1_000_000;
        c.limits.request_burst = 1_000_000;
        c.limits.max_rate_violations = u32::MAX;
        c.limits.max_malformed_messages = u32::MAX;
        c.limits.join_code_failures_per_peer = u32::MAX;
        c.limits.join_code_failures_global = u32::MAX;
        c.limits.max_sessions = 40;
        c.limits.max_tickets = 64;
        c.limits.max_tickets_per_mode = 32;
        c.limits.max_peers = 64;
        c.limits.max_memberships_per_peer = 2;
    });
    let limits = sim.limits.clone();
    let mut rng = Rng(0x5EED_5EED_5EED_5EED);
    let mut next_peer = 1u64;
    let mut peers: Vec<u64> = Vec::new();
    for name in 0..PLAYERS {
        sim.login(next_peer, &format!("p{name}"));
        peers.push(next_peer);
        next_peer += 1;
    }
    let mut codes: Vec<String> = Vec::new();
    let (mut ok_replies, mut error_replies, mut notifications) = (0u64, 0u64, 0u64);
    for iteration in 0..15_000u32 {
        let slot = rng.below(PLAYERS) as usize;
        let peer = peers[slot];
        match rng.below(100) {
            0..=59 => {
                let payload = encode(random_body(&mut rng, &codes), &limits);
                sim.send_raw(peer, &payload);
            }
            60..=79 => {
                let seed = encode(random_body(&mut rng, &codes), &limits);
                let payload = mutate(&mut rng, &seed);
                sim.send_raw(peer, &payload);
            }
            80..=89 => {
                let payload = rng.some_bytes(300);
                sim.send_raw(peer, &payload);
            }
            90..=94 => sim.advance(rng.below(3000)),
            95..=97 => {
                // The player reconnects, sometimes before the old link was reaped.
                if rng.chance(50) {
                    sim.drop_peer(peer);
                }
                sim.login(next_peer, &format!("p{slot}"));
                peers[slot] = next_peer;
                next_peer += 1;
            }
            _ => {
                let _ = sim.service.force_migrate(1 + rng.below(30), sim.now());
                sim.pump();
            }
        }
        // Account for replies, harvest join codes, then discard.
        for (_, queue) in std::mem::take(&mut sim.inbox) {
            for envelope in queue {
                match envelope.body {
                    engine_online::ServerBody::Error { .. } => error_replies += 1,
                    engine_online::ServerBody::SessionCreated { ref session, .. } => {
                        ok_replies += 1;
                        if let Some(code) = &session.code
                            && codes.len() < 64
                        {
                            codes.push(code.clone());
                        }
                    }
                    ref body if envelope.id == 0 => {
                        notifications += 1;
                        let _ = body;
                    }
                    _ => ok_replies += 1,
                }
            }
        }
        // The service may have dropped peers (bad Hello, token expiry...): bring them back.
        for (slot, current) in peers.iter_mut().enumerate() {
            if sim.disconnected.contains(current) {
                sim.login(next_peer, &format!("p{slot}"));
                *current = next_peer;
                next_peer += 1;
                sim.inbox.clear();
            }
        }
        if iteration % 100 == 0 {
            sim.service.lobby().check_invariants().unwrap();
            assert!(sim.service.lobby().session_count() <= 40);
            assert!(sim.service.matchmaker().len() <= 64);
            assert!(sim.service.tracked_peers() <= 64);
            assert!(sim.service.authenticated_peers() <= PLAYERS as usize + 1);
        }
    }
    sim.service.lobby().check_invariants().unwrap();
    println!(
        "service fuzz: ok_replies={ok_replies} error_replies={error_replies} notifications={notifications} stats={:?}",
        sim.service.stats()
    );
    assert!(
        ok_replies > 1_000,
        "the fuzzer must exercise successful paths"
    );
    assert!(error_replies > 1_000, "and rejections");
    assert!(notifications > 50, "and notifications");
    assert!(sim.service.stats().sessions_created > 100);
}
