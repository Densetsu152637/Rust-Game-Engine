//! Host certificate pinning over a real loopback QUIC handshake (127.0.0.1:0, synthetic identities):
//! a joiner configured with the host's registered fingerprint reaches that host and nobody else.
mod common;

use common::Identity;
use engine_net_quic::{
    QuicClient, QuicClientConfig, QuicClientSecurity, QuicServer, QuicServerConfig,
    QuicServerSecurity,
};
use engine_network::{LinkEvent, MessageEndpoint, PollBudget};
use engine_online::{CertFingerprint, pin::pinned_client_config};
use std::{
    net::SocketAddr,
    thread,
    time::{Duration, Instant},
};

/// Whether a client pinned to `pin` completes a handshake with a server presenting `server_id`.
fn handshake(server_id: &Identity, client_id: &Identity, pin: CertFingerprint) -> bool {
    let security = QuicServerSecurity::new(
        vec![server_id.cert.clone()],
        server_id.key.clone_key(),
        [client_id.cert.clone()],
        |_, _| true,
    )
    .unwrap();
    let mut server = QuicServer::bind(
        SocketAddr::from(([127, 0, 0, 1], 0)),
        QuicServerConfig::default(),
        security,
    )
    .unwrap();
    let addr = server.local_addr().unwrap();
    let tls =
        pinned_client_config(pin, vec![client_id.cert.clone()], client_id.key.clone_key()).unwrap();
    let security = QuicClientSecurity::from_rustls_config(tls, "game-host").unwrap();
    let mut client = QuicClient::connect(addr, QuicClientConfig::default(), security).unwrap();
    let end = Instant::now() + Duration::from_secs(4);
    while Instant::now() < end {
        server.poll(PollBudget::default()).unwrap();
        for event in client.poll(PollBudget::default()).unwrap() {
            match event {
                LinkEvent::Connected(_) => return true,
                LinkEvent::Disconnected { .. } => return false,
                _ => {}
            }
        }
        thread::sleep(Duration::from_millis(1));
    }
    false
}

#[test]
fn a_joiner_connects_only_to_the_certificate_the_lobby_named() {
    let host = Identity::new("host.invalid");
    let imposter = Identity::new("host.invalid");
    let joiner = Identity::new("joiner");
    // The registered fingerprint matches the host's self-signed certificate: no CA, no hostname.
    assert!(handshake(&host, &joiner, host.fingerprint()));
    // The same name with a different key (a malicious member standing in for the host) fails.
    assert!(!handshake(&imposter, &joiner, host.fingerprint()));
    // And a pin that names nobody's certificate fails closed.
    assert!(!handshake(&host, &joiner, [0; 32]));
}
