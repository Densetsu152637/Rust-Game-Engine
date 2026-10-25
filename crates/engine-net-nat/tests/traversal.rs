//! Reflexive discovery, hole punching through simulated NATs, QUIC over the punched path
//! and on-the-wire spoofing. Loopback only (127.0.0.1:0). Run with `--nocapture` for the
//! measured punch times and round trips.
mod support;

use engine_net_nat::*;
use engine_net_quic::DatagramSocket;
use engine_network::{MessageEndpoint, SERVER_PEER};
use std::{
    io,
    sync::atomic::Ordering,
    thread,
    time::{Duration, Instant},
};
use support::*;

fn gather(
    socket: &mut dyn DatagramSocket,
    service: &mut RelayService<std::net::UdpSocket>,
) -> CandidateSet {
    let reflexive = discover(socket, service);
    CandidateSet::gather(
        socket.local_addr().unwrap(),
        [reflexive],
        [service.local_addr().unwrap()],
    )
    .unwrap()
}

/// Runs a controlling (A) and controlled (B) session to completion.
fn punch(
    a: &mut dyn DatagramSocket,
    a_set: &CandidateSet,
    b: &mut dyn DatagramSocket,
    b_set: &CandidateSet,
    config: &PunchConfig,
) -> (PunchSession, PunchSession) {
    let secret = PunchSecret::generate().unwrap();
    let now = Instant::now();
    let mut a_session =
        PunchSession::new(now, PunchRole::Controlling, &secret, b_set, config.clone()).unwrap();
    let mut b_session =
        PunchSession::new(now, PunchRole::Controlled, &secret, a_set, config.clone()).unwrap();
    until(config.timeout * 2, "punch sessions to finish", || {
        drive_agents(a, Instant::now(), &mut [&mut a_session], 64).unwrap();
        drive_agents(b, Instant::now(), &mut [&mut b_session], 64).unwrap();
        a_session.outcome() != PunchOutcome::Pending && b_session.outcome() != PunchOutcome::Pending
    });
    (a_session, b_session)
}

fn connected(session: &PunchSession) -> PunchPath {
    match session.outcome() {
        PunchOutcome::Connected(path) => path,
        other => panic!("{:?} session not connected: {other:?}", session.role()),
    }
}

#[test]
fn reflexive_discovery_reveals_the_mapping_behaviour() {
    let mut first = relay_service(RelayLimits::default());
    let mut second = relay_service(RelayLimits::default());
    let mut open = udp();
    let local = open.local_addr().unwrap();
    assert_eq!(
        discover(&mut open, &mut first),
        local,
        "no NAT: reflexive equals host"
    );
    for (nat_type, independent) in [
        (FULL_CONE, true),
        (PORT_RESTRICTED, true),
        (SYMMETRIC, false),
    ] {
        let mut nat = SimulatedNat::new(nat_type);
        let a = discover(&mut nat, &mut first);
        let b = discover(&mut nat, &mut second);
        assert_ne!(a, nat.local_addr().unwrap());
        assert_eq!(a == b, independent, "{}: {a} vs {b}", nat_type.name);
    }
    let stats = first.server().stats();
    assert!(stats.binding_responses >= 4);
    assert!(stats.control_bytes_out <= stats.control_bytes_in);
}

#[test]
fn hole_punching_succeeds_through_cone_nats() {
    let mut service = relay_service(RelayLimits::default());
    let config = PunchConfig::default();
    for (a_type, b_type) in [
        (FULL_CONE, FULL_CONE),
        (RESTRICTED_CONE, PORT_RESTRICTED),
        (PORT_RESTRICTED, PORT_RESTRICTED),
        // A symmetric side still punches to a full cone through a peer-reflexive address.
        (SYMMETRIC, FULL_CONE),
        (FULL_CONE, SYMMETRIC),
    ] {
        let (mut a, mut b) = (SimulatedNat::new(a_type), SimulatedNat::new(b_type));
        let a_set = gather(&mut a, &mut service);
        let b_set = gather(&mut b, &mut service);
        let start = Instant::now();
        let (a_session, b_session) = punch(&mut a, &a_set, &mut b, &b_set, &config);
        let elapsed = start.elapsed();
        let (a_path, b_path) = (connected(&a_session), connected(&b_session));
        // Each side selected an address the other side's NAT really sends from.
        assert!(b.public_addrs().contains(&a_path.remote), "{}", a_type.name);
        assert!(a.public_addrs().contains(&b_path.remote), "{}", b_type.name);
        assert_eq!(
            a_session.stats().rejected_auth + b_session.stats().rejected_auth,
            0
        );
        println!(
            "punch {} <-> {}: both selected in {:.1} ms (A via {:?}, B via {:?}, probe rtt {:?}), \
             {} + {} probes, {} inbound datagrams filtered",
            a_type.name,
            b_type.name,
            elapsed.as_secs_f64() * 1e3,
            a_path.kind,
            b_path.kind,
            a_path.rtt,
            a_session.stats().probes_sent,
            b_session.stats().probes_sent,
            a.filtered().load(Ordering::Relaxed) + b.filtered().load(Ordering::Relaxed),
        );
        if a_type.name == "symmetric" {
            assert_eq!(b_path.kind, CandidateKind::PeerReflexive);
        }
    }
}

#[test]
fn quic_runs_over_a_punched_path_while_the_host_punches_for_another_peer() {
    let mut service = relay_service(RelayLimits::default());
    let config = PunchConfig::default();
    let pki = Pki::new(2);
    let (host_quic, mut host_nat) =
        DemuxSocket::new(SimulatedNat::new(PORT_RESTRICTED), DemuxConfig::default()).unwrap();
    let host_set = gather(&mut host_nat, &mut service);
    let mut server = pki.server(host_quic);

    // Peer 1 punches to the host, then connects QUIC over the selected path.
    let (p1_quic, mut p1_nat) =
        DemuxSocket::new(SimulatedNat::new(PORT_RESTRICTED), DemuxConfig::default()).unwrap();
    let p1_set = gather(&mut p1_nat, &mut service);
    let secret = PunchSecret::generate().unwrap();
    let now = Instant::now();
    let mut host_s1 =
        PunchSession::new(now, PunchRole::Controlled, &secret, &p1_set, config.clone()).unwrap();
    let mut p1_s = PunchSession::new(
        now,
        PunchRole::Controlling,
        &secret,
        &host_set,
        config.clone(),
    )
    .unwrap();
    let punch1 = until(Duration::from_secs(5), "peer 1 punch", || {
        drive_agents(&mut host_nat, Instant::now(), &mut [&mut host_s1], 64).unwrap();
        drive_agents(&mut p1_nat, Instant::now(), &mut [&mut p1_s], 64).unwrap();
        poll(&mut server);
        host_s1.outcome() != PunchOutcome::Pending && p1_s.outcome() != PunchOutcome::Pending
    });
    let path1 = connected(&p1_s);
    connected(&host_s1);
    let mut c1 = pki.client(0, p1_quic, path1.remote);
    let quic1 = until(Duration::from_secs(10), "QUIC over punched path", || {
        drive_agents(&mut host_nat, Instant::now(), &mut [&mut host_s1], 64).unwrap();
        poll(&mut server);
        poll(&mut c1);
        c1.is_connected() && server.connected_peers().len() == 1
    });
    let peer1 = server.connected_peers()[0];

    // Peer 2 punches through the host's live QUIC socket via the NatChannel.
    let (p2_quic, mut p2_nat) =
        DemuxSocket::new(SimulatedNat::new(RESTRICTED_CONE), DemuxConfig::default()).unwrap();
    let p2_set = gather(&mut p2_nat, &mut service);
    let secret2 = PunchSecret::generate().unwrap();
    let now = Instant::now();
    let mut host_s2 = PunchSession::new(
        now,
        PunchRole::Controlled,
        &secret2,
        &p2_set,
        config.clone(),
    )
    .unwrap();
    let mut p2_s =
        PunchSession::new(now, PunchRole::Controlling, &secret2, &host_set, config).unwrap();
    send(&mut c1, SERVER_PEER, b"during punch");
    let mut during = Vec::new();
    let punch2 = until(
        Duration::from_secs(5),
        "peer 2 punch on live socket",
        || {
            drive_agents(
                &mut host_nat,
                Instant::now(),
                &mut [&mut host_s1, &mut host_s2],
                64,
            )
            .unwrap();
            drive_agents(&mut p2_nat, Instant::now(), &mut [&mut p2_s], 64).unwrap();
            during.extend(payloads(poll(&mut server)));
            poll(&mut c1);
            host_s2.outcome() != PunchOutcome::Pending && p2_s.outcome() != PunchOutcome::Pending
        },
    );
    let path2 = connected(&p2_s);
    let mut c2 = pki.client(1, p2_quic, path2.remote);
    until(Duration::from_secs(10), "second QUIC peer", || {
        drive_agents(
            &mut host_nat,
            Instant::now(),
            &mut [&mut host_s1, &mut host_s2],
            64,
        )
        .unwrap();
        during.extend(payloads(poll(&mut server)));
        poll(&mut c1);
        poll(&mut c2);
        c2.is_connected() && server.connected_peers().len() == 2
    });
    assert_eq!(
        during,
        vec![(peer1, b"during punch".to_vec())],
        "QUIC kept flowing"
    );
    let peer2 = *server
        .connected_peers()
        .iter()
        .find(|&&p| p != peer1)
        .unwrap();

    // Messages both ways with both peers.
    send(&mut server, peer1, b"to peer 1");
    send(&mut server, peer2, b"to peer 2");
    send(&mut c2, SERVER_PEER, b"from peer 2");
    let (mut got1, mut got2, mut at_server) = (Vec::new(), Vec::new(), Vec::new());
    until(Duration::from_secs(10), "messages", || {
        at_server.extend(payloads(poll(&mut server)));
        got1.extend(payloads(poll(&mut c1)));
        got2.extend(payloads(poll(&mut c2)));
        !got1.is_empty() && !got2.is_empty() && !at_server.is_empty()
    });
    assert_eq!(got1, vec![(SERVER_PEER, b"to peer 1".to_vec())]);
    assert_eq!(got2, vec![(SERVER_PEER, b"to peer 2".to_vec())]);
    assert_eq!(at_server, vec![(peer2, b"from peer 2".to_vec())]);

    let rtt = mean_echo_rtt(20, &mut server, &mut c1, peer1, || {
        poll(&mut c2);
    });
    let quic_rtt = c1.stats(SERVER_PEER).unwrap().rtt_ms;
    let stats = host_nat.stats();
    assert!(stats.quic_received > 0 && stats.control_received > 0);
    assert_eq!(stats.control_dropped + stats.quic_dropped, 0);
    println!(
        "punched QUIC: punch 1 {:.1} ms, QUIC handshake {:.1} ms, punch 2 on live socket {:.1} ms; \
         direct echo rtt {:.3} ms (QUIC rtt {quic_rtt:.3} ms); host demux {stats:?}",
        punch1.as_secs_f64() * 1e3,
        quic1.as_secs_f64() * 1e3,
        punch2.as_secs_f64() * 1e3,
        rtt.as_secs_f64() * 1e3,
    );
}

#[test]
fn spoofed_and_unauthenticated_probes_on_the_wire_get_no_answer() {
    let (mut host, mut peer, attacker) = (udp(), udp(), udp());
    let host_set = CandidateSet::gather(host.local_addr().unwrap(), [], []).unwrap();
    let peer_set = CandidateSet::gather(peer.local_addr().unwrap(), [], []).unwrap();
    let host_addr = host.local_addr().unwrap();
    let secret = PunchSecret::generate().unwrap();
    let now = Instant::now();
    let config = PunchConfig::default();
    let mut host_session = PunchSession::new(
        now,
        PunchRole::Controlled,
        &secret,
        &peer_set,
        config.clone(),
    )
    .unwrap();

    // A genuine probe (as an on-path observer would capture it), tampered in its tag.
    let mut genuine = PunchSession::new(
        now,
        PunchRole::Controlling,
        &secret,
        &host_set,
        config.clone(),
    )
    .unwrap();
    let mut tampered = genuine.poll_transmit(now).unwrap().payload;
    let last = tampered.len() - 1;
    tampered[last] ^= 0x01;
    // Probes keyed by a guessed secret, and garbage behind a valid control header.
    let mut forger = PunchSession::new(
        now,
        PunchRole::Controlling,
        &PunchSecret::generate().unwrap(),
        &host_set,
        config.clone(),
    )
    .unwrap();
    let forged = forger.poll_transmit(now).unwrap().payload;
    let mut garbage = vec![0x10];
    garbage.extend_from_slice(&MAGIC);
    garbage.extend_from_slice(&[0xEE; 72]);
    for datagram in [&tampered, &forged, &garbage] {
        for _ in 0..10 {
            attacker.send_to(datagram, host_addr).unwrap();
        }
    }
    let mut unclaimed = 0;
    let deadline = Instant::now() + Duration::from_millis(200);
    while Instant::now() < deadline {
        unclaimed += drive_agents(&mut host, Instant::now(), &mut [&mut host_session], 64).unwrap();
        thread::sleep(Duration::from_millis(1));
    }
    let stats = host_session.stats();
    assert_eq!(stats.rejected_auth, 10, "tampered probes");
    assert_eq!((stats.probes_received, stats.acks_sent), (0, 0));
    assert_eq!(
        unclaimed, 20,
        "foreign-session and garbage datagrams are not this session's"
    );
    let mut buf = [0; 256];
    assert_eq!(
        attacker.recv_from(&mut buf).unwrap_err().kind(),
        io::ErrorKind::WouldBlock,
        "the attacker received nothing"
    );
    assert_eq!(host_session.outcome(), PunchOutcome::Pending);

    // The genuine peer still punches afterwards.
    until(Duration::from_secs(5), "genuine punch after attack", || {
        drive_agents(&mut host, Instant::now(), &mut [&mut host_session], 64).unwrap();
        drive_agents(&mut peer, Instant::now(), &mut [&mut genuine], 64).unwrap();
        matches!(host_session.outcome(), PunchOutcome::Connected(_))
            && matches!(genuine.outcome(), PunchOutcome::Connected(_))
    });
    assert_eq!(connected(&genuine).remote, host_addr);
    assert_eq!(connected(&genuine).kind, CandidateKind::Host);
}
