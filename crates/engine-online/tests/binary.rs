//! The reference server binary, driven as a real process on 127.0.0.1:0 with synthetic
//! identities written to PEM files.
mod common;

use common::Identity;
use engine_net_quic::{QuicClient, QuicClientConfig, QuicClientSecurity};
use engine_network::{
    Client as TcpClient, ClientConfig as TcpClientConfig, ClientSecurity, MessageEndpoint,
    PrivateKeyDer,
};
use engine_online::{
    ClientBody, ClientEvent, OnlineClient, OnlineLimits, ServerBody,
    config::decode_token_text,
    pem,
    protocol::{Attributes, Blob, CreateSession, SessionFilter},
};
use std::{
    io::{BufRead, BufReader, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const BIN: &str = env!("CARGO_BIN_EXE_engine-online-server");

fn scratch(tag: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("online-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
fn pkcs8_of(identity: &Identity) -> Vec<u8> {
    match &identity.key {
        PrivateKeyDer::Pkcs8(key) => key.secret_pkcs8_der().to_vec(),
        _ => panic!("rcgen emits PKCS#8"),
    }
}
fn write_identity(dir: &Path, name: &str, identity: &Identity) {
    std::fs::write(
        dir.join(format!("{name}.pem")),
        pem::encode("CERTIFICATE", identity.cert.as_ref()),
    )
    .unwrap();
    let key = dir.join(format!("{name}-key.pem"));
    std::fs::write(&key, pem::encode("PRIVATE KEY", &pkcs8_of(identity))).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}
fn run(args: &[&str]) -> std::process::Output {
    Command::new(BIN).args(args).output().unwrap()
}

struct Server {
    child: Child,
    lines: mpsc::Receiver<String>,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn spawn(config: &Path) -> (Server, SocketAddr) {
    let mut child = Command::new(BIN)
        .args([
            "run",
            "--config",
            config.to_str().unwrap(),
            "--exit-on-stdin-close",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let server = Server { child, lines: rx };
    let line = server
        .lines
        .recv_timeout(Duration::from_secs(20))
        .expect("server printed its address");
    assert!(
        line.starts_with("engine-online-server listening "),
        "{line}"
    );
    let addr = line
        .split_whitespace()
        .nth(2)
        .and_then(|a| a.parse::<SocketAddr>().ok())
        .expect("address in banner");
    assert!(addr.ip().is_loopback() && addr.port() != 0, "{addr}");
    (server, addr)
}

/// Authenticate, create a session and list it, over whichever endpoint the caller built.
fn exercise<E: MessageEndpoint>(mut client: OnlineClient<E>, token: &[u8]) -> OnlineClient<E> {
    let end = Instant::now() + Duration::from_secs(15);
    let mut hello = None;
    let mut create = None;
    let mut list = None;
    let mut listed = false;
    while !listed {
        assert!(Instant::now() < end, "binary session timed out");
        for event in client.poll().unwrap() {
            match event {
                ClientEvent::Connected => hello = Some(client.hello(token).unwrap()),
                ClientEvent::Response { id, body } if Some(id) == hello => {
                    assert!(matches!(body, ServerBody::HelloOk { .. }), "{body:?}");
                    create = Some(
                        client
                            .request(ClientBody::CreateSession(CreateSession {
                                name: "from the binary".into(),
                                mode: "duel".into(),
                                capacity: 2,
                                private: false,
                                attributes: Attributes::new(),
                                candidates: Some(Blob(vec![1, 2, 3])),
                            }))
                            .unwrap(),
                    );
                }
                ClientEvent::Response { id, body } if Some(id) == create => {
                    assert!(
                        matches!(body, ServerBody::SessionCreated { .. }),
                        "{body:?}"
                    );
                    list = Some(
                        client
                            .request(ClientBody::ListSessions {
                                filter: SessionFilter::default(),
                                after: None,
                                limit: 5,
                            })
                            .unwrap(),
                    );
                }
                ClientEvent::Response { id, body } if Some(id) == list => {
                    let ServerBody::SessionList { sessions, .. } = body else {
                        panic!("{body:?}");
                    };
                    assert_eq!(sessions.len(), 1);
                    assert_eq!(sessions[0].name, "from the binary");
                    assert_eq!(sessions[0].host.as_deref(), Some("alice"));
                    listed = true;
                }
                ClientEvent::Disconnected(why) => panic!("disconnected: {why}"),
                _ => {}
            }
        }
        thread::sleep(Duration::from_millis(1));
    }
    client
}

fn config_json(transport: &str) -> String {
    format!(
        r#"{{
  "listen": "127.0.0.1:0",
  "transport": "{transport}",
  "tls": {{"server_chain": "server.pem", "server_key": "server-key.pem", "client_roots": "client.pem"}},
  "tokens": {{"signing_key": "signing.pem", "key_id": "k1", "issuer": "studio", "relay_audience": "engine-relay"}},
  "max_connections": 8
}}"#
    )
}

fn shutdown<E: MessageEndpoint>(server: &mut Server, client: &mut OnlineClient<E>) {
    // Closing stdin shuts the server down cleanly.
    drop(server.child.stdin.take());
    let end = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = server.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < end,
            "server did not exit after stdin closed"
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success());
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        if client
            .poll()
            .unwrap()
            .iter()
            .any(|event| matches!(event, ClientEvent::Disconnected(_)))
        {
            break;
        }
        assert!(
            Instant::now() < end,
            "active client was not closed by shutdown"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

fn full_cycle(transport: &str) {
    let dir = scratch(transport);
    let server_id = Identity::new("localhost");
    let client_id = Identity::new("alice-device");
    write_identity(&dir, "server", &server_id);
    write_identity(&dir, "client", &client_id);
    let config = dir.join("server.json");
    std::fs::write(&config, config_json(transport)).unwrap();
    let signing = dir.join("signing.pem");

    // Key generation prints only the public half and refuses to overwrite.
    let generated = run(&[
        "generate-key",
        "--out",
        signing.to_str().unwrap(),
        "--key-id",
        "k1",
        "--issuer",
        "studio",
    ]);
    assert!(generated.status.success());
    let stdout = String::from_utf8(generated.stdout).unwrap();
    assert!(stdout.starts_with("key_id=k1 public_key="), "{stdout}");
    let secret_body: String = std::fs::read_to_string(&signing)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect();
    assert!(!stdout.contains(&secret_body) && !secret_body.is_empty());
    assert!(
        !run(&[
            "generate-key",
            "--out",
            signing.to_str().unwrap(),
            "--key-id",
            "k1",
            "--issuer",
            "studio"
        ])
        .status
        .success()
    );

    // A token for alice, bound to her device certificate.
    let issued = run(&[
        "issue-token",
        "--config",
        config.to_str().unwrap(),
        "--client-cert",
        dir.join("client.pem").to_str().unwrap(),
        "--subject",
        "alice",
        "--ttl",
        "600",
    ]);
    assert!(
        issued.status.success(),
        "{}",
        String::from_utf8_lossy(&issued.stderr)
    );
    let token_text = String::from_utf8(issued.stdout).unwrap();
    let token = decode_token_text(&token_text).unwrap();

    // A second process reuses the same key and token: replay nonces and sessions are in memory.
    for _ in 0..2 {
        let (mut server, addr) = spawn(&config);
        let limits = OnlineLimits::default();
        if transport == "quic" {
            let security = QuicClientSecurity::new(
                "localhost",
                [server_id.cert.clone()],
                vec![client_id.cert.clone()],
                client_id.key.clone_key(),
            )
            .unwrap();
            let endpoint =
                QuicClient::connect(addr, QuicClientConfig::default(), security).unwrap();
            let mut client = exercise(OnlineClient::new(endpoint, limits).unwrap(), &token);
            shutdown(&mut server, &mut client);
        } else {
            let security = ClientSecurity::new(
                "localhost",
                [server_id.cert.clone()],
                vec![client_id.cert.clone()],
                client_id.key.clone_key(),
            )
            .unwrap();
            let endpoint = TcpClient::connect_secure(TcpClientConfig::new(addr), security).unwrap();
            let mut client = exercise(OnlineClient::new(endpoint, limits).unwrap(), &token);
            shutdown(&mut server, &mut client);
        }
        let rest: String = server.lines.try_iter().collect();
        assert!(!rest.contains(token_text.trim()) && !rest.contains(&secret_body));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reference_server_serves_the_full_flow_over_quic() {
    full_cycle("quic");
}

#[test]
fn reference_server_serves_the_full_flow_over_tls_tcp() {
    full_cycle("tcp");
}

#[test]
fn a_token_for_a_different_device_certificate_is_refused_by_the_binary() {
    let dir = scratch("othercert");
    let server_id = Identity::new("localhost");
    let alice = Identity::new("alice-device");
    let mallory = Identity::new("mallory-device");
    write_identity(&dir, "server", &server_id);
    write_identity(&dir, "client", &alice);
    write_identity(&dir, "mallory", &mallory);
    // Both devices are trusted at the TLS layer; only alice's certificate is bound to the token.
    let roots = format!(
        "{}{}",
        pem::encode("CERTIFICATE", alice.cert.as_ref()),
        pem::encode("CERTIFICATE", mallory.cert.as_ref())
    );
    std::fs::write(dir.join("client.pem"), &roots).unwrap();
    std::fs::write(
        dir.join("alice-only.pem"),
        pem::encode("CERTIFICATE", alice.cert.as_ref()),
    )
    .unwrap();
    let config = dir.join("server.json");
    std::fs::write(&config, config_json("quic")).unwrap();
    let generated = run(&[
        "generate-key",
        "--out",
        dir.join("signing.pem").to_str().unwrap(),
        "--key-id",
        "k1",
        "--issuer",
        "studio",
    ]);
    assert!(generated.status.success());
    let issued = run(&[
        "issue-token",
        "--config",
        config.to_str().unwrap(),
        "--client-cert",
        dir.join("alice-only.pem").to_str().unwrap(),
        "--subject",
        "alice",
    ]);
    assert!(issued.status.success());
    let token = decode_token_text(&String::from_utf8(issued.stdout).unwrap()).unwrap();

    let (_server, addr) = spawn(&config);
    let security = QuicClientSecurity::new(
        "localhost",
        [server_id.cert.clone()],
        vec![mallory.cert.clone()],
        mallory.key.clone_key(),
    )
    .unwrap();
    let endpoint = QuicClient::connect(addr, QuicClientConfig::default(), security).unwrap();
    let mut client = OnlineClient::new(endpoint, OnlineLimits::default()).unwrap();
    let end = Instant::now() + Duration::from_secs(15);
    let mut hello = None;
    let mut refused = false;
    while !refused {
        assert!(Instant::now() < end, "timed out");
        for event in client.poll().unwrap() {
            match event {
                ClientEvent::Connected => hello = Some(client.hello(&token).unwrap()),
                ClientEvent::Response { id, body } if Some(id) == hello => {
                    assert!(
                        matches!(
                            body,
                            ServerBody::Error {
                                code: engine_online::protocol::ErrorCode::Unauthorized,
                                ..
                            }
                        ),
                        "{body:?}"
                    );
                    refused = true;
                }
                _ => {}
            }
        }
        thread::sleep(Duration::from_millis(1));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bad_invocations_and_configs_fail_without_leaking_secrets() {
    let dir = scratch("badconfig");
    assert!(!run(&[]).status.success());
    assert!(!run(&["frobnicate"]).status.success());
    assert!(!run(&["run"]).status.success());
    assert!(!run(&["run", "--config"]).status.success());
    assert!(
        !run(&["run", "--config", "a", "--config", "b"])
            .status
            .success()
    );
    assert!(!run(&["issue-token", "--bogus", "x"]).status.success());
    // Unknown config fields and missing key files are refused with a path, not contents.
    let config = dir.join("bad.json");
    std::fs::write(
        &config,
        config_json("quic").replace("\"listen\"", "\"surprise\": 1, \"listen\""),
    )
    .unwrap();
    let out = run(&["run", "--config", config.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid server config"));
    std::fs::write(&config, config_json("quic")).unwrap();
    std::fs::write(
        dir.join("signing.pem"),
        "-----BEGIN PRIVATE KEY-----\nc2VjcmV0LWJ5dGVz\n-----END PRIVATE KEY-----\n",
    )
    .unwrap();
    let out = run(&["run", "--config", config.to_str().unwrap()]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !stderr.contains("c2VjcmV0LWJ5dGVz") && !stderr.contains("secret-bytes"),
        "{stderr}"
    );
    // The server never starts listening with a bad config.
    assert!(out.stdout.is_empty());
    let _ = std::io::stdout().flush();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn occupied_listen_addresses_fail_before_the_ready_banner() {
    let dir = scratch("occupied");
    write_identity(&dir, "server", &Identity::new("localhost"));
    write_identity(&dir, "client", &Identity::new("alice-device"));
    assert!(
        run(&[
            "generate-key",
            "--out",
            dir.join("signing.pem").to_str().unwrap(),
            "--key-id",
            "k1",
            "--issuer",
            "studio",
        ])
        .status
        .success()
    );
    let config = dir.join("server.json");
    let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    for (transport, addr) in [
        ("quic", udp.local_addr().unwrap()),
        ("tcp", tcp.local_addr().unwrap()),
    ] {
        std::fs::write(
            &config,
            config_json(transport).replace("127.0.0.1:0", &addr.to_string()),
        )
        .unwrap();
        let out = run(&["run", "--config", config.to_str().unwrap()]);
        assert!(
            !out.status.success(),
            "occupied {transport} address was bound"
        );
        assert!(out.stdout.is_empty(), "failed startup advertised readiness");
        assert!(!out.stderr.is_empty(), "failed startup needs a diagnostic");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
