//! JSON configuration and construction of the reference server (`engine-online-server`).
//!
//! The file holds *paths* to the TLS chain, TLS key, client trust roots and token signing key,
//! never key material itself, and nothing in this module prints or logs file contents. Relative
//! paths resolve against the directory of the config file. Unknown fields are rejected.
use crate::{
    CertificateBinding, OnlineError, OnlineServer, ServiceConfig,
    limits::OnlineLimits,
    matchmaking::{ModeRules, default_modes},
    pem,
    service::OnlineService,
    token::{TokenIssuer, TokenRequest, TokenVerifier, TrustedKey, VerifierConfig, fingerprint},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use engine_net_quic::{QuicLimits, QuicServer, QuicServerConfig, QuicServerSecurity};
use engine_network::{
    Delivery, Lane, LinkEvent, LinkStats, MessageEndpoint, PeerId, PollBudget, Server,
    ServerConfig as TcpServerConfig, ServerSecurity, TransportKind, TransportSecurity,
};
use serde::Deserialize;
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportChoice {
    #[default]
    Quic,
    Tcp,
}

/// TLS material locations. The server key file is a secret: protect it with file permissions.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsFiles {
    /// Server certificate chain (PEM, leaf first) clients verify.
    pub server_chain: PathBuf,
    /// Server private key (PEM or PKCS#8 DER).
    pub server_key: PathBuf,
    /// Trust roots for client certificates (PEM bundle). A pinned self-signed client
    /// certificate may itself be a root.
    pub client_roots: PathBuf,
}

/// A public key this server accepts connect tokens from (an external identity provider, or the
/// previous key during rotation).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyKey {
    pub key_id: String,
    pub issuer: String,
    /// 32-byte Ed25519 public key, base64url without padding.
    pub public_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenSettings {
    /// Ed25519 PKCS#8 signing key (PEM `PRIVATE KEY` or DER). A secret.
    pub signing_key: PathBuf,
    pub key_id: String,
    pub issuer: String,
    #[serde(default = "default_online_audience")]
    pub online_audience: String,
    #[serde(default = "default_game_audience")]
    pub game_audience: String,
    #[serde(default)]
    pub relay_audience: Option<String>,
    #[serde(default = "default_join_ttl")]
    pub join_token_ttl_secs: u64,
    /// Lifetime of relay credentials (also the hard end of the relay allocation made with one).
    #[serde(default = "default_relay_ttl")]
    pub relay_token_ttl_secs: u64,
    #[serde(default = "yes")]
    pub connect_token_single_use: bool,
    #[serde(default = "default_max_lifetime")]
    pub max_token_lifetime_secs: u64,
    #[serde(default = "default_skew")]
    pub clock_skew_secs: u64,
    #[serde(default = "default_replay_entries")]
    pub replay_cache_entries: usize,
    /// Most live replay entries one certificate fingerprint may hold (its own oldest are evicted).
    #[serde(default = "default_replay_per_fingerprint")]
    pub replay_per_fingerprint: usize,
    /// Additional trusted verification keys (the signing key's own public half is implicit).
    #[serde(default)]
    pub verify_keys: Vec<VerifyKey>,
}
fn default_online_audience() -> String {
    "engine-online".into()
}
fn default_game_audience() -> String {
    "engine-game".into()
}
fn default_join_ttl() -> u64 {
    120
}
fn default_relay_ttl() -> u64 {
    3600
}
fn default_replay_per_fingerprint() -> usize {
    1024
}
fn yes() -> bool {
    true
}
fn default_max_lifetime() -> u64 {
    24 * 3600
}
fn default_skew() -> u64 {
    30
}
fn default_replay_entries() -> usize {
    65_536
}
fn default_listen() -> String {
    "127.0.0.1:0".into()
}
fn default_max_connections() -> usize {
    256
}
fn default_mode_list() -> Vec<ModeRules> {
    default_modes()
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerFileConfig {
    /// Bind address. Defaults to loopback with an OS-assigned port; exposing the service is an
    /// explicit operator decision.
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default)]
    pub transport: TransportChoice,
    pub tls: TlsFiles,
    pub tokens: TokenSettings,
    /// Transport connections (admitted + handshaking) accepted at once.
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    #[serde(default)]
    pub limits: OnlineLimits,
    #[serde(default = "default_mode_list")]
    pub modes: Vec<ModeRules>,
}
impl ServerFileConfig {
    /// Parses config JSON; relative paths resolve against `base_dir`.
    pub fn from_json(bytes: &[u8], base_dir: &Path) -> Result<Self, OnlineError> {
        let mut config: Self = serde_json::from_slice(bytes)
            .map_err(|e| OnlineError::Config(format!("invalid server config: {e}")))?;
        for path in [
            &mut config.tls.server_chain,
            &mut config.tls.server_key,
            &mut config.tls.client_roots,
            &mut config.tokens.signing_key,
        ] {
            if path.is_relative() {
                *path = base_dir.join(&*path);
            }
        }
        config.validate()?;
        Ok(config)
    }
    pub fn load(path: &Path) -> Result<Self, OnlineError> {
        let bytes = pem::read_file(path)?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        Self::from_json(&bytes, base)
    }
    pub fn listen_addr(&self) -> Result<SocketAddr, OnlineError> {
        self.listen
            .parse()
            .map_err(|_| OnlineError::Config("listen must be a socket address".into()))
    }
    fn validate(&self) -> Result<(), OnlineError> {
        self.listen_addr()?;
        self.limits.validate()?;
        if self.max_connections == 0 || self.max_connections > 4096 {
            return Err(OnlineError::Config(
                "max_connections must be 1..=4096".into(),
            ));
        }
        if self.tokens.join_token_ttl_secs == 0
            || self.tokens.relay_token_ttl_secs == 0
            || self.tokens.max_token_lifetime_secs == 0
        {
            return Err(OnlineError::Config(
                "token lifetimes must be nonzero".into(),
            ));
        }
        Ok(())
    }
}

/// Encodes a public key for configuration files (base64url, no padding).
pub fn encode_public_key(key: &TrustedKey) -> String {
    URL_SAFE_NO_PAD.encode(key.public_key())
}
fn decode_public_key(text: &str) -> Result<[u8; 32], OnlineError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(text.as_bytes())
        .map_err(|_| OnlineError::Config("public key is not base64url".into()))?;
    <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| OnlineError::Config("public key must be 32 bytes".into()))
}

/// Server endpoint for either transport behind one `MessageEndpoint`.
pub enum AnyEndpoint {
    Quic(Box<QuicServer>),
    Tcp(Box<Server>),
}
impl AnyEndpoint {
    pub fn local_addr(&self) -> Result<SocketAddr, OnlineError> {
        match self {
            Self::Quic(server) => server.local_addr(),
            Self::Tcp(server) => server.local_addr(),
        }
        .map_err(OnlineError::from)
    }
}
impl MessageEndpoint for AnyEndpoint {
    fn transport(&self) -> TransportKind {
        match self {
            Self::Quic(e) => e.transport(),
            Self::Tcp(e) => MessageEndpoint::transport(e.as_ref()),
        }
    }
    fn security(&self) -> TransportSecurity {
        match self {
            Self::Quic(e) => e.security(),
            Self::Tcp(e) => MessageEndpoint::security(e.as_ref()),
        }
    }
    fn connected_peers(&self) -> Vec<PeerId> {
        match self {
            Self::Quic(e) => e.connected_peers(),
            Self::Tcp(e) => MessageEndpoint::connected_peers(e.as_ref()),
        }
    }
    fn max_payload(&self, peer: PeerId, delivery: Delivery) -> Option<usize> {
        match self {
            Self::Quic(e) => e.max_payload(peer, delivery),
            Self::Tcp(e) => MessageEndpoint::max_payload(e.as_ref(), peer, delivery),
        }
    }
    fn send(
        &mut self,
        peer: PeerId,
        lane: Lane,
        delivery: Delivery,
        payload: &[u8],
    ) -> engine_network::Result<()> {
        match self {
            Self::Quic(e) => e.send(peer, lane, delivery, payload),
            Self::Tcp(e) => MessageEndpoint::send(e.as_mut(), peer, lane, delivery, payload),
        }
    }
    fn poll(&mut self, budget: PollBudget) -> engine_network::Result<Vec<LinkEvent>> {
        match self {
            Self::Quic(e) => e.poll(budget),
            Self::Tcp(e) => MessageEndpoint::poll(e.as_mut(), budget),
        }
    }
    fn stats(&self, peer: PeerId) -> Option<LinkStats> {
        match self {
            Self::Quic(e) => e.stats(peer),
            Self::Tcp(e) => MessageEndpoint::stats(e.as_ref(), peer),
        }
    }
    fn disconnect(&mut self, peer: PeerId) -> bool {
        match self {
            Self::Quic(e) => e.disconnect(peer),
            Self::Tcp(e) => MessageEndpoint::disconnect(e.as_mut(), peer),
        }
    }
}

/// A constructed reference server and the public facts an operator needs to print.
pub struct BuiltServer {
    pub server: OnlineServer<AnyEndpoint>,
    pub local_addr: SocketAddr,
    /// Public half of the signing key, for game servers and relays.
    pub public_key: TrustedKey,
}

fn load_issuer(config: &ServerFileConfig) -> Result<TokenIssuer, OnlineError> {
    pem::ensure_owner_only(&config.tokens.signing_key)?;
    let pkcs8 = pem::load_pkcs8(&config.tokens.signing_key)?;
    TokenIssuer::from_pkcs8(
        config.tokens.key_id.clone(),
        config.tokens.issuer.clone(),
        &pkcs8,
        config.tokens.max_token_lifetime_secs,
    )
    .map_err(|_| {
        OnlineError::Config(format!(
            "invalid signing key or ids in {}",
            config.tokens.signing_key.display()
        ))
    })
}

/// Binds the server described by `config` (a socket is opened here and nowhere earlier).
pub fn build_server(config: &ServerFileConfig) -> Result<BuiltServer, OnlineError> {
    let issuer = load_issuer(config)?;
    let public_key = issuer.trusted_key();
    let mut keys = vec![public_key.clone()];
    for extra in &config.tokens.verify_keys {
        keys.push(TrustedKey::new(
            extra.key_id.clone(),
            extra.issuer.clone(),
            decode_public_key(&extra.public_key)?,
        )?);
    }
    let verifier = Arc::new(TokenVerifier::new(
        VerifierConfig {
            max_lifetime_secs: config.tokens.max_token_lifetime_secs,
            clock_skew_secs: config.tokens.clock_skew_secs,
            replay_capacity: config.tokens.replay_cache_entries,
            replay_per_fingerprint: config.tokens.replay_per_fingerprint,
        },
        keys,
    )?);
    let service = OnlineService::new(
        ServiceConfig {
            limits: config.limits.clone(),
            online_audience: config.tokens.online_audience.clone(),
            game_audience: config.tokens.game_audience.clone(),
            relay_audience: config.tokens.relay_audience.clone(),
            join_token_ttl_secs: config.tokens.join_token_ttl_secs,
            relay_token_ttl_secs: config.tokens.relay_token_ttl_secs,
            connect_token_single_use: config.tokens.connect_token_single_use,
            modes: config.modes.clone(),
        },
        issuer,
        verifier,
    )?;
    let binding = CertificateBinding::new(
        config.max_connections.saturating_mul(2),
        Duration::from_secs(30),
    );
    let chain = pem::load_certificates(&config.tls.server_chain)?;
    pem::ensure_owner_only(&config.tls.server_key)?;
    let key = pem::load_private_key(&config.tls.server_key)?;
    let roots = pem::load_certificates(&config.tls.client_roots)?;
    let addr = config.listen_addr()?;
    let endpoint = match config.transport {
        TransportChoice::Quic => {
            let security =
                QuicServerSecurity::new(chain, key, roots, binding.authorize_verified())?;
            let limits = QuicLimits {
                max_peers: config.max_connections,
                max_message_bytes: config.limits.max_message_bytes,
                ..QuicLimits::default()
            };
            AnyEndpoint::Quic(Box::new(QuicServer::bind(
                addr,
                QuicServerConfig {
                    limits,
                    ..QuicServerConfig::default()
                },
                security,
            )?))
        }
        TransportChoice::Tcp => {
            let security = ServerSecurity::new(chain, key, roots, binding.authorize_verified())?;
            let limits = engine_network::Limits {
                max_peers: config.max_connections,
                max_frame_bytes: config.limits.max_message_bytes + 1024,
                ..engine_network::Limits::default()
            };
            AnyEndpoint::Tcp(Box::new(Server::bind_secure(
                TcpServerConfig {
                    bind_addr: addr,
                    limits,
                },
                security,
            )?))
        }
    };
    let local_addr = endpoint.local_addr()?;
    Ok(BuiltServer {
        server: OnlineServer::new(endpoint, service, binding),
        local_addr,
        public_key,
    })
}

/// Writes a new Ed25519 signing key (PEM, mode 0600 on Unix) and returns its public half. Refuses
/// to overwrite an existing file.
pub fn generate_signing_key(
    path: &Path,
    key_id: &str,
    issuer: &str,
) -> Result<TrustedKey, OnlineError> {
    use std::io::Write;
    let pkcs8 = TokenIssuer::generate_pkcs8()?;
    let public = TokenIssuer::from_pkcs8(key_id, issuer, &pkcs8, 1)?.trusted_key();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|_| OnlineError::Config(format!("cannot create {}", path.display())))?;
    let armored = zeroize::Zeroizing::new(pem::encode("PRIVATE KEY", &pkcs8));
    file.write_all(armored.as_bytes())?;
    Ok(public)
}

/// Mints a connect token for the holder of `client_certificate` (leaf = first certificate in the
/// file). Intended for development and operations; production deployments run an account
/// service that calls [`TokenIssuer::issue`] after real authentication.
pub fn issue_connect_token(
    config: &ServerFileConfig,
    client_certificate: &Path,
    subject: &str,
    ttl_secs: u64,
    now_unix: u64,
) -> Result<String, OnlineError> {
    let issuer = load_issuer(config)?;
    let leaf = pem::load_certificates(client_certificate)?
        .into_iter()
        .next()
        .ok_or(OnlineError::Invalid("no certificate"))?;
    let token = issuer.issue(
        &TokenRequest {
            audience: &config.tokens.online_audience,
            subject,
            session: None,
            cert_sha256: fingerprint(leaf.as_ref()),
            lifetime_secs: ttl_secs,
            scopes: vec![crate::token::Scope::Connect],
        },
        now_unix,
    )?;
    Ok(URL_SAFE_NO_PAD.encode(token))
}

/// Decodes a token printed by [`issue_connect_token`].
pub fn decode_token_text(text: &str) -> Result<Vec<u8>, OnlineError> {
    URL_SAFE_NO_PAD
        .decode(text.trim().as_bytes())
        .map_err(|_| OnlineError::Invalid("token is not base64url"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"{
        "tls": {"server_chain": "chain.pem", "server_key": "key.pem", "client_roots": "roots.pem"},
        "tokens": {"signing_key": "sign.pem", "key_id": "k1", "issuer": "dev"}
    }"#;

    #[test]
    fn defaults_are_loopback_quic_and_paths_resolve_against_the_config_directory() {
        let base = Path::new("/etc/engine");
        let config = ServerFileConfig::from_json(MINIMAL.as_bytes(), base).unwrap();
        assert_eq!(
            config.listen_addr().unwrap(),
            "127.0.0.1:0".parse().unwrap()
        );
        assert_eq!(config.transport, TransportChoice::Quic);
        assert_eq!(config.tls.server_key, base.join("key.pem"));
        assert_eq!(config.tokens.signing_key, base.join("sign.pem"));
        assert!(config.tokens.connect_token_single_use);
        assert!(!config.modes.is_empty());
    }

    #[test]
    fn unknown_fields_bad_addresses_and_bad_limits_are_rejected() {
        let base = Path::new(".");
        for bad in [
            MINIMAL.replace("\"tls\"", "\"surprise\": 1, \"tls\""),
            MINIMAL.replace("\"tls\"", "\"listen\": \"not an address\", \"tls\""),
            MINIMAL.replace("\"tls\"", "\"max_connections\": 0, \"tls\""),
            MINIMAL.replace("\"tls\"", "\"limits\": {\"max_capacity\": 0}, \"tls\""),
            MINIMAL.replace("\"tls\"", "\"limits\": {\"bogus\": 1}, \"tls\""),
            MINIMAL.replace("\"tls\"", "\"transport\": \"udp\", \"tls\""),
            "{}".to_owned(),
            String::new(),
        ] {
            assert!(
                ServerFileConfig::from_json(bad.as_bytes(), base).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn public_keys_round_trip_and_reject_wrong_lengths() {
        let key = TrustedKey::new("k", "i", [9; 32]).unwrap();
        assert_eq!(
            decode_public_key(&encode_public_key(&key)).unwrap(),
            [9; 32]
        );
        assert!(decode_public_key("AAAA").is_err());
        assert!(decode_public_key("not base64!").is_err());
    }
}
