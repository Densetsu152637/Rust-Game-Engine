//! Shared test harnesses: a deterministic sans-IO simulator and a real loopback QUIC world.
#![allow(dead_code)]
use engine_net_quic::{
    QuicClient, QuicClientConfig, QuicClientSecurity, QuicServer, QuicServerConfig,
    QuicServerSecurity,
};
use engine_network::{CertificateDer, PeerId, PrivateKeyDer};
use engine_online::{
    CertFingerprint, CertificateBinding, ClientBody, ClientEnvelope, ClientEvent,
    InMemoryDirectory, Now, OnlineClient, OnlineLimits, OnlineServer, OnlineService, Scope,
    ServerBody, ServerEnvelope, ServiceConfig, TokenIssuer, TokenVerifier, TrustedKey,
    VerifierConfig, fingerprint, protocol::PROTOCOL_VERSION, service::Action, token::TokenRequest,
};
use rcgen::{CertifiedKey, KeyPair, generate_simple_self_signed};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    net::SocketAddr,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

pub const UNIX0: u64 = 1_800_000_000;

pub struct Identity {
    pub cert: CertificateDer<'static>,
    pub key: PrivateKeyDer<'static>,
}
impl Identity {
    pub fn new(name: &str) -> Self {
        let CertifiedKey { cert, signing_key }: CertifiedKey<KeyPair> =
            generate_simple_self_signed(vec![name.into()]).unwrap();
        Self {
            cert: cert.der().clone(),
            key: PrivateKeyDer::Pkcs8(signing_key.serialize_der().into()),
        }
    }
    pub fn fingerprint(&self) -> CertFingerprint {
        fingerprint(self.cert.as_ref())
    }
}

/// A token-signing authority plus the matching verifier (what a game server would hold).
pub struct Authority {
    pkcs8: Vec<u8>,
    pub key_id: String,
}
impl Authority {
    pub fn new(key_id: &str) -> Self {
        Self {
            pkcs8: TokenIssuer::generate_pkcs8().unwrap().to_vec(),
            key_id: key_id.into(),
        }
    }
    pub fn issuer(&self) -> TokenIssuer {
        TokenIssuer::from_pkcs8(&self.key_id, "test-issuer", &self.pkcs8, 24 * 3600).unwrap()
    }
    pub fn trusted(&self) -> TrustedKey {
        self.issuer().trusted_key()
    }
    pub fn verifier(&self) -> Arc<TokenVerifier> {
        Arc::new(TokenVerifier::new(VerifierConfig::default(), [self.trusted()]).unwrap())
    }
    pub fn token(
        &self,
        audience: &str,
        subject: &str,
        cert: CertFingerprint,
        ttl: u64,
        now_unix: u64,
    ) -> Vec<u8> {
        self.issuer()
            .issue(
                &TokenRequest {
                    audience,
                    subject,
                    session: None,
                    cert_sha256: cert,
                    lifetime_secs: ttl,
                    scopes: vec![Scope::Connect],
                },
                now_unix,
            )
            .unwrap()
    }
}

pub fn service_config(mutate: impl FnOnce(&mut ServiceConfig)) -> ServiceConfig {
    let mut cfg = ServiceConfig::default();
    cfg.limits.match_interval_ms = 50;
    cfg.relay_audience = Some("engine-relay".into());
    mutate(&mut cfg);
    cfg
}

pub fn service(auth: &Authority, mutate: impl FnOnce(&mut ServiceConfig)) -> OnlineService {
    OnlineService::new(service_config(mutate), auth.issuer(), auth.verifier()).unwrap()
}

// ---------------------------------------------------------------------------------------------
// Sans-IO simulator
// ---------------------------------------------------------------------------------------------

pub struct Sim {
    pub service: OnlineService,
    pub auth: Authority,
    pub mono: u64,
    pub next_id: u32,
    pub inbox: BTreeMap<PeerId, VecDeque<ServerEnvelope>>,
    pub disconnected: BTreeSet<PeerId>,
    pub prints: BTreeMap<PeerId, CertFingerprint>,
    pub limits: OnlineLimits,
}
impl Sim {
    pub fn new(mutate: impl FnOnce(&mut ServiceConfig)) -> Self {
        Self::build(mutate, None)
    }
    pub fn with_directory(
        mutate: impl FnOnce(&mut ServiceConfig),
        directory: InMemoryDirectory,
    ) -> Self {
        Self::build(mutate, Some(directory))
    }
    fn build(
        mutate: impl FnOnce(&mut ServiceConfig),
        directory: Option<InMemoryDirectory>,
    ) -> Self {
        let auth = Authority::new("sim-key");
        let cfg = service_config(mutate);
        let limits = cfg.limits.clone();
        let mut service = OnlineService::new(cfg, auth.issuer(), auth.verifier()).unwrap();
        if let Some(directory) = directory {
            service = service.with_directory(Box::new(directory));
        }
        Self {
            service,
            auth,
            mono: 0,
            next_id: 1,
            inbox: BTreeMap::new(),
            disconnected: BTreeSet::new(),
            prints: BTreeMap::new(),
            limits,
        }
    }
    pub fn now(&self) -> Now {
        Now {
            mono_ms: self.mono,
            unix_secs: UNIX0 + self.mono / 1000,
        }
    }
    pub fn advance(&mut self, ms: u64) {
        self.mono += ms;
        let now = self.now();
        self.service.tick(now);
        self.pump();
    }
    pub fn print_for(name: &str) -> CertFingerprint {
        fingerprint(format!("sim-cert-{name}").as_bytes())
    }
    /// Drains service actions into per-peer inboxes (decoding and size-checking every reply).
    pub fn pump(&mut self) {
        for action in self.service.drain_actions() {
            match action {
                Action::Send { peer, payload } => {
                    assert!(payload.len() <= self.limits.max_message_bytes);
                    let envelope = ServerEnvelope::decode(&payload, &self.limits)
                        .expect("service must only emit valid protocol messages");
                    self.inbox.entry(peer).or_default().push_back(envelope);
                }
                Action::Disconnect { peer, .. } => {
                    self.disconnected.insert(peer);
                }
            }
        }
    }
    /// Transport admitted `peer` presenting the certificate identified by `name`.
    pub fn admit(&mut self, peer: PeerId, name: &str) {
        let print = Self::print_for(name);
        self.prints.insert(peer, print);
        self.service.on_connected(peer, Some(print), self.now());
        self.pump();
    }
    pub fn connect_token(&self, name: &str, subject: &str, ttl: u64) -> Vec<u8> {
        self.auth.token(
            "engine-online",
            subject,
            Self::print_for(name),
            ttl,
            self.now().unix_secs,
        )
    }
    /// Admits and authenticates a player; the cert name equals the subject.
    pub fn login(&mut self, peer: PeerId, name: &str) {
        self.admit(peer, name);
        let token = self.connect_token(name, name, 3600);
        match self.request(
            peer,
            ClientBody::Hello {
                token: engine_online::protocol::Blob(token),
            },
        ) {
            ServerBody::HelloOk { subject, .. } => assert_eq!(subject, name),
            other => panic!("login failed: {other:?}"),
        }
    }
    pub fn send_raw(&mut self, peer: PeerId, payload: &[u8]) {
        self.service.on_message(peer, payload, self.now());
        self.pump();
    }
    /// Sends a request and returns the response with the matching ID (others stay queued).
    pub fn request(&mut self, peer: PeerId, body: ClientBody) -> ServerBody {
        let id = self.next_id;
        self.next_id += 1;
        let payload = ClientEnvelope {
            v: PROTOCOL_VERSION,
            id,
            body,
        }
        .encode(&self.limits)
        .unwrap();
        self.send_raw(peer, &payload);
        let queue = self.inbox.entry(peer).or_default();
        let position = queue
            .iter()
            .position(|e| e.id == id)
            .unwrap_or_else(|| panic!("no response to request {id} for peer {peer}"));
        queue.remove(position).unwrap().body
    }
    /// Takes every notification (and stray reply) queued for `peer`.
    pub fn take(&mut self, peer: PeerId) -> Vec<ServerBody> {
        self.inbox
            .entry(peer)
            .or_default()
            .drain(..)
            .map(|e| e.body)
            .collect()
    }
    pub fn drop_peer(&mut self, peer: PeerId) {
        self.service.on_disconnected(peer, self.now());
        self.pump();
    }
}

pub fn expect_error(body: ServerBody) -> engine_online::protocol::ErrorCode {
    match body {
        ServerBody::Error { code, .. } => code,
        other => panic!("expected an error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// Real loopback QUIC
// ---------------------------------------------------------------------------------------------

pub struct Net {
    pub server: OnlineServer<QuicServer>,
    pub auth: Authority,
    pub server_cert: CertificateDer<'static>,
    pub ids: Vec<Identity>,
    pub addr: SocketAddr,
    pub limits: OnlineLimits,
    pub binding: CertificateBinding,
}
pub struct Conn {
    pub client: OnlineClient<QuicClient>,
    pub idx: usize,
    pub events: Vec<ClientEvent>,
}
impl Conn {
    fn pump(&mut self) {
        let events = self.client.poll().expect("client poll");
        self.events.extend(events);
    }
    pub fn connected(&self) -> bool {
        self.events
            .iter()
            .any(|e| matches!(e, ClientEvent::Connected))
    }
    pub fn disconnected(&self) -> bool {
        self.events
            .iter()
            .any(|e| matches!(e, ClientEvent::Disconnected(_)))
    }
    pub fn take_response(&mut self, id: u32) -> Option<ServerBody> {
        let position = self
            .events
            .iter()
            .position(|e| matches!(e, ClientEvent::Response { id: got, .. } if *got == id))?;
        match self.events.remove(position) {
            ClientEvent::Response { body, .. } => Some(body),
            _ => None,
        }
    }
    pub fn take_notifications(&mut self) -> Vec<ServerBody> {
        let mut out = Vec::new();
        self.events.retain(|e| match e {
            ClientEvent::Notification(body) => {
                out.push(body.clone());
                false
            }
            _ => true,
        });
        out
    }
}

impl Net {
    /// `clients` trusted client identities; every client certificate is a pinned root.
    pub fn new(clients: usize, mutate: impl FnOnce(&mut ServiceConfig)) -> Self {
        Self::with_untrusted(clients, 0, mutate)
    }
    pub fn with_untrusted(
        clients: usize,
        _untrusted: usize,
        mutate: impl FnOnce(&mut ServiceConfig),
    ) -> Self {
        let server_id = Identity::new("localhost");
        let ids: Vec<Identity> = (0..clients)
            .map(|i| Identity::new(&format!("player{i}")))
            .collect();
        let auth = Authority::new("net-key");
        let cfg = service_config(mutate);
        let limits = cfg.limits.clone();
        let binding = CertificateBinding::new(64, Duration::from_secs(30));
        let security = QuicServerSecurity::new(
            vec![server_id.cert.clone()],
            server_id.key.clone_key(),
            ids.iter().map(|i| i.cert.clone()),
            binding.authorize_verified(),
        )
        .unwrap();
        let endpoint = QuicServer::bind(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            QuicServerConfig::default(),
            security,
        )
        .unwrap();
        let addr = endpoint.local_addr().unwrap();
        let service = OnlineService::new(cfg, auth.issuer(), auth.verifier()).unwrap();
        Self {
            server: OnlineServer::new(endpoint, service, binding.clone()),
            auth,
            server_cert: server_id.cert,
            ids,
            addr,
            limits,
            binding,
        }
    }
    pub fn connect(&self, idx: usize) -> Conn {
        let id = &self.ids[idx];
        let security = QuicClientSecurity::new(
            "localhost",
            [self.server_cert.clone()],
            vec![id.cert.clone()],
            id.key.clone_key(),
        )
        .unwrap();
        let endpoint =
            QuicClient::connect(self.addr, QuicClientConfig::default(), security).unwrap();
        Conn {
            client: OnlineClient::new(endpoint, self.limits.clone()).unwrap(),
            idx,
            events: Vec::new(),
        }
    }
    pub fn unix_now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
    pub fn token_for(&self, idx: usize, subject: &str, ttl: u64) -> Vec<u8> {
        self.auth.token(
            "engine-online",
            subject,
            self.ids[idx].fingerprint(),
            ttl,
            Self::unix_now(),
        )
    }
    pub fn step(&mut self, conns: &mut [Conn]) {
        self.server.poll().expect("server poll");
        for conn in conns.iter_mut() {
            conn.pump();
        }
        thread::sleep(Duration::from_millis(1));
    }
    /// Drives the world until `done` holds (10 s deadline).
    pub fn drive(&mut self, conns: &mut [Conn], mut done: impl FnMut(&mut [Conn]) -> bool) {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            self.step(conns);
            if done(conns) {
                return;
            }
            assert!(Instant::now() < end, "loopback world timed out");
        }
    }
    pub fn connect_all(&mut self, conns: &mut [Conn]) {
        self.drive(conns, |c| c.iter().all(Conn::connected));
    }
    /// Sends `body` from `conns[i]` and waits for its response.
    pub fn call(&mut self, conns: &mut [Conn], i: usize, body: ClientBody) -> ServerBody {
        let id = conns[i].client.request(body).expect("send request");
        let mut response = None;
        self.drive(conns, |c| {
            response = c[i].take_response(id);
            response.is_some()
        });
        response.unwrap()
    }
    /// Authenticates `conns[i]` with a fresh token whose subject is `player{idx}`.
    pub fn login(&mut self, conns: &mut [Conn], i: usize) -> ServerBody {
        let idx = conns[i].idx;
        let token = self.token_for(idx, &format!("player{idx}"), 600);
        let id = conns[i].client.hello(&token).expect("hello");
        let mut response = None;
        self.drive(conns, |c| {
            response = c[i].take_response(id);
            response.is_some()
        });
        response.unwrap()
    }
}
