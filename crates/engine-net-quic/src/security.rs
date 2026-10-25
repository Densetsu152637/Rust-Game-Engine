//! Mutual TLS 1.3 identities for QUIC. Same certificate model as the TCP transport; there is no
//! plaintext mode and no way to disable verification through the default constructors.
use engine_network::{
    CertificateDer, Error, PeerId, PrivateKeyDer, Result,
    rustls::{ClientConfig, ServerConfig},
    tls,
};
use quinn_proto::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use std::sync::Arc;

/// ALPN protocol identifier of the QUIC transport.
pub const QUIC_ALPN: &[u8] = b"rust-game-engine-quic/1";

pub(crate) type Authorize = dyn Fn(PeerId, &[CertificateDer<'_>]) -> bool + Send + Sync;

/// Server identity, client trust roots and the native admission policy. No Debug/Serialize
/// implementation exposes keys or identities. `authorize` receives the assigned peer ID and the
/// TLS-verified client chain exactly once per connection, before `Authenticated`/`Connected`;
/// it must be fast, nonblocking and fail closed.
#[derive(Clone)]
pub struct QuicServerSecurity {
    pub(crate) config: Arc<QuicServerConfig>,
    pub(crate) authorize: Arc<Authorize>,
}
impl QuicServerSecurity {
    pub fn new(
        chain: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        client_roots: impl IntoIterator<Item = CertificateDer<'static>>,
        authorize: impl Fn(PeerId, &[CertificateDer<'_>]) -> bool + Send + Sync + 'static,
    ) -> Result<Self> {
        Self::with_config(
            tls::server_config(chain, key, client_roots, QUIC_ALPN)?,
            authorize,
        )
    }
    /// Uses a caller-built rustls server configuration. Its client-certificate verifier,
    /// certificate resolver and crypto provider become trusted configuration. The endpoint still
    /// forces ALPN `rust-game-engine-quic/1`, disables 0-RTT, tickets, resumption, key logging and
    /// secret extraction ([`tls::harden_server`]), requires a presented client chain and runs
    /// `authorize` exactly once before Connected. QUIC itself only negotiates TLS 1.3. The
    /// provider must include `TLS13_AES_128_GCM_SHA256` (QUIC's initial suite).
    pub fn from_rustls_config(
        config: Arc<ServerConfig>,
        authorize: impl Fn(PeerId, &[CertificateDer<'_>]) -> bool + Send + Sync + 'static,
    ) -> Result<Self> {
        Self::with_config(tls::harden_server(config, QUIC_ALPN)?, authorize)
    }
    fn with_config(
        config: Arc<ServerConfig>,
        authorize: impl Fn(PeerId, &[CertificateDer<'_>]) -> bool + Send + Sync + 'static,
    ) -> Result<Self> {
        Ok(Self {
            config: Arc::new(
                QuicServerConfig::try_from(config)
                    .map_err(|_| Error::Invalid("TLS config lacks the QUIC initial suite"))?,
            ),
            authorize: Arc::new(authorize),
        })
    }
}

/// Client identity and server trust roots. The server name (DNS name or IP SAN) must match the
/// verified server certificate, independently of the address dialled.
#[derive(Clone)]
pub struct QuicClientSecurity {
    pub(crate) config: Arc<QuicClientConfig>,
    pub(crate) server_name: String,
}
impl QuicClientSecurity {
    pub fn new(
        server_name: impl Into<String>,
        server_roots: impl IntoIterator<Item = CertificateDer<'static>>,
        chain: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
    ) -> Result<Self> {
        Self::with_config(
            tls::client_config(server_roots, chain, key, QUIC_ALPN)?,
            server_name,
        )
    }
    /// Uses a caller-built rustls client configuration; its server verifier and client
    /// certificate resolver become trusted configuration. A configuration without client
    /// certificates is rejected; ALPN, early data, resumption and key logging are overridden
    /// ([`tls::harden_client`]) and the negotiated ALPN is rechecked after the handshake.
    pub fn from_rustls_config(
        config: Arc<ClientConfig>,
        server_name: impl Into<String>,
    ) -> Result<Self> {
        Self::with_config(tls::harden_client(config, QUIC_ALPN)?, server_name)
    }
    fn with_config(config: Arc<ClientConfig>, server_name: impl Into<String>) -> Result<Self> {
        let server_name = server_name.into();
        tls::server_name(server_name.clone())?;
        Ok(Self {
            config: Arc::new(
                QuicClientConfig::try_from(config)
                    .map_err(|_| Error::Invalid("TLS config lacks the QUIC initial suite"))?,
            ),
            server_name,
        })
    }
}
