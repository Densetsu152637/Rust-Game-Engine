//! Sans-IO relay server behaviour and seeded fuzzing of every decoder.
use crate::{
    wire::{self, Allocate, AllocateOk, COOKIE_LEN, Refresh, TXID_LEN, kind},
    *,
};
use engine_net_quic::DatagramSocket;
use std::{
    collections::VecDeque,
    io,
    net::SocketAddr,
    time::{Duration, Instant},
};

const CHANNEL_1: ChannelId = ChannelId([1; 16]);
const CHANNEL_2: ChannelId = ChannelId([2; 16]);

fn credentials() -> StaticCredentials {
    let mut list = StaticCredentials::new();
    for (token, channel, party) in [
        (&b"token-channel-1-a"[..], CHANNEL_1, Party::A),
        (b"token-channel-1-b", CHANNEL_1, Party::B),
        (b"token-channel-2-a", CHANNEL_2, Party::A),
        (b"token-channel-2-b", CHANNEL_2, Party::B),
    ] {
        list.add(token, channel, party).unwrap();
    }
    list
}
fn addr(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// Feeds datagrams to a server, asserting the amplification invariant on every reply.
struct Harness {
    server: RelayServer,
    now: Instant,
    out: Vec<u8>,
}
impl Harness {
    fn new(limits: RelayLimits) -> Self {
        Self::with_authorizer(limits, credentials().into_authorizer())
    }
    fn with_authorizer(
        limits: RelayLimits,
        authorize: impl Fn(&[u8], SocketAddr) -> Option<Grant> + Send + Sync + 'static,
    ) -> Self {
        let now = Instant::now();
        Self {
            server: RelayServer::new(now, limits, authorize).unwrap(),
            now,
            out: Vec::new(),
        }
    }
    fn send(&mut self, from: SocketAddr, datagram: &[u8]) -> Option<(SocketAddr, Vec<u8>)> {
        let to = self
            .server
            .handle(self.now, from, datagram, &mut self.out)?;
        if to == from {
            assert!(
                self.out.len() <= datagram.len(),
                "reply larger than request"
            );
        } else {
            assert_eq!(self.out, datagram, "forwarding must not alter or grow data");
        }
        Some((to, self.out.clone()))
    }
    /// Full allocate exchange (challenge, then cookie). Err carries the refusal code.
    fn allocate(
        &mut self,
        from: SocketAddr,
        credential: &[u8],
    ) -> std::result::Result<AllocateOk, u8> {
        let txid = [from.port() as u8; TXID_LEN];
        let mut request = Vec::new();
        let mut cookie = [0; COOKIE_LEN];
        for _ in 0..2 {
            wire::encode_allocate(
                &mut request,
                &Allocate {
                    txid,
                    cookie,
                    credential,
                },
            )
            .unwrap();
            let (to, reply) = self.send(from, &request).expect("allocate answered");
            assert_eq!(to, from);
            if let Some((t, c)) = wire::decode_challenge(&reply) {
                assert_eq!(t, txid);
                cookie = c;
                continue;
            }
            if let Some(ok) = wire::decode_allocate_ok(&reply) {
                assert_eq!(ok.observed, from);
                return Ok(ok);
            }
            return Err(wire::decode_error(&reply).expect("error reply").1);
        }
        panic!("second challenge")
    }
    fn refresh(
        &mut self,
        from: SocketAddr,
        ok: &AllocateOk,
        lifetime_secs: u16,
    ) -> Option<wire::RefreshOk> {
        let mut request = Vec::new();
        wire::encode_refresh(
            &mut request,
            &Refresh {
                txid: [5; TXID_LEN],
                vport: ok.vport,
                key: ok.key,
                lifetime_secs,
            },
        );
        let (_, reply) = self.send(from, &request)?;
        wire::decode_refresh_ok(&reply)
    }
}
fn data(source: u16, destination: u16, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    wire::encode_data_header(&mut out, source, destination);
    out.extend_from_slice(payload);
    out
}

#[test]
fn relay_pairs_two_authorized_parties_and_forwards_both_ways() {
    let mut h = Harness::new(RelayLimits::default());
    let (a_addr, b_addr) = (addr(1001), addr(1002));
    let a = h.allocate(a_addr, b"token-channel-1-a").unwrap();
    assert_eq!(a.counterpart, 0, "B has not allocated yet");
    let b = h.allocate(b_addr, b"token-channel-1-b").unwrap();
    assert_eq!(b.counterpart, a.vport);
    // Retransmitted allocate from the same address is idempotent.
    let again = h.allocate(b_addr, b"token-channel-1-b").unwrap();
    assert_eq!((again.vport, again.key), (b.vport, b.key));
    assert_eq!(h.refresh(a_addr, &a, 60).unwrap().counterpart, b.vport);

    let packet = data(a.vport, b.vport, b"ciphertext");
    assert_eq!(h.send(a_addr, &packet), Some((b_addr, packet.clone())));
    let packet = data(b.vport, a.vport, &[7; 1472]);
    assert_eq!(h.send(b_addr, &packet).map(|r| r.0), Some(a_addr));
    let too_big = data(b.vport, a.vport, &[7; 1473]);
    assert_eq!(h.send(b_addr, &too_big), None);
    let stats = h.server.stats();
    assert_eq!((stats.forwarded_packets, stats.allocations_active), (2, 2));
    assert_eq!(stats.allocations_created, 2);

    // Release frees the slot; the counterpart sees it gone on its next refresh.
    assert_eq!(h.refresh(a_addr, &a, 0).unwrap().lifetime_secs, 0);
    assert_eq!(h.refresh(b_addr, &b, 60).unwrap().counterpart, 0);
    assert_eq!(h.send(b_addr, &data(b.vport, a.vport, b"x")), None);
}

#[test]
fn relay_refuses_bad_credentials_foreign_slots_and_unpermitted_destinations() {
    let mut h = Harness::new(RelayLimits {
        max_allocations: 3,
        ..RelayLimits::default()
    });
    let (a_addr, b_addr, c_addr, mallory) = (addr(2001), addr(2002), addr(2003), addr(2666));
    assert_eq!(h.allocate(mallory, b"not-a-valid-token-at-all"), Err(1));
    let a = h.allocate(a_addr, b"token-channel-1-a").unwrap();
    let b = h.allocate(b_addr, b"token-channel-1-b").unwrap();
    // Exactly two parties: a stolen slot credential from another address is refused.
    assert_eq!(h.allocate(mallory, b"token-channel-1-a"), Err(2));
    let c = h.allocate(c_addr, b"token-channel-2-a").unwrap();
    // Capacity reached.
    assert_eq!(h.allocate(mallory, b"token-channel-2-b"), Err(3));

    // A may only send to its counterpart B, never to channel 2's allocation.
    assert_eq!(h.send(a_addr, &data(a.vport, c.vport, b"x")), None);
    // Spoofed source address with A's allocation.
    assert_eq!(h.send(mallory, &data(a.vport, b.vport, b"x")), None);
    // Unknown allocation.
    assert_eq!(h.send(a_addr, &data(4242, b.vport, b"x")), None);
    // C's counterpart has not allocated: nothing to forward to.
    assert_eq!(h.send(c_addr, &data(c.vport, a.vport, b"x")), None);
    // Refresh with the wrong key or from the wrong address.
    let mut forged = a.clone();
    forged.key[0] ^= 1;
    assert!(h.refresh(a_addr, &forged, 60).is_none());
    assert!(h.refresh(mallory, &a, 60).is_none());

    let stats = h.server.stats();
    assert_eq!(stats.refused_unauthorized, 3);
    assert_eq!((stats.refused_party_taken, stats.refused_capacity), (1, 1));
    assert_eq!(stats.data_refused_permission, 2);
    assert_eq!(
        (stats.data_refused_source, stats.data_refused_unknown),
        (1, 1)
    );
    assert_eq!(stats.forwarded_packets, 0);
}

#[test]
fn relay_allocations_expire_unless_refreshed() {
    let mut h = Harness::new(RelayLimits {
        allocation_lifetime_secs: 2,
        ..RelayLimits::default()
    });
    let (a_addr, b_addr) = (addr(3001), addr(3002));
    let a = h.allocate(a_addr, b"token-channel-1-a").unwrap();
    let b = h.allocate(b_addr, b"token-channel-1-b").unwrap();
    assert_eq!(a.lifetime_secs, 2);
    h.now += Duration::from_millis(1500);
    // Refresh cannot extend beyond the server lifetime.
    assert_eq!(h.refresh(b_addr, &b, 3600).unwrap().lifetime_secs, 2);
    h.now += Duration::from_millis(1000);
    assert_eq!(h.send(a_addr, &data(a.vport, b.vport, b"late")), None);
    assert_eq!(h.send(b_addr, &data(b.vport, a.vport, b"late")), None);
    assert!(
        h.refresh(a_addr, &a, 60).is_none(),
        "expired allocation cannot be revived"
    );
    h.now += Duration::from_secs(2);
    h.server.sweep(h.now);
    let stats = h.server.stats();
    assert_eq!(stats.allocations_active, 0);
    assert_eq!(stats.allocations_expired, 2);
}

#[test]
fn relay_grant_deadline_caps_refreshes() {
    let mut h = Harness::with_authorizer(RelayLimits::default(), |credential, _| {
        Some(Grant {
            channel: CHANNEL_1,
            party: if credential == b"a" {
                Party::A
            } else {
                Party::B
            },
            max_lifetime: Some(Duration::from_secs(5)),
        })
    });
    let a = h.allocate(addr(3101), b"a").unwrap();
    assert_eq!(
        a.lifetime_secs, 5,
        "server lifetime 120 s capped by the grant"
    );
    h.now += Duration::from_secs(3);
    assert_eq!(h.refresh(addr(3101), &a, 120).unwrap().lifetime_secs, 2);
    h.now += Duration::from_millis(2100);
    assert!(h.refresh(addr(3101), &a, 120).is_none(), "deadline passed");
}

#[test]
fn relay_rate_limits_hold() {
    let mut h = Harness::new(RelayLimits {
        control_requests_per_second_per_ip: 10,
        control_burst_per_ip: 20,
        allocation_bytes_per_second: 10_000,
        allocation_packets_per_second: 1000,
        ..RelayLimits::default()
    });
    let mut request = Vec::new();
    wire::encode_binding_request(&mut request, &[1; TXID_LEN]);
    let answered = (0..100)
        .filter(|i| h.send(addr(4000 + i), &request).is_some())
        .count();
    assert_eq!(answered, 20, "per-IP burst across source ports");
    h.now += Duration::from_secs(1);
    let answered = (0..100)
        .filter(|_| h.send(addr(4000), &request).is_some())
        .count();
    assert_eq!(answered, 10, "refill rate");

    h.now += Duration::from_secs(10);
    let (a_addr, b_addr) = (addr(4101), addr(4102));
    let a = h.allocate(a_addr, b"token-channel-1-a").unwrap();
    let b = h.allocate(b_addr, b"token-channel-1-b").unwrap();
    let packet = data(a.vport, b.vport, &[0; 991]);
    let forwarded = (0..100)
        .filter(|_| h.send(a_addr, &packet).is_some())
        .count();
    assert_eq!(forwarded, 10, "10 kB/s budget at 1000 bytes per datagram");
    let stats = h.server.stats();
    assert!(stats.rate_limited_control >= 170);
    assert_eq!(stats.data_refused_rate, 90);
}

#[test]
fn relay_never_amplifies_any_input() {
    let mut h = Harness::new(RelayLimits {
        control_requests_per_second_per_ip: 1_000_000,
        control_burst_per_ip: 1_000_000,
        ..RelayLimits::default()
    });
    let mut rng = Rng(0xA11CE);
    let mut samples = valid_samples();
    let (a_addr, b_addr) = (addr(5001), addr(5002));
    let a = h.allocate(a_addr, b"token-channel-1-a").unwrap();
    let b = h.allocate(b_addr, b"token-channel-1-b").unwrap();
    samples.push(data(a.vport, b.vport, b"payload"));
    let mut worst: f64 = 0.0;
    for i in 0..20_000 {
        let input = fuzz_input(&mut rng, &samples, i);
        let from = if i % 2 == 0 {
            a_addr
        } else {
            addr(5003 + (i % 7) as u16)
        };
        if let Some((to, reply)) = h.send(from, &input)
            && to == from
        {
            worst = worst.max(reply.len() as f64 / input.len() as f64);
        }
    }
    let stats = h.server.stats();
    let ratio = stats.control_bytes_out as f64 / stats.control_bytes_in as f64;
    println!(
        "relay amplification: aggregate control ratio {ratio:.3}, worst single reply {worst:.3}, \
         {} replies to {} control bytes",
        stats.datagrams_out, stats.control_bytes_in
    );
    assert!(ratio <= 1.0 && worst <= 1.0);
    assert_eq!(stats.suppressed_amplification, 0);
}

// --- Seeded fuzzing ------------------------------------------------------------------------

/// SplitMix64, as in `engine-network`'s conditioner.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn valid_samples() -> Vec<Vec<u8>> {
    let txid = [3; TXID_LEN];
    let observed = addr(40000);
    let mut samples = Vec::new();
    let mut push = |f: &dyn Fn(&mut Vec<u8>)| {
        let mut out = Vec::new();
        f(&mut out);
        samples.push(out);
    };
    push(&|o| wire::encode_binding_request(o, &txid));
    push(&|o| wire::encode_binding_response(o, &txid, observed));
    push(&|o| {
        wire::encode_allocate(
            o,
            &Allocate {
                txid,
                cookie: [1; COOKIE_LEN],
                credential: b"token-channel-1-a",
            },
        )
        .unwrap()
    });
    push(&|o| wire::encode_challenge(o, &txid, &[4; COOKIE_LEN]));
    push(&|o| {
        wire::encode_allocate_ok(
            o,
            &AllocateOk {
                txid,
                vport: 9,
                key: [2; 16],
                lifetime_secs: 60,
                counterpart: 10,
                observed,
            },
        )
    });
    push(&|o| {
        wire::encode_refresh(
            o,
            &Refresh {
                txid,
                vport: 9,
                key: [2; 16],
                lifetime_secs: 30,
            },
        )
    });
    push(&|o| {
        wire::encode_refresh_ok(
            o,
            &wire::RefreshOk {
                txid,
                lifetime_secs: 30,
                counterpart: 10,
            },
        )
    });
    push(&|o| wire::encode_error(o, &txid, 1));
    push(&|o| {
        wire::encode_data_header(o, 9, 10);
        o.extend_from_slice(b"quic");
    });
    let secret = PunchSecret::from_bytes([6; 32]);
    let mut set = CandidateSet::new();
    set.push(Candidate {
        kind: CandidateKind::ServerReflexive,
        addr: addr(7000),
    })
    .unwrap();
    let mut session = PunchSession::new(
        Instant::now(),
        PunchRole::Controlling,
        &secret,
        &set,
        PunchConfig::default(),
    )
    .unwrap();
    samples.push(session.poll_transmit(Instant::now()).unwrap().payload);
    samples.push(set.to_bytes());
    samples.push(
        HmacCredentials::new(&[1; 32])
            .unwrap()
            .mint(CHANNEL_1, Party::A, u64::MAX),
    );
    samples
}

/// Random bytes, mutated valid samples, or a valid header with a random body.
fn fuzz_input(rng: &mut Rng, samples: &[Vec<u8>], i: usize) -> Vec<u8> {
    match i % 3 {
        0 => (0..rng.below(160)).map(|_| rng.next() as u8).collect(),
        1 => {
            let mut input = samples[rng.below(samples.len())].clone();
            for _ in 0..=rng.below(4) {
                match rng.below(4) {
                    0 if !input.is_empty() => {
                        let at = rng.below(input.len());
                        input[at] ^= 1 << rng.below(8);
                    }
                    1 if !input.is_empty() => input.truncate(rng.below(input.len())),
                    2 => input.push(rng.next() as u8),
                    _ if !input.is_empty() => {
                        let at = rng.below(input.len());
                        input[at] = rng.next() as u8;
                    }
                    _ => {}
                }
            }
            input
        }
        _ => {
            let kinds = [
                kind::BINDING_REQUEST,
                kind::BINDING_RESPONSE,
                kind::PUNCH_PROBE,
                kind::PUNCH_ACK,
                kind::ALLOCATE,
                kind::ALLOCATE_CHALLENGE,
                kind::ALLOCATE_OK,
                kind::REFRESH,
                kind::REFRESH_OK,
                kind::RELAY_ERROR,
                kind::DATA,
            ];
            let mut input = vec![kinds[rng.below(kinds.len())]];
            input.extend_from_slice(&MAGIC);
            input.extend((0..rng.below(120)).map(|_| rng.next() as u8));
            input
        }
    }
}

/// Yields queued datagrams as if they came from `from`.
struct ScriptedSocket {
    from: SocketAddr,
    queue: VecDeque<Vec<u8>>,
}
impl DatagramSocket for ScriptedSocket {
    fn send_to(&mut self, datagram: &[u8], _: SocketAddr) -> io::Result<usize> {
        Ok(datagram.len())
    }
    fn recv_from(&mut self, buffer: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let datagram = self.queue.pop_front().ok_or(io::ErrorKind::WouldBlock)?;
        let len = datagram.len().min(buffer.len());
        buffer[..len].copy_from_slice(&datagram[..len]);
        Ok((len, self.from))
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(addr(1))
    }
}

#[test]
fn seeded_fuzz_of_every_decoder_never_panics() {
    const INPUTS: usize = 30_000;
    let mut rng = Rng(0x005E_ED0F_FA22);
    let samples = valid_samples();
    let now = Instant::now();
    let secret = PunchSecret::from_bytes([6; 32]);
    let mut set = CandidateSet::new();
    set.push(Candidate {
        kind: CandidateKind::ServerReflexive,
        addr: addr(7000),
    })
    .unwrap();
    let mut controlled = PunchSession::new(
        now,
        PunchRole::Controlled,
        &secret,
        &set,
        PunchConfig::default(),
    )
    .unwrap();
    let mut controlling = PunchSession::new(
        now,
        PunchRole::Controlling,
        &secret,
        &set,
        PunchConfig::default(),
    )
    .unwrap();
    let mut reflexive = ReflexiveClient::new(now, addr(3478), ReflexiveConfig::default()).unwrap();
    let mut server =
        RelayServer::new(now, RelayLimits::default(), credentials().into_authorizer()).unwrap();
    let hmac = HmacCredentials::new(&[1; 32]).unwrap();
    let mut relay_socket = RelaySocket::new(
        ScriptedSocket {
            from: addr(3478),
            queue: VecDeque::new(),
        },
        addr(3478),
        RelaySocketConfig::default(),
    )
    .unwrap();
    relay_socket.allocate(b"token-channel-1-a").unwrap();
    let mut out = Vec::new();
    let mut accepted = 0_usize;
    let mut buffer = [0; 2048];
    for i in 0..INPUTS {
        let input = fuzz_input(&mut rng, &samples, i);
        let decoded = [
            wire::decode_binding_request(&input).is_some(),
            wire::decode_binding_response(&input).is_some(),
            wire::decode_allocate(&input).is_some(),
            wire::decode_challenge(&input).is_some(),
            wire::decode_allocate_ok(&input).is_some(),
            wire::decode_refresh(&input).is_some(),
            wire::decode_refresh_ok(&input).is_some(),
            wire::decode_error(&input).is_some(),
            wire::decode_data(&input).is_some(),
            crate::punch::decode_punch(&input).is_some(),
            CandidateSet::from_bytes(&input).is_ok(),
            hmac.verify(&input, 0).is_some(),
        ];
        accepted += decoded.iter().filter(|&&d| d).count();
        let from = addr(7000);
        controlled.handle(now, from, &input);
        controlling.handle(now, from, &input);
        reflexive.handle(now, addr(3478), &input);
        if let Some(to) = server.handle(now, addr(9000 + (i % 50) as u16), &input, &mut out)
            && to == addr(9000 + (i % 50) as u16)
        {
            assert!(out.len() <= input.len());
        }
        relay_socket.inner_mut().queue.push_back(input);
        let _ = relay_socket.recv_from(&mut buffer);
    }
    while controlled.poll_transmit(now).is_some() {}
    // Mutated valid samples keep some inputs decodable, so the decoders are exercised past
    // their first length check.
    assert!(accepted > 100, "only {accepted} accepted");
    assert_eq!(
        controlling.stats().acks_received,
        0,
        "no forged acknowledgement accepted"
    );
    assert!(controlled.stats().rejected_auth > 0);
    println!("fuzz: {INPUTS} inputs x 17 targets, {accepted} decoder acceptances");
}
