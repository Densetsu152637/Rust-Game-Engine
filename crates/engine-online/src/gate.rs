//! Admission gate: a transport-admitted peer gets no service until it presents a valid
//! certificate-bound token on `Lane::SESSION` within a deadline.
//!
//! Sans-IO. The same gate protects the lobby service (scope [`Scope::Connect`]) and a game
//! server (scope `JoinSession { session, epoch }`): construct it with that server's
//! [`TokenVerifier`] and audience and pass the scope the connection must prove.
use crate::token::{CertFingerprint, Expect, Scope, TokenError, TokenVerifier};
use engine_network::PeerId;
use std::{collections::BTreeMap, sync::Arc};

/// Time sources for sans-IO cores: a monotonic clock for deadlines and rate limits, and wall
/// time for token validity. Tests construct this directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Now {
    pub mono_ms: u64,
    pub unix_secs: u64,
}

#[derive(Clone, Debug)]
pub struct GateConfig {
    /// Audience tokens must name (this service).
    pub audience: String,
    /// Deadline from `peer_connected` to a valid token.
    pub auth_timeout_ms: u64,
    /// Pending plus admitted peers tracked.
    pub max_peers: usize,
    /// Consume token nonces (recommended): a captured token cannot be presented twice.
    pub single_use: bool,
    /// Disconnect an admitted peer when the token that admitted it expires.
    pub enforce_expiry: bool,
}

/// What the gate learned from a verified token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub subject: String,
    pub fingerprint: CertFingerprint,
    pub expires_at: u64,
    pub session: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateError {
    /// The transport never recorded a certificate for the peer.
    NoCertificate,
    /// Too many tracked peers.
    Full,
    UnknownPeer,
    Token(TokenError),
    /// A re-authentication named a different subject.
    SubjectChanged,
}

/// Why `expired` selected a peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateTimeout {
    NoToken,
    TokenExpired,
}

enum State {
    Pending { deadline_ms: u64 },
    Admitted(Identity),
}
struct Peer {
    fingerprint: CertFingerprint,
    state: State,
}

pub struct SessionGate {
    config: GateConfig,
    verifier: Arc<TokenVerifier>,
    peers: BTreeMap<PeerId, Peer>,
}
impl SessionGate {
    pub fn new(config: GateConfig, verifier: Arc<TokenVerifier>) -> Self {
        Self {
            config,
            verifier,
            peers: BTreeMap::new(),
        }
    }
    /// Starts the clock for a peer the transport just admitted. `fingerprint` comes from
    /// [`crate::CertificateBinding::take`]; without it the peer must be disconnected.
    pub fn peer_connected(
        &mut self,
        peer: PeerId,
        fingerprint: Option<CertFingerprint>,
        now_ms: u64,
    ) -> Result<(), GateError> {
        let fingerprint = fingerprint.ok_or(GateError::NoCertificate)?;
        if self.peers.len() >= self.config.max_peers && !self.peers.contains_key(&peer) {
            return Err(GateError::Full);
        }
        self.peers.insert(
            peer,
            Peer {
                fingerprint,
                state: State::Pending {
                    deadline_ms: now_ms.saturating_add(self.config.auth_timeout_ms),
                },
            },
        );
        Ok(())
    }
    /// Verifies `token` against the peer's recorded certificate. The caller disconnects the peer
    /// on any error (no retry: guessing is not offered a second attempt on the same connection).
    pub fn authenticate(
        &mut self,
        peer: PeerId,
        token: &[u8],
        required: &Scope,
        now: Now,
    ) -> Result<&Identity, GateError> {
        let entry = self.peers.get_mut(&peer).ok_or(GateError::UnknownPeer)?;
        let verified = self
            .verifier
            .verify(
                token,
                &Expect {
                    audience: &self.config.audience,
                    scope: required,
                    cert_sha256: &entry.fingerprint,
                    single_use: self.config.single_use,
                },
                now.unix_secs,
            )
            .map_err(GateError::Token)?;
        if let State::Admitted(existing) = &entry.state
            && existing.subject != verified.claims.subject
        {
            return Err(GateError::SubjectChanged);
        }
        entry.state = State::Admitted(Identity {
            subject: verified.claims.subject,
            fingerprint: entry.fingerprint,
            expires_at: verified.claims.expires_at,
            session: verified.claims.session,
        });
        match &entry.state {
            State::Admitted(identity) => Ok(identity),
            State::Pending { .. } => Err(GateError::UnknownPeer),
        }
    }
    pub fn identity(&self, peer: PeerId) -> Option<&Identity> {
        match &self.peers.get(&peer)?.state {
            State::Admitted(identity) => Some(identity),
            State::Pending { .. } => None,
        }
    }
    /// Whether the peer is tracked (pending or admitted).
    pub fn contains(&self, peer: PeerId) -> bool {
        self.peers.contains_key(&peer)
    }
    pub fn is_admitted(&self, peer: PeerId) -> bool {
        self.identity(peer).is_some()
    }
    pub fn peer_gone(&mut self, peer: PeerId) {
        self.peers.remove(&peer);
    }
    pub fn len(&self) -> usize {
        self.peers.len()
    }
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }
    /// Checks a peer before processing a message, independently of periodic timer work.
    pub(crate) fn timeout(&self, peer: PeerId, now: Now) -> Option<GateTimeout> {
        match &self.peers.get(&peer)?.state {
            State::Pending { deadline_ms } if now.mono_ms >= *deadline_ms => {
                Some(GateTimeout::NoToken)
            }
            State::Admitted(identity)
                if self.config.enforce_expiry && now.unix_secs >= identity.expires_at =>
            {
                Some(GateTimeout::TokenExpired)
            }
            _ => None,
        }
    }

    /// Removes and returns peers that missed the deadline or whose admitting token expired.
    pub fn expired(&mut self, now: Now) -> Vec<(PeerId, GateTimeout)> {
        let due: Vec<(PeerId, GateTimeout)> = self
            .peers
            .keys()
            .filter_map(|&peer| self.timeout(peer, now).map(|why| (peer, why)))
            .collect();
        for (peer, _) in &due {
            self.peers.remove(peer);
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::{TokenIssuer, TokenRequest, VerifierConfig, fingerprint};

    const UNIX: u64 = 1_800_000_000;

    fn setup() -> (SessionGate, TokenIssuer) {
        let issuer =
            TokenIssuer::from_pkcs8("k1", "iss", &TokenIssuer::generate_pkcs8().unwrap(), 3600)
                .unwrap();
        let verifier = Arc::new(
            TokenVerifier::new(VerifierConfig::default(), [issuer.trusted_key()]).unwrap(),
        );
        let gate = SessionGate::new(
            GateConfig {
                audience: "svc".into(),
                auth_timeout_ms: 1000,
                max_peers: 2,
                single_use: true,
                enforce_expiry: true,
            },
            verifier,
        );
        (gate, issuer)
    }
    fn token(issuer: &TokenIssuer, cert: &[u8], subject: &str, ttl: u64) -> Vec<u8> {
        issuer
            .issue(
                &TokenRequest {
                    audience: "svc",
                    subject,
                    session: None,
                    cert_sha256: fingerprint(cert),
                    lifetime_secs: ttl,
                    scopes: vec![Scope::Connect],
                },
                UNIX,
            )
            .unwrap()
    }
    fn now(mono_ms: u64) -> Now {
        Now {
            mono_ms,
            unix_secs: UNIX,
        }
    }

    #[test]
    fn peer_without_token_times_out_and_without_certificate_is_refused() {
        let (mut gate, _) = setup();
        assert_eq!(
            gate.peer_connected(1, None, 0),
            Err(GateError::NoCertificate)
        );
        gate.peer_connected(1, Some(fingerprint(b"a")), 0).unwrap();
        assert!(!gate.is_admitted(1));
        assert!(gate.expired(now(999)).is_empty());
        assert_eq!(gate.expired(now(1000)), vec![(1, GateTimeout::NoToken)]);
        assert!(gate.is_empty());
    }

    #[test]
    fn valid_bound_token_admits_and_foreign_or_replayed_tokens_do_not() {
        let (mut gate, issuer) = setup();
        gate.peer_connected(1, Some(fingerprint(b"alice-cert")), 0)
            .unwrap();
        gate.peer_connected(2, Some(fingerprint(b"mallory-cert")), 0)
            .unwrap();
        let t = token(&issuer, b"alice-cert", "alice", 60);
        // Mallory replays Alice's captured token on her own (different-certificate) connection.
        assert_eq!(
            gate.authenticate(2, &t, &Scope::Connect, now(10))
                .unwrap_err(),
            GateError::Token(TokenError::WrongCertificate)
        );
        assert_eq!(
            gate.authenticate(1, &t, &Scope::Connect, now(10))
                .unwrap()
                .subject,
            "alice"
        );
        assert!(gate.is_admitted(1));
        // The same token again (even from the owner) is a replay.
        assert_eq!(
            gate.authenticate(1, &t, &Scope::Connect, now(11))
                .unwrap_err(),
            GateError::Token(TokenError::Replayed)
        );
        assert_eq!(
            gate.authenticate(9, &t, &Scope::Connect, now(11))
                .unwrap_err(),
            GateError::UnknownPeer
        );
    }

    #[test]
    fn reauthentication_cannot_change_identity_and_expiry_is_enforced() {
        let (mut gate, issuer) = setup();
        gate.peer_connected(1, Some(fingerprint(b"c")), 0).unwrap();
        gate.authenticate(
            1,
            &token(&issuer, b"c", "alice", 60),
            &Scope::Connect,
            now(1),
        )
        .unwrap();
        assert_eq!(
            gate.authenticate(1, &token(&issuer, b"c", "bob", 60), &Scope::Connect, now(2))
                .unwrap_err(),
            GateError::SubjectChanged
        );
        let later = Now {
            mono_ms: 5,
            unix_secs: UNIX + 60,
        };
        assert_eq!(gate.expired(later), vec![(1, GateTimeout::TokenExpired)]);
    }

    #[test]
    fn tracked_peers_are_bounded() {
        let (mut gate, _) = setup();
        gate.peer_connected(1, Some([1; 32]), 0).unwrap();
        gate.peer_connected(2, Some([2; 32]), 0).unwrap();
        assert_eq!(
            gate.peer_connected(3, Some([3; 32]), 0),
            Err(GateError::Full)
        );
        gate.peer_gone(1);
        assert!(gate.peer_connected(3, Some([3; 32]), 0).is_ok());
    }
}
