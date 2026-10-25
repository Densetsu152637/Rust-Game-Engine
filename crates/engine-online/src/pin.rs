//! Pinning the session host's game-server certificate.
//!
//! The host registers the SHA-256 fingerprint of the certificate its game server presents
//! (`SetGameIdentity`); the service publishes the *current host's* fingerprint in the session view
//! and in `HostChanged` / `HostIdentityChanged`, over the authenticated lobby connection. A joiner
//! then accepts only a game server whose leaf certificate hashes to that fingerprint. Because the
//! pin names the exact certificate, the game server can use a self-signed certificate: no CA, no
//! hostname and no expiry check is needed (and none is done), the lobby vouches for the key.
//!
//! ```ignore
//! let pin = engine_online::pin::fingerprint_from_bytes(&session_view.host_fingerprint?.0)?;
//! let tls = engine_online::pin::pinned_client_config(pin, my_chain, my_key)?;
//! let security = engine_net_quic::QuicClientSecurity::from_rustls_config(tls, "game-host")?;
//! let client = engine_net_quic::QuicClient::connect(host_addr, QuicClientConfig::default(), security)?;
//! ```
//!
//! The server name only feeds SNI; it is not matched against the certificate. Everything else the
//! engine enforces on `from_rustls_config` (TLS 1.3 only, ALPN, no resumption, no early data,
//! mandatory client certificate) still applies.
use crate::{OnlineError, token::CertFingerprint, token::fingerprint};
use engine_network::rustls::{
    self, CertificateError, ClientConfig, DigitallySignedStruct, Error as TlsError,
    SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{
        WebPkiSupportedAlgorithms, ring::default_provider, verify_tls12_signature,
        verify_tls13_signature,
    },
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime},
};
use std::sync::Arc;
use subtle::ConstantTimeEq;

/// A rustls server-certificate verifier that accepts exactly one certificate, identified by the
/// SHA-256 of its DER encoding (compared in constant time). Handshake signatures are still
/// verified against that certificate's key by the provider.
#[derive(Debug)]
pub struct HostCertPin {
    fingerprint: CertFingerprint,
    algorithms: WebPkiSupportedAlgorithms,
}
impl HostCertPin {
    pub fn new(fingerprint: CertFingerprint) -> Self {
        Self {
            fingerprint,
            algorithms: default_provider().signature_verification_algorithms,
        }
    }
}
impl ServerCertVerifier for HostCertPin {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let actual = fingerprint(end_entity.as_ref());
        if bool::from(actual.as_slice().ct_eq(self.fingerprint.as_slice())) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(TlsError::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ))
        }
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// A TLS 1.3 client configuration that trusts only the game-server certificate whose SHA-256 is
/// `host_fingerprint`, and presents `chain`/`key` as the client certificate (the one the join
/// token is bound to). Pass it to `QuicClientSecurity::from_rustls_config` or
/// `engine_network::ClientSecurity::from_rustls_config`.
pub fn pinned_client_config(
    host_fingerprint: CertFingerprint,
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<Arc<ClientConfig>, OnlineError> {
    ClientConfig::builder_with_provider(Arc::new(default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| OnlineError::Invalid("TLS 1.3 unavailable"))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(HostCertPin::new(host_fingerprint)))
        .with_client_auth_cert(chain, key)
        .map(Arc::new)
        .map_err(|_| OnlineError::Invalid("invalid client certificate or key"))
}

/// Extracts the 32-byte fingerprint from a `host_fingerprint` blob; `None` if it has another
/// length (a host that has not registered one, or a malformed value).
pub fn fingerprint_from_bytes(bytes: &[u8]) -> Option<CertFingerprint> {
    CertFingerprint::try_from(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_pinned_certificate_is_accepted() {
        let der = |name: &str| -> CertificateDer<'static> {
            rcgen::generate_simple_self_signed(vec![name.into()])
                .unwrap()
                .cert
                .der()
                .clone()
        };
        let (host, other) = (der("host.example"), der("other.example"));
        let pin = HostCertPin::new(fingerprint(host.as_ref()));
        let name = ServerName::try_from("anything.invalid").unwrap();
        let now = UnixTime::now();
        assert!(pin.verify_server_cert(&host, &[], &name, &[], now).is_ok());
        assert!(matches!(
            pin.verify_server_cert(&other, &[], &name, &[], now),
            Err(TlsError::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure
            ))
        ));
        // Intermediates cannot smuggle a different leaf through.
        assert!(
            pin.verify_server_cert(&other, &[host], &name, &[], now)
                .is_err()
        );
        assert!(!pin.supported_verify_schemes().is_empty());
    }

    #[test]
    fn fingerprints_must_be_exactly_32_bytes() {
        assert!(fingerprint_from_bytes(&[1; 32]).is_some());
        assert!(fingerprint_from_bytes(&[1; 31]).is_none());
        assert!(fingerprint_from_bytes(&[]).is_none());
    }
}
