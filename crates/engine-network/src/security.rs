//! TLS configuration is deliberately opaque: callers cannot disable verification or enable 0-RTT.
use crate::{CertificateDer, Error, PrivateKeyDer, Result};
use rustls::{ClientConfig, RootCertStore, ServerConfig, pki_types::ServerName};
use serde::Serialize;
use std::{sync::Arc, time::Duration};

const ALPN: &[u8] = b"rust-game-engine/1";
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
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
            Arc::new(roots(client_roots)?),
            provider.clone(),
        )
        .build()
        .map_err(|_| Error::Invalid("invalid client trust roots"))?;
        let mut config = ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|_| Error::Invalid("TLS 1.3 unavailable"))?
            .with_client_cert_verifier(verifier)
            .with_single_cert(chain, key)
            .map_err(|_| Error::Invalid("invalid server certificate or key"))?;
        config.alpn_protocols = vec![ALPN.to_vec()];
        config.max_early_data_size = 0;
        config.send_tls13_tickets = 0;
        // Each connection must freshly prove certificate possession and run application policy.
        config.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
        Ok(Self {
            config: Arc::new(config),
            authorize: Arc::new(authorize),
            limits: SecurityLimits::default(),
        })
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
        let mut config =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_protocol_versions(&[&rustls::version::TLS13])
                .map_err(|_| Error::Invalid("TLS 1.3 unavailable"))?
                .with_root_certificates(roots(server_roots)?)
                .with_client_auth_cert(chain, key)
                .map_err(|_| Error::Invalid("invalid client certificate or key"))?;
        config.alpn_protocols = vec![ALPN.to_vec()];
        config.enable_early_data = false;
        config.resumption = rustls::client::Resumption::disabled();
        let server_name = ServerName::try_from(server_name.into())
            .map_err(|_| Error::Invalid("invalid TLS server name"))?;
        Ok(Self {
            config: Arc::new(config),
            server_name,
            limits: SecurityLimits::default(),
        })
    }
}
fn roots(certificates: impl IntoIterator<Item = CertificateDer<'static>>) -> Result<RootCertStore> {
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

pub(crate) fn verify_protocol(connection: &rustls::Connection) -> Result<()> {
    if connection.protocol_version() != Some(rustls::ProtocolVersion::TLSv1_3)
        || connection.alpn_protocol() != Some(ALPN)
    {
        return Err(Error::Protocol("TLS protocol negotiation failed"));
    }
    Ok(())
}
