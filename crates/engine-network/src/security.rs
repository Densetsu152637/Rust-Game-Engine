//! TLS configuration is deliberately opaque: callers cannot disable the engine's TLS invariants.
//! The default constructors build the whole rustls configuration; `from_rustls_config` accepts a
//! caller-built configuration whose verifiers become trusted configuration, and then overrides
//! every setting the endpoint can enforce (ALPN, early data, tickets, resumption, key logging).
use crate::{CertificateDer, Error, PrivateKeyDer, Result};
use rustls::{ClientConfig, RootCertStore, ServerConfig, pki_types::ServerName};
use serde::Serialize;
use std::{sync::Arc, time::Duration};

/// ALPN protocol identifier of the framed TCP transport.
pub const TCP_ALPN: &[u8] = b"rust-game-engine/1";
pub(crate) type Authorize = dyn Fn(crate::PeerId, &[CertificateDer<'_>]) -> bool + Send + Sync;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportSecurity {
    TrustedLanPlaintext,
    MutualTls13,
}

/// Trusted configuration. Rate windows reset once per second; a boundary can admit two bursts.
#[derive(Clone, Debug)]
pub struct SecurityLimits {
    pub handshake_timeout: Duration,
    pub max_handshake_bytes: usize,
    pub max_tls_steps_per_poll: usize,
    pub max_received_bytes_per_second: usize,
    pub max_messages_per_second: usize,
}
impl Default for SecurityLimits {
    fn default() -> Self {
        Self {
            handshake_timeout: Duration::from_secs(5),
            max_handshake_bytes: 128 * 1024,
            max_tls_steps_per_poll: 8,
            max_received_bytes_per_second: 2 * 1024 * 1024,
            max_messages_per_second: 256,
        }
    }
}
impl SecurityLimits {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.handshake_timeout.is_zero()
            || self.max_handshake_bytes == 0
            || self.max_tls_steps_per_poll == 0
            || self.max_received_bytes_per_second == 0
            || self.max_messages_per_second == 0
        {
            return Err(Error::Invalid("invalid security limits"));
        }
        Ok(())
    }
}

/// Native secret injection point. No Debug/Serialize implementation exposes keys or identities.
/// Policy receives the assigned peer ID and TLS-verified chain exactly once, before Connected or gameplay delivery.
/// It must be fast, nonblocking and fail closed; map a certificate to your application allowlist.
#[derive(Clone)]
pub struct ServerSecurity {
    pub(crate) config: Arc<ServerConfig>,
    pub(crate) authorize: Arc<Authorize>,
    pub limits: SecurityLimits,
}
impl ServerSecurity {
    pub fn new(
        chain: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        client_roots: impl IntoIterator<Item = CertificateDer<'static>>,
        authorize: impl Fn(crate::PeerId, &[CertificateDer<'_>]) -> bool + Send + Sync + 'static,
    ) -> Result<Self> {
        let config = tls::server_config(chain, key, client_roots, TCP_ALPN)?;
        Ok(Self::with_config(config, authorize))
    }
    /// Uses a caller-built rustls server configuration. The caller's client-certificate verifier,
    /// certificate resolver and crypto provider become trusted configuration: the engine cannot
    /// prove that they verify anything. The endpoint still overrides ALPN, disables early data,
    /// tickets, session resumption, key logging and secret extraction (see [`tls::harden_server`]),
    /// closes every connection that does not negotiate TLS 1.3 with the engine ALPN, rejects
    /// connections without a presented client certificate chain, and runs `authorize` exactly
    /// once before Connected.
    pub fn from_rustls_config(
        config: Arc<ServerConfig>,
        authorize: impl Fn(crate::PeerId, &[CertificateDer<'_>]) -> bool + Send + Sync + 'static,
    ) -> Result<Self> {
        Ok(Self::with_config(
            tls::harden_server(config, TCP_ALPN)?,
            authorize,
        ))
    }
    fn with_config(
        config: Arc<ServerConfig>,
        authorize: impl Fn(crate::PeerId, &[CertificateDer<'_>]) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            config,
            authorize: Arc::new(authorize),
            limits: SecurityLimits::default(),
        }
    }
}

#[derive(Clone)]
pub struct ClientSecurity {
    pub(crate) config: Arc<ClientConfig>,
    pub(crate) server_name: ServerName<'static>,
    pub limits: SecurityLimits,
}
impl ClientSecurity {
    /// The DNS name (or IP SAN) must match the verified server certificate, independently of address.
    pub fn new(
        server_name: impl Into<String>,
        server_roots: impl IntoIterator<Item = CertificateDer<'static>>,
        chain: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
    ) -> Result<Self> {
        Self::with_config(
            tls::client_config(server_roots, chain, key, TCP_ALPN)?,
            server_name,
        )
    }
    /// Uses a caller-built rustls client configuration. Its server-certificate verifier and
    /// client-certificate resolver become trusted configuration. A configuration without client
    /// certificates is rejected; ALPN, early data, resumption and key logging are overridden
    /// (see [`tls::harden_client`]) and TLS 1.3 + ALPN is still checked after the handshake.
    pub fn from_rustls_config(
        config: Arc<ClientConfig>,
        server_name: impl Into<String>,
    ) -> Result<Self> {
        Self::with_config(tls::harden_client(config, TCP_ALPN)?, server_name)
    }
    fn with_config(config: Arc<ClientConfig>, server_name: impl Into<String>) -> Result<Self> {
        Ok(Self {
            config,
            server_name: tls::server_name(server_name)?,
            limits: SecurityLimits::default(),
        })
    }
}

/// TLS policy shared by every engine transport (framed TCP here, QUIC in `engine-net-quic`).
/// Functions take the transport's ALPN identifier so each transport keeps its own protocol name.
pub mod tls {
    use super::*;
    use rustls::{
        KeyLog, NoKeyLog,
        server::{NoServerSessionStorage, ProducesTickets, WebPkiClientVerifier},
    };

    /// Builds the engine's default server configuration: ring provider, TLS 1.3 only,
    /// mandatory WebPKI client certificates from `client_roots`, hardened per [`harden_server`].
    pub fn server_config(
        chain: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        client_roots: impl IntoIterator<Item = CertificateDer<'static>>,
        alpn: &[u8],
    ) -> Result<Arc<ServerConfig>> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = WebPkiClientVerifier::builder_with_provider(
            Arc::new(roots(client_roots)?),
            provider.clone(),
        )
        .build()
        .map_err(|_| Error::Invalid("invalid client trust roots"))?;
        let config = ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|_| Error::Invalid("TLS 1.3 unavailable"))?
            .with_client_cert_verifier(verifier)
            .with_single_cert(chain, key)
            .map_err(|_| Error::Invalid("invalid server certificate or key"))?;
        harden_server(Arc::new(config), alpn)
    }

    /// Builds the engine's default client configuration: ring provider, TLS 1.3 only, WebPKI
    /// server verification against `server_roots`, a mandatory client certificate.
    pub fn client_config(
        server_roots: impl IntoIterator<Item = CertificateDer<'static>>,
        chain: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        alpn: &[u8],
    ) -> Result<Arc<ClientConfig>> {
        let config =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_protocol_versions(&[&rustls::version::TLS13])
                .map_err(|_| Error::Invalid("TLS 1.3 unavailable"))?
                .with_root_certificates(roots(server_roots)?)
                .with_client_auth_cert(chain, key)
                .map_err(|_| Error::Invalid("invalid client certificate or key"))?;
        harden_client(Arc::new(config), alpn)
    }

    /// Copies a server configuration and enforces the endpoint-controlled invariants: exactly one
    /// ALPN protocol, no 0-RTT or half-RTT data, no TLS 1.3 tickets or stateful resumption (each
    /// connection freshly proves key possession and runs policy), no key logging and no secret
    /// extraction. Protocol version and client-certificate presence are checked per connection.
    pub fn harden_server(config: Arc<ServerConfig>, alpn: &[u8]) -> Result<Arc<ServerConfig>> {
        if alpn.is_empty() {
            return Err(Error::Invalid("ALPN must not be empty"));
        }
        let mut config = Arc::unwrap_or_clone(config);
        config.alpn_protocols = vec![alpn.to_vec()];
        config.max_early_data_size = 0;
        config.send_half_rtt_data = false;
        config.send_tls13_tickets = 0;
        config.session_storage = Arc::new(NoServerSessionStorage {});
        config.ticketer = Arc::new(NoTickets);
        config.key_log = no_key_log();
        config.enable_secret_extraction = false;
        Ok(Arc::new(config))
    }

    /// Copies a client configuration, rejects one that cannot present a client certificate, and
    /// enforces one ALPN protocol (checked against the server's selection), no early data, no
    /// session resumption, no key logging and no secret extraction.
    pub fn harden_client(config: Arc<ClientConfig>, alpn: &[u8]) -> Result<Arc<ClientConfig>> {
        if alpn.is_empty() {
            return Err(Error::Invalid("ALPN must not be empty"));
        }
        if !config.client_auth_cert_resolver.has_certs() {
            return Err(Error::Invalid("client certificate is required"));
        }
        let mut config = Arc::unwrap_or_clone(config);
        config.alpn_protocols = vec![alpn.to_vec()];
        config.check_selected_alpn = true;
        config.enable_early_data = false;
        config.resumption = rustls::client::Resumption::disabled();
        config.key_log = no_key_log();
        config.enable_secret_extraction = false;
        Ok(Arc::new(config))
    }

    /// Validates a DNS name or IP address used for server-certificate verification.
    pub fn server_name(name: impl Into<String>) -> Result<ServerName<'static>> {
        ServerName::try_from(name.into()).map_err(|_| Error::Invalid("invalid TLS server name"))
    }

    /// Builds a nonempty root store.
    pub fn roots(
        certificates: impl IntoIterator<Item = CertificateDer<'static>>,
    ) -> Result<RootCertStore> {
        let mut roots = RootCertStore::empty();
        for cert in certificates {
            roots
                .add(cert)
                .map_err(|_| Error::Invalid("invalid trust root"))?;
        }
        if roots.is_empty() {
            return Err(Error::Invalid("trust roots must not be empty"));
        }
        Ok(roots)
    }

    fn no_key_log() -> Arc<dyn KeyLog> {
        Arc::new(NoKeyLog)
    }

    /// Ticketer that never issues or accepts tickets (rustls's own is not public).
    #[derive(Debug)]
    struct NoTickets;
    impl ProducesTickets for NoTickets {
        fn enabled(&self) -> bool {
            false
        }
        fn lifetime(&self) -> u32 {
            0
        }
        fn encrypt(&self, _: &[u8]) -> Option<Vec<u8>> {
            None
        }
        fn decrypt(&self, _: &[u8]) -> Option<Vec<u8>> {
            None
        }
    }
}

pub(crate) fn verify_protocol(connection: &rustls::Connection) -> Result<()> {
    if connection.protocol_version() != Some(rustls::ProtocolVersion::TLSv1_3)
        || connection.alpn_protocol() != Some(TCP_ALPN)
    {
        return Err(Error::Protocol("TLS protocol negotiation failed"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{CertifiedKey, KeyPair, generate_simple_self_signed};

    fn identity(name: &str) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
        let CertifiedKey { cert, signing_key }: CertifiedKey<KeyPair> =
            generate_simple_self_signed(vec![name.into()]).unwrap();
        (
            cert.der().clone(),
            PrivateKeyDer::Pkcs8(signing_key.serialize_der().into()),
        )
    }

    #[test]
    fn caller_configs_with_early_data_tickets_and_no_alpn_are_corrected() {
        let (cert, key) = identity("localhost");
        let mut server =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![cert.clone()], key.clone_key())
                .unwrap();
        server.max_early_data_size = 16 * 1024;
        server.send_tls13_tickets = 4;
        server.send_half_rtt_data = true;
        server.enable_secret_extraction = true;
        let hardened = ServerSecurity::from_rustls_config(Arc::new(server), |_, _| true)
            .unwrap()
            .config;
        assert_eq!(hardened.alpn_protocols, vec![TCP_ALPN.to_vec()]);
        assert_eq!(hardened.max_early_data_size, 0);
        assert_eq!(hardened.send_tls13_tickets, 0);
        assert!(!hardened.send_half_rtt_data && !hardened.enable_secret_extraction);
        assert!(!hardened.ticketer.enabled());
        assert!(!hardened.session_storage.can_cache());

        let mut roots = RootCertStore::empty();
        roots.add(cert.clone()).unwrap();
        let builder =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots);
        let mut client = builder
            .clone()
            .with_client_auth_cert(vec![cert], key)
            .unwrap();
        client.enable_early_data = true;
        client.check_selected_alpn = false;
        let hardened = ClientSecurity::from_rustls_config(Arc::new(client), "localhost")
            .unwrap()
            .config;
        assert_eq!(hardened.alpn_protocols, vec![TCP_ALPN.to_vec()]);
        assert!(!hardened.enable_early_data && hardened.check_selected_alpn);
        assert!(matches!(
            ClientSecurity::from_rustls_config(
                Arc::new(builder.with_no_client_auth()),
                "localhost"
            ),
            Err(Error::Invalid(_))
        ));
    }
}
