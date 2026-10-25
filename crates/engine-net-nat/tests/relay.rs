//! Relay fallback: symmetric NATs defeat punching, QUIC then runs end to end through the
//! relay; RelaySocket refusals; the `engine-relay-server` binary. Loopback only.
mod support;

use engine_net_nat::*;
use engine_net_quic::DatagramSocket;
use engine_network::{MessageEndpoint, SERVER_PEER};
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{SocketAddr, UdpSocket},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use support::*;

fn quick_punch_config() -> PunchConfig {
    PunchConfig {
        timeout: Duration::from_millis(1500),
        ..PunchConfig::default()
    }
}

fn candidates(
    socket: &mut dyn DatagramSocket,
    service: &mut RelayService<UdpSocket>,
) -> CandidateSet {
    let reflexive = discover(socket, service);
    CandidateSet::gather(
        socket.local_addr().unwrap(),
        [reflexive],
        [service.local_addr().unwrap()],
    )
    .unwrap()
}

/// Punches between A (controlling) and B (controlled); returns both outcomes.
fn try_punch(
    a: &mut dyn DatagramSocket,
    b: &mut dyn DatagramSocket,
    service: &mut RelayService<UdpSocket>,
) -> (PunchOutcome, PunchOutcome, CandidateSet) {
    let a_set = candidates(a, service);
    let b_set = candidates(b, service);
    let secret = PunchSecret::generate().unwrap();
    let now = Instant::now();
    let mut a_s = PunchSession::new(
        now,
        PunchRole::Controlling,
        &secret,
        &b_set,
        quick_punch_config(),
    )
    .unwrap();
    let mut b_s = PunchSession::new(
        now,
        PunchRole::Controlled,
        &secret,
        &a_set,
        quick_punch_config(),
    )
    .unwrap();
    until(Duration::from_secs(5), "punch outcome", || {
        drive_agents(a, Instant::now(), &mut [&mut a_s], 64).unwrap();
        drive_agents(b, Instant::now(), &mut [&mut b_s], 64).unwrap();
        a_s.outcome() != PunchOutcome::Pending && b_s.outcome() != PunchOutcome::Pending
    });
    (a_s.outcome(), b_s.outcome(), b_set)
}

fn ready_peer(socket: &RelaySocket<impl DatagramSocket>, id: AllocationId) -> Option<SocketAddr> {
    match socket.status(id)? {
        AllocationStatus::Ready { peer } => Some(peer),
        _ => None,
    }
}

#[test]
fn symmetric_nats_fail_to_punch_and_quic_connects_through_the_relay() {
    let mut service = relay_service(RelayLimits::default());
    let (mut a, mut b) = (
        SimulatedNat::new(SYMMETRIC),
        SimulatedNat::new(PORT_RESTRICTED),
    );
    let (oa, ob, _) = try_punch(&mut a, &mut b, &mut service);
    assert_eq!(
        (oa, ob),
        (PunchOutcome::TimedOut, PunchOutcome::TimedOut),
        "symmetric vs port-restricted"
    );

    let (mut a, mut b) = (SimulatedNat::new(SYMMETRIC), SimulatedNat::new(SYMMETRIC));
    let (oa, ob, b_set) = try_punch(&mut a, &mut b, &mut service);
    assert_eq!(
        (oa, ob),
        (PunchOutcome::TimedOut, PunchOutcome::TimedOut),
        "symmetric vs symmetric"
    );

    // Fallback: both sides allocate on the relay the peer advertised.
    let relay = b_set.relays().next().unwrap();
    let mut a_relay = RelaySocket::new(a, relay, RelaySocketConfig::default()).unwrap();
    let mut b_relay = RelaySocket::new(b, relay, RelaySocketConfig::default()).unwrap();
    let a_id = a_relay.allocate(&relay_credential(Party::A)).unwrap();
    let b_id = b_relay.allocate(&relay_credential(Party::B)).unwrap();
    let allocation = until(Duration::from_secs(5), "relay allocations", || {
        a_relay.poll_control().unwrap();
        b_relay.poll_control().unwrap();
        service.poll(256).unwrap();
        ready_peer(&a_relay, a_id).is_some() && ready_peer(&b_relay, b_id).is_some()
    });
    let a_peer = ready_peer(&a_relay, a_id).unwrap();
    assert_eq!(
        a_peer,
        SocketAddr::new(relay.ip(), b_relay.local_vport(b_id).unwrap())
    );

    // B hosts, A connects to B's virtual relay address; mutual TLS end to end.
    let pki = Pki::new(1);
    let mut server = pki.server(b_relay);
    let mut client = pki.client(0, a_relay, a_peer);
    let handshake = until(Duration::from_secs(10), "QUIC through relay", || {
        service.poll(256).unwrap();
        poll(&mut server);
        poll(&mut client);
        client.is_connected() && server.connected_peers().len() == 1
    });
    let peer = server.connected_peers()[0];
    send(&mut client, SERVER_PEER, b"hello via relay");
    send(&mut server, peer, &[0xAB; 4000]);
    let (mut at_server, mut at_client) = (Vec::new(), Vec::new());
    until(Duration::from_secs(10), "relayed messages", || {
        service.poll(256).unwrap();
        at_server.extend(payloads(poll(&mut server)));
        at_client.extend(payloads(poll(&mut client)));
        !at_server.is_empty() && !at_client.is_empty()
    });
    assert_eq!(at_server, vec![(peer, b"hello via relay".to_vec())]);
    assert_eq!(at_client, vec![(SERVER_PEER, vec![0xAB; 4000])]);

    let rtt = mean_echo_rtt(20, &mut server, &mut client, peer, || {
        service.poll(256).unwrap();
    });
    let stats = service.server().stats();
    let overhead =
        (stats.forwarded_packets * DATA_HEADER_BYTES as u64) as f64 / stats.forwarded_bytes as f64;
    println!(
        "relayed QUIC: allocations ready in {:.1} ms, handshake {:.1} ms, echo rtt {:.3} ms \
         (QUIC rtt {:.3} ms); forwarded {} datagrams / {} bytes, header overhead {} bytes per \
         datagram = {:.2} % of relayed bytes",
        allocation.as_secs_f64() * 1e3,
        handshake.as_secs_f64() * 1e3,
        rtt.as_secs_f64() * 1e3,
        client.stats(SERVER_PEER).unwrap().rtt_ms,
        stats.forwarded_packets,
        stats.forwarded_bytes,
        DATA_HEADER_BYTES,
        overhead * 100.0,
    );
    assert!(stats.forwarded_packets > 20);
    assert_eq!(stats.data_refused_permission + stats.data_refused_source, 0);
    assert!(stats.control_bytes_out <= stats.control_bytes_in);
}

#[test]
fn relay_socket_refuses_bad_credentials_unpermitted_destinations_and_foreign_datagrams() {
    let mut service = relay_service(RelayLimits::default());
    let relay = service.local_addr().unwrap();
    let mut socket = RelaySocket::new(udp(), relay, RelaySocketConfig::default()).unwrap();
    let forged = socket
        .allocate(b"definitely-not-a-relay-credential")
        .unwrap();
    let issuer = HmacCredentials::new(&[0x11; 32]).unwrap();
    let wrong_key = socket
        .allocate(&issuer.mint(CHANNEL, Party::A, u64::MAX))
        .unwrap();
    let good = socket.allocate(&relay_credential(Party::A)).unwrap();
    until(Duration::from_secs(5), "allocation outcomes", || {
        socket.poll_control().unwrap();
        service.poll(256).unwrap();
        socket.status(good) == Some(AllocationStatus::WaitingForPeer)
            && matches!(socket.status(forged), Some(AllocationStatus::Failed(_)))
            && matches!(socket.status(wrong_key), Some(AllocationStatus::Failed(_)))
    });
    for id in [forged, wrong_key] {
        assert_eq!(
            socket.status(id),
            Some(AllocationStatus::Failed(RelayFailure::Refused(
                RelayRefusal::Unauthorized
            )))
        );
    }
    // Nothing is sent to an address that is not a ready counterpart (no open relaying).
    let elsewhere = SocketAddr::from(([127, 0, 0, 1], 9));
    let error = socket.send_to(b"payload", elsewhere).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AddrNotAvailable);
    let own_vport = SocketAddr::new(relay.ip(), socket.local_vport(good).unwrap());
    assert!(socket.send_to(b"payload", own_vport).is_err());
    assert_eq!(socket.stats().send_refused, 2);

    // Datagrams that do not come from the relay are ignored, even if they look relayed.
    let attacker = udp();
    let mut fake = vec![0x30];
    fake.extend_from_slice(&MAGIC);
    fake.extend_from_slice(&[0, 7, 0, 8, 1, 2, 3]);
    attacker
        .send_to(&fake, socket.local_addr().unwrap())
        .unwrap();
    attacker
        .send_to(b"plain garbage", socket.local_addr().unwrap())
        .unwrap();
    thread::sleep(Duration::from_millis(50));
    let mut buf = [0; 2048];
    assert_eq!(
        socket.recv_from(&mut buf).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(socket.stats().dropped_foreign, 2);
    let stats = service.server().stats();
    assert_eq!(stats.refused_unauthorized, 2);
    assert_eq!(stats.forwarded_packets, 0);
}

/// Spawns the reference binary with a stdin config, so no secret touches the disk.
#[test]
fn reference_binary_serves_discovery_and_relays_with_static_credentials() {
    let tokens = ["static-token-party-a-0123", "static-token-party-b-4567"];
    let config = format!(
        r#"{{"bind":"127.0.0.1:0","stats_interval_secs":1,
            "limits":{{"max_allocations":8}},
            "auth":{{"mode":"static","credentials":[
              {{"token":"{}","channel":"42424242424242424242424242424242","party":"a"}},
              {{"token":"{}","channel":"42424242424242424242424242424242","party":"b"}}]}}}}"#,
        tokens[0], tokens[1]
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_engine-relay-server"))
        .args(["--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(config.as_bytes())
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let relay: SocketAddr = line
        .trim()
        .strip_prefix("engine-relay-server listening on ")
        .unwrap_or_else(|| panic!("unexpected banner {line:?}"))
        .parse()
        .unwrap();
    assert!(relay.ip().is_loopback() && relay.port() != 0);

    let result = std::panic::catch_unwind(|| {
        let mut probe = udp();
        let mut reflexive =
            ReflexiveClient::new(Instant::now(), relay, ReflexiveConfig::default()).unwrap();
        until(Duration::from_secs(5), "binary discovery", || {
            drive_agents(&mut probe, Instant::now(), &mut [&mut reflexive], 8).unwrap();
            reflexive.outcome() != ReflexiveOutcome::Pending
        });
        assert_eq!(
            reflexive.outcome(),
            ReflexiveOutcome::Discovered(probe.local_addr().unwrap())
        );

        let mut a = RelaySocket::new(udp(), relay, RelaySocketConfig::default()).unwrap();
        let mut b = RelaySocket::new(udp(), relay, RelaySocketConfig::default()).unwrap();
        let a_id = a.allocate(tokens[0].as_bytes()).unwrap();
        let b_id = b.allocate(tokens[1].as_bytes()).unwrap();
        let bad = b.allocate(b"static-token-party-c-8910").unwrap();
        until(Duration::from_secs(5), "binary allocations", || {
            a.poll_control().unwrap();
            b.poll_control().unwrap();
            ready_peer(&a, a_id).is_some()
                && ready_peer(&b, b_id).is_some()
                && matches!(b.status(bad), Some(AllocationStatus::Failed(_)))
        });
        let to_b = ready_peer(&a, a_id).unwrap();
        a.send_to(b"opaque ciphertext", to_b).unwrap();
        let mut buf = [0; 2048];
        let mut received = None;
        until(Duration::from_secs(5), "relayed datagram", || {
            if let Ok((len, from)) = b.recv_from(&mut buf) {
                received = Some((buf[..len].to_vec(), from));
            }
            received.is_some()
        });
        assert_eq!(
            received,
            Some((b"opaque ciphertext".to_vec(), ready_peer(&b, b_id).unwrap()))
        );
        // Let at least one statistics line be written.
        thread::sleep(Duration::from_millis(1200));
    });
    child.kill().unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    child.wait().unwrap();
    result.unwrap();
    assert!(
        stderr.contains("\"forwarded_packets\":1"),
        "stats line: {stderr}"
    );
    for token in tokens {
        assert!(!stderr.contains(token), "credential leaked to logs");
    }
    assert!(!line.contains("static-token"));
}
