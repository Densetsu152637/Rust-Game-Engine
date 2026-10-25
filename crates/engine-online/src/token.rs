//! Ed25519-signed, certificate-bound bearer tokens in a bounded canonical binary encoding.
//!
//! ```text
//! u8  version (1)
//! u8  len | key id      (1..=32,  ASCII identifier)
//! u8  len | issuer      (1..=64,  ASCII identifier)
//! u8  len | audience    (1..=64,  ASCII identifier)
//! u8  len | subject     (1..=64,  UTF-8 without control characters)
//! u8  has_session (0|1) | [u64 BE session id]
//! [32] SHA-256 of the client's TLS leaf certificate DER
//! u64 BE issued_at (unix seconds) | u64 BE expires_at | [16] random nonce
//! u8  scope count (1..=8) | scopes
//!       1 Connect | 2 JoinSession u64 session, u64 epoch
//!       3 Relay u64 session, [16] channel, u8 party (0 A | 1 B) | 4 Custom u8 len (1..=32) id
//! [64] Ed25519 signature over "engine-online/token/v1\0" || everything above
//! ```
//!
//! Every field is validated while parsing, trailing bytes are rejected and the encoding is
//! deterministic, so a token has exactly one valid byte representation. Parsing happens before
//! signature verification but allocates only bounded, validated strings. A token proves what the
//! issuer asserted; it is only useful to the holder of the certificate named in it, because every
//! verifier also compares that fingerprint with the certificate of the live TLS connection.
use crate::text::{valid_ident, valid_text};
use ring::{
    digest,
    rand::{SecureRandom, SystemRandom},
    signature::{self, Ed25519KeyPair, KeyPair, UnparsedPublicKey},
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    sync::{Mutex, RwLock},
};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

pub const TOKEN_VERSION: u8 = 1;
pub const MAX_TOKEN_BYTES: usize = 1024;
pub const SIGNATURE_BYTES: usize = 64;
pub const NONCE_BYTES: usize = 16;
pub const MAX_KEY_ID_BYTES: usize = 32;
pub const MAX_NAME_BYTES: usize = 64;
pub const MAX_SCOPES: usize = 8;
pub const MAX_CUSTOM_SCOPE_BYTES: usize = 32;
/// Most public keys a verifier trusts at once (rotation overlap needs only a few).
pub const MAX_TRUSTED_KEYS: usize = 16;
const DOMAIN: &[u8] = b"engine-online/token/v1\0";

/// SHA-256 of a DER-encoded TLS leaf certificate.
pub type CertFingerprint = [u8; 32];

/// Computes the fingerprint tokens are bound to.
pub fn fingerprint(certificate_der: &[u8]) -> CertFingerprint {
    let mut out = [0; 32];
    out.copy_from_slice(digest::digest(&digest::SHA256, certificate_der).as_ref());
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenError {
    Malformed(&'static str),
    UnsupportedVersion,
    UnknownKey,
    BadSignature,
    WrongIssuer,
    WrongAudience,
    NotYetValid,
    Expired,
    LifetimeTooLong,
    MissingScope,
    WrongCertificate,
    Replayed,
    ReplayCacheFull,
    InvalidRequest(&'static str),
    Internal,
}
impl fmt::Display for TokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(why) => write!(f, "malformed token: {why}"),
            Self::UnsupportedVersion => f.write_str("unsupported token version"),
            Self::UnknownKey => f.write_str("unknown token key"),
            Self::BadSignature => f.write_str("bad token signature"),
            Self::WrongIssuer => f.write_str("wrong token issuer"),
            Self::WrongAudience => f.write_str("wrong token audience"),
            Self::NotYetValid => f.write_str("token not yet valid"),
            Self::Expired => f.write_str("token expired"),
            Self::LifetimeTooLong => f.write_str("token lifetime too long"),
            Self::MissingScope => f.write_str("token lacks the required scope"),
            Self::WrongCertificate => f.write_str("token bound to another certificate"),
            Self::Replayed => f.write_str("token already used"),
            Self::ReplayCacheFull => f.write_str("token replay cache is full"),
            Self::InvalidRequest(why) => write!(f, "invalid token request: {why}"),
            Self::Internal => f.write_str("token subsystem failure"),
        }
    }
}
impl std::error::Error for TokenError {}

/// A permission a token grants. Verifiers require an exact match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Connect to the online (lobby/matchmaking) service.
    Connect,
    /// Join `session` while its host epoch is `epoch`; game servers reject other epochs, which
    /// fences a deposed host after migration.
    JoinSession { session: u64, epoch: u64 },
    /// Take one party slot of one relay channel (consumed by the relay service through
    /// [`crate::relay::relay_authorizer`]). A credential names a *pair*, not a session: the
    /// channel is derived from the session and the two members' subjects, and the party is fixed
    /// by subject order, so a holder can occupy exactly one slot of exactly one channel.
    Relay {
        session: u64,
        channel: [u8; 16],
        party: RelayParty,
    },
    /// Application-defined scope identifier.
    Custom(String),
}
/// Which of a relay channel's two slots a relay credential may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelayParty {
    A,
    B,
}
impl RelayParty {
    fn code(self) -> u8 {
        match self {
            Self::A => 0,
            Self::B => 1,
        }
    }
    fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::A),
            1 => Some(Self::B),
            _ => None,
        }
    }
}

impl Scope {
    fn encode(&self, out: &mut Vec<u8>) -> Result<(), TokenError> {
        match self {
            Self::Connect => out.push(1),
            Self::JoinSession { session, epoch } => {
                out.push(2);
                out.extend_from_slice(&session.to_be_bytes());
                out.extend_from_slice(&epoch.to_be_bytes());
            }
            Self::Relay {
                session,
                channel,
                party,
            } => {
                out.push(3);
                out.extend_from_slice(&session.to_be_bytes());
                out.extend_from_slice(channel);
                out.push(party.code());
            }
            Self::Custom(name) => {
                if !valid_ident(name, MAX_CUSTOM_SCOPE_BYTES) {
                    return Err(TokenError::InvalidRequest("invalid custom scope"));
                }
                out.push(4);
                out.push(name.len() as u8);
                out.extend_from_slice(name.as_bytes());
            }
        }
        Ok(())
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self, TokenError> {
        Ok(match r.u8()? {
            1 => Self::Connect,
            2 => Self::JoinSession {
                session: r.u64()?,
                epoch: r.u64()?,
            },
            3 => Self::Relay {
                session: r.u64()?,
                channel: r.array()?,
                party: RelayParty::from_code(r.u8()?)
                    .ok_or(TokenError::Malformed("relay party"))?,
            },
            4 => Self::Custom(r.ident(MAX_CUSTOM_SCOPE_BYTES)?),
            _ => return Err(TokenError::Malformed("unknown scope")),
        })
    }
}

/// The authenticated statements in a token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claims {
    pub key_id: String,
    pub issuer: String,
    pub audience: String,
    pub subject: String,
    pub session: Option<u64>,
    pub cert_sha256: CertFingerprint,
    pub issued_at: u64,
    pub expires_at: u64,
    pub nonce: [u8; NONCE_BYTES],
    pub scopes: Vec<Scope>,
}

struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], TokenError> {
        if self.0.len() < n {
            return Err(TokenError::Malformed("truncated"));
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8, TokenError> {
        Ok(self.take(1)?[0])
    }
    fn u64(&mut self) -> Result<u64, TokenError> {
        let mut a = [0; 8];
        a.copy_from_slice(self.take(8)?);
        Ok(u64::from_be_bytes(a))
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], TokenError> {
        let mut a = [0; N];
        a.copy_from_slice(self.take(N)?);
        Ok(a)
    }
    fn string(&mut self, max: usize) -> Result<&'a str, TokenError> {
        let len = usize::from(self.u8()?);
        if len == 0 || len > max {
            return Err(TokenError::Malformed("string length"));
        }
        std::str::from_utf8(self.take(len)?).map_err(|_| TokenError::Malformed("string encoding"))
    }
    fn ident(&mut self, max: usize) -> Result<String, TokenError> {
        let text = self.string(max)?;
        if !valid_ident(text, max) {
            return Err(TokenError::Malformed("identifier characters"));
        }
        Ok(text.to_owned())
    }
    fn text(&mut self, max: usize) -> Result<String, TokenError> {
        let text = self.string(max)?;
        if !valid_text(text, max) {
            return Err(TokenError::Malformed("text characters"));
        }
        Ok(text.to_owned())
    }
}

fn put_str(out: &mut Vec<u8>, text: &str) {
    // Callers validate `text.len() <= 64` first.
    out.push(text.len() as u8);
    out.extend_from_slice(text.as_bytes());
}

impl Claims {
    fn validate(&self) -> Result<(), TokenError> {
        let bad = |why| Err(TokenError::InvalidRequest(why));
        if !valid_ident(&self.key_id, MAX_KEY_ID_BYTES) {
            return bad("key id");
        }
        if !valid_ident(&self.issuer, MAX_NAME_BYTES) {
            return bad("issuer");
        }
        if !valid_ident(&self.audience, MAX_NAME_BYTES) {
            return bad("audience");
        }
        if !valid_text(&self.subject, MAX_NAME_BYTES) {
            return bad("subject");
        }
        if self.scopes.is_empty() || self.scopes.len() > MAX_SCOPES {
            return bad("scope count");
        }
        for (i, scope) in self.scopes.iter().enumerate() {
            if self.scopes[..i].contains(scope) {
                return bad("duplicate scope");
            }
        }
        if self.expires_at <= self.issued_at {
            return bad("expiry before issue");
        }
        Ok(())
    }

    fn encode_payload(&self) -> Result<Vec<u8>, TokenError> {
        self.validate()?;
        let mut out = Vec::with_capacity(256);
        out.push(TOKEN_VERSION);
        put_str(&mut out, &self.key_id);
        put_str(&mut out, &self.issuer);
        put_str(&mut out, &self.audience);
        put_str(&mut out, &self.subject);
        match self.session {
            None => out.push(0),
            Some(session) => {
                out.push(1);
                out.extend_from_slice(&session.to_be_bytes());
            }
        }
        out.extend_from_slice(&self.cert_sha256);
        out.extend_from_slice(&self.issued_at.to_be_bytes());
        out.extend_from_slice(&self.expires_at.to_be_bytes());
        out.extend_from_slice(&self.nonce);
        out.push(self.scopes.len() as u8);
        for scope in &self.scopes {
            scope.encode(&mut out)?;
        }
        Ok(out)
    }

    fn decode_payload(payload: &[u8]) -> Result<Self, TokenError> {
        let mut r = Reader(payload);
        if r.u8()? != TOKEN_VERSION {
            return Err(TokenError::UnsupportedVersion);
        }
        let key_id = r.ident(MAX_KEY_ID_BYTES)?;
        let issuer = r.ident(MAX_NAME_BYTES)?;
        let audience = r.ident(MAX_NAME_BYTES)?;
        let subject = r.text(MAX_NAME_BYTES)?;
        let session = match r.u8()? {
            0 => None,
            1 => Some(r.u64()?),
            _ => return Err(TokenError::Malformed("session flag")),
        };
        let cert_sha256 = r.array()?;
        let issued_at = r.u64()?;
        let expires_at = r.u64()?;
        let nonce = r.array()?;
        let count = usize::from(r.u8()?);
        if count == 0 || count > MAX_SCOPES {
            return Err(TokenError::Malformed("scope count"));
        }
        let mut scopes = Vec::with_capacity(count);
        for _ in 0..count {
            scopes.push(Scope::decode(&mut r)?);
        }
        if !r.0.is_empty() {
            return Err(TokenError::Malformed("trailing bytes"));
        }
        let claims = Self {
            key_id,
            issuer,
            audience,
            subject,
            session,
            cert_sha256,
            issued_at,
            expires_at,
            nonce,
            scopes,
        };
        claims
            .validate()
            .map_err(|_| TokenError::Malformed("claims"))?;
        Ok(claims)
    }
}

/// Splits a token into validated claims, the signed payload and the signature.
fn parse(token: &[u8]) -> Result<(Claims, &[u8], &[u8]), TokenError> {
    if token.len() > MAX_TOKEN_BYTES {
        return Err(TokenError::Malformed("too long"));
    }
    if token.len() <= SIGNATURE_BYTES {
        return Err(TokenError::Malformed("too short"));
    }
    let (payload, signature) = token.split_at(token.len() - SIGNATURE_BYTES);
    Ok((Claims::decode_payload(payload)?, payload, signature))
}

fn signing_input(payload: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(DOMAIN.len() + payload.len());
    message.extend_from_slice(DOMAIN);
    message.extend_from_slice(payload);
    message
}

/// What a token is minted for.
#[derive(Clone, Debug)]
pub struct TokenRequest<'a> {
    pub audience: &'a str,
    pub subject: &'a str,
    pub session: Option<u64>,
    /// Fingerprint of the certificate the holder will present on the TLS connection.
    pub cert_sha256: CertFingerprint,
    pub lifetime_secs: u64,
    pub scopes: Vec<Scope>,
}

/// Signing authority. Holds the private key: no `Debug`, `Clone` or `Serialize`.
pub struct TokenIssuer {
    key_id: String,
    issuer: String,
    pair: Ed25519KeyPair,
    rng: SystemRandom,
    max_lifetime_secs: u64,
}
impl TokenIssuer {
    /// Loads an Ed25519 PKCS#8 key (ring-generated v2 or OpenSSL v1).
    pub fn from_pkcs8(
        key_id: impl Into<String>,
        issuer: impl Into<String>,
        pkcs8: &[u8],
        max_lifetime_secs: u64,
    ) -> Result<Self, TokenError> {
        let key_id = key_id.into();
        let issuer = issuer.into();
        if !valid_ident(&key_id, MAX_KEY_ID_BYTES) || !valid_ident(&issuer, MAX_NAME_BYTES) {
            return Err(TokenError::InvalidRequest("key id or issuer"));
        }
        if max_lifetime_secs == 0 {
            return Err(TokenError::InvalidRequest("max lifetime"));
        }
        let pair = Ed25519KeyPair::from_pkcs8_maybe_unchecked(pkcs8)
            .map_err(|_| TokenError::InvalidRequest("signing key"))?;
        Ok(Self {
            key_id,
            issuer,
            pair,
            rng: SystemRandom::new(),
            max_lifetime_secs,
        })
    }
    /// Generates a fresh signing key as PKCS#8 DER. The caller must protect the returned bytes.
    pub fn generate_pkcs8() -> Result<Zeroizing<Vec<u8>>, TokenError> {
        Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .map(|document| Zeroizing::new(document.as_ref().to_vec()))
            .map_err(|_| TokenError::Internal)
    }
    pub fn key_id(&self) -> &str {
        &self.key_id
    }
    pub fn issuer(&self) -> &str {
        &self.issuer
    }
    /// Longest lifetime this issuer will sign.
    pub fn max_lifetime_secs(&self) -> u64 {
        self.max_lifetime_secs
    }
    /// The public half, for verifiers (game servers, relays).
    pub fn trusted_key(&self) -> TrustedKey {
        let mut public_key = [0; 32];
        public_key.copy_from_slice(self.pair.public_key().as_ref());
        TrustedKey {
            key_id: self.key_id.clone(),
            issuer: self.issuer.clone(),
            public_key,
        }
    }
    /// Mints and signs one token. `now_unix` is seconds since the Unix epoch.
    pub fn issue(&self, request: &TokenRequest<'_>, now_unix: u64) -> Result<Vec<u8>, TokenError> {
        if request.lifetime_secs == 0 || request.lifetime_secs > self.max_lifetime_secs {
            return Err(TokenError::InvalidRequest("lifetime"));
        }
        let expires_at = now_unix
            .checked_add(request.lifetime_secs)
            .ok_or(TokenError::InvalidRequest("lifetime"))?;
        let mut nonce = [0; NONCE_BYTES];
        self.rng
            .fill(&mut nonce)
            .map_err(|_| TokenError::Internal)?;
        let claims = Claims {
            key_id: self.key_id.clone(),
            issuer: self.issuer.clone(),
            audience: request.audience.to_owned(),
            subject: request.subject.to_owned(),
            session: request.session,
            cert_sha256: request.cert_sha256,
            issued_at: now_unix,
            expires_at,
            nonce,
            scopes: request.scopes.clone(),
        };
        let mut token = claims.encode_payload()?;
        let signature = self.pair.sign(&signing_input(&token));
        token.extend_from_slice(signature.as_ref());
        debug_assert!(token.len() <= MAX_TOKEN_BYTES);
        Ok(token)
    }
}

/// A public verification key and the issuer name it may assert.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustedKey {
    key_id: String,
    issuer: String,
    public_key: [u8; 32],
}
impl TrustedKey {
    pub fn new(
        key_id: impl Into<String>,
        issuer: impl Into<String>,
        public_key: [u8; 32],
    ) -> Result<Self, TokenError> {
        let key_id = key_id.into();
        let issuer = issuer.into();
        if !valid_ident(&key_id, MAX_KEY_ID_BYTES) || !valid_ident(&issuer, MAX_NAME_BYTES) {
            return Err(TokenError::InvalidRequest("key id or issuer"));
        }
        Ok(Self {
            key_id,
            issuer,
            public_key,
        })
    }
    pub fn key_id(&self) -> &str {
        &self.key_id
    }
    pub fn public_key(&self) -> &[u8; 32] {
        &self.public_key
    }
}

/// Verifier limits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifierConfig {
    /// Longest `expires_at - issued_at` accepted, regardless of what the issuer would sign.
    pub max_lifetime_secs: u64,
    /// Tolerated clock difference with the issuer, applied to both ends of the validity window.
    pub clock_skew_secs: u64,
    /// Most single-use nonces remembered; a full cache rejects single-use tokens (fail closed).
    pub replay_capacity: usize,
    /// Most live nonces remembered per certificate fingerprint. A fingerprint over its share
    /// evicts its own oldest nonce instead of taking space from others, so one certificate can
    /// never fill the cache and lock every other certificate out. The effective share is
    /// clamped to half of `replay_capacity` so at least two fingerprints always coexist.
    pub replay_per_fingerprint: usize,
}
impl Default for VerifierConfig {
    fn default() -> Self {
        Self {
            max_lifetime_secs: 24 * 3600,
            clock_skew_secs: 30,
            replay_capacity: 65_536,
            replay_per_fingerprint: 1024,
        }
    }
}

/// What a verifier requires of one token.
#[derive(Clone, Copy, Debug)]
pub struct Expect<'a> {
    pub audience: &'a str,
    /// Exact scope the token must carry.
    pub scope: &'a Scope,
    /// Fingerprint of the certificate of the live TLS connection presenting the token.
    pub cert_sha256: &'a CertFingerprint,
    /// Consume the nonce: a second presentation is rejected as a replay.
    pub single_use: bool,
}

/// Claims of a token that passed every check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedToken {
    pub claims: Claims,
}

/// What a verified relay credential allows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayClaims {
    /// The member the credential was minted for.
    pub subject: String,
    pub session: u64,
    pub channel: [u8; 16],
    pub party: RelayParty,
    /// Unix seconds; the allocation must end by then.
    pub expires_at: u64,
}

struct ReplayEntry {
    expiry: u64,
    fingerprint: CertFingerprint,
    seq: u64,
}
#[derive(Default)]
struct ReplayCache {
    by_nonce: HashMap<[u8; NONCE_BYTES], ReplayEntry>,
    by_expiry: BTreeSet<(u64, [u8; NONCE_BYTES])>,
    /// Each fingerprint's live nonces in insertion order (oldest first).
    by_fingerprint: HashMap<CertFingerprint, BTreeMap<u64, [u8; NONCE_BYTES]>>,
    next_seq: u64,
}
impl ReplayCache {
    fn remove(&mut self, nonce: [u8; NONCE_BYTES]) {
        let Some(entry) = self.by_nonce.remove(&nonce) else {
            return;
        };
        self.by_expiry.remove(&(entry.expiry, nonce));
        if let Some(live) = self.by_fingerprint.get_mut(&entry.fingerprint) {
            live.remove(&entry.seq);
            if live.is_empty() {
                self.by_fingerprint.remove(&entry.fingerprint);
            }
        }
    }
    fn prune(&mut self, now: u64) {
        while let Some(&(expiry, nonce)) = self.by_expiry.first() {
            if expiry >= now {
                break;
            }
            self.remove(nonce);
        }
    }
    fn live_for(&self, fingerprint: &CertFingerprint) -> usize {
        self.by_fingerprint
            .get(fingerprint)
            .map_or(0, BTreeMap::len)
    }
    /// Remembers `nonce` until `retain_until`. Fails when already present, or when the cache is
    /// full of other fingerprints' nonces. A fingerprint at its share evicts its own oldest
    /// nonce first (that old token becomes replayable again, but only by the holder of the
    /// certificate it is bound to).
    fn insert(
        &mut self,
        nonce: [u8; NONCE_BYTES],
        fingerprint: CertFingerprint,
        retain_until: u64,
        now: u64,
        limits: (usize, usize),
    ) -> Result<(), TokenError> {
        let (capacity, per_fingerprint) = limits;
        self.prune(now);
        if self.by_nonce.contains_key(&nonce) {
            return Err(TokenError::Replayed);
        }
        let share = per_fingerprint.clamp(1, (capacity / 2).max(1));
        if self.live_for(&fingerprint) >= share
            && let Some(oldest) = self
                .by_fingerprint
                .get(&fingerprint)
                .and_then(|live| live.first_key_value())
                .map(|(_, &nonce)| nonce)
        {
            self.remove(oldest);
        }
        if self.by_nonce.len() >= capacity {
            return Err(TokenError::ReplayCacheFull);
        }
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        self.by_nonce.insert(
            nonce,
            ReplayEntry {
                expiry: retain_until,
                fingerprint,
                seq,
            },
        );
        self.by_expiry.insert((retain_until, nonce));
        self.by_fingerprint
            .entry(fingerprint)
            .or_default()
            .insert(seq, nonce);
        Ok(())
    }
}

/// Verifies tokens against a rotating set of public keys. Share it between threads with `Arc`.
pub struct TokenVerifier {
    config: VerifierConfig,
    keys: RwLock<BTreeMap<String, TrustedKey>>,
    replay: Mutex<ReplayCache>,
}
impl TokenVerifier {
    pub fn new(
        config: VerifierConfig,
        keys: impl IntoIterator<Item = TrustedKey>,
    ) -> Result<Self, TokenError> {
        if config.max_lifetime_secs == 0
            || config.replay_capacity == 0
            || config.replay_per_fingerprint == 0
        {
            return Err(TokenError::InvalidRequest("verifier config"));
        }
        let verifier = Self {
            config,
            keys: RwLock::default(),
            replay: Mutex::default(),
        };
        for key in keys {
            verifier.add_key(key)?;
        }
        Ok(verifier)
    }
    /// Starts trusting a key (rotation: add the new key, switch the signer, later remove the old).
    pub fn add_key(&self, key: TrustedKey) -> Result<(), TokenError> {
        let mut keys = self.keys.write().map_err(|_| TokenError::Internal)?;
        if !keys.contains_key(&key.key_id) && keys.len() >= MAX_TRUSTED_KEYS {
            return Err(TokenError::InvalidRequest("too many trusted keys"));
        }
        keys.insert(key.key_id.clone(), key);
        Ok(())
    }
    /// Stops trusting a key immediately; its outstanding tokens stop verifying.
    pub fn remove_key(&self, key_id: &str) -> Result<bool, TokenError> {
        Ok(self
            .keys
            .write()
            .map_err(|_| TokenError::Internal)?
            .remove(key_id)
            .is_some())
    }
    pub fn key_ids(&self) -> Vec<String> {
        self.keys
            .read()
            .map(|keys| keys.keys().cloned().collect())
            .unwrap_or_default()
    }
    /// Encoding, key, signature, issuer, audience, validity window and lifetime: everything
    /// that does not depend on what the token is used for.
    fn verify_signed(
        &self,
        token: &[u8],
        audience: &str,
        now_unix: u64,
    ) -> Result<Claims, TokenError> {
        let (claims, payload, signature) = parse(token)?;
        let key = self
            .keys
            .read()
            .map_err(|_| TokenError::Internal)?
            .get(&claims.key_id)
            .cloned()
            .ok_or(TokenError::UnknownKey)?;
        UnparsedPublicKey::new(&signature::ED25519, &key.public_key)
            .verify(&signing_input(payload), signature)
            .map_err(|_| TokenError::BadSignature)?;
        if claims.issuer != key.issuer {
            return Err(TokenError::WrongIssuer);
        }
        if claims.audience != audience {
            return Err(TokenError::WrongAudience);
        }
        let skew = self.config.clock_skew_secs;
        if claims.issued_at > now_unix.saturating_add(skew) {
            return Err(TokenError::NotYetValid);
        }
        if claims.expires_at.saturating_add(skew) <= now_unix {
            return Err(TokenError::Expired);
        }
        if claims.expires_at - claims.issued_at > self.config.max_lifetime_secs {
            return Err(TokenError::LifetimeTooLong);
        }
        Ok(claims)
    }
    /// Checks, in order: encoding, key, signature, issuer, audience, validity window and
    /// lifetime, scope, certificate binding (constant time) and finally replay. The nonce is
    /// consumed only when every other check passed.
    pub fn verify(
        &self,
        token: &[u8],
        expect: &Expect<'_>,
        now_unix: u64,
    ) -> Result<VerifiedToken, TokenError> {
        let claims = self.verify_signed(token, expect.audience, now_unix)?;
        if !claims.scopes.iter().any(|scope| scope == expect.scope) {
            return Err(TokenError::MissingScope);
        }
        if !bool::from(
            claims
                .cert_sha256
                .as_slice()
                .ct_eq(expect.cert_sha256.as_slice()),
        ) {
            return Err(TokenError::WrongCertificate);
        }
        if expect.single_use {
            let skew = self.config.clock_skew_secs;
            let retain_until = claims.expires_at.saturating_add(skew);
            self.replay
                .lock()
                .map_err(|_| TokenError::Internal)?
                .insert(
                    claims.nonce,
                    claims.cert_sha256,
                    retain_until,
                    now_unix,
                    (
                        self.config.replay_capacity,
                        self.config.replay_per_fingerprint,
                    ),
                )?;
        }
        Ok(VerifiedToken { claims })
    }
    /// Verifies a relay credential for `audience`: signature, issuer, audience, validity and the
    /// `Relay` scope. A relay sees no TLS certificate of the holder, so unlike
    /// [`verify`](Self::verify) this checks **no certificate binding and consumes no nonce**
    /// (allocate requests are retransmitted). The credential is a bearer secret bounded by its
    /// expiry and by the relay's return-routability cookie; see `docs/online-nat-relay.md`.
    pub fn verify_relay(
        &self,
        token: &[u8],
        audience: &str,
        now_unix: u64,
    ) -> Result<RelayClaims, TokenError> {
        let claims = self.verify_signed(token, audience, now_unix)?;
        let (session, channel, party) = claims
            .scopes
            .iter()
            .find_map(|scope| match scope {
                Scope::Relay {
                    session,
                    channel,
                    party,
                } => Some((*session, *channel, *party)),
                _ => None,
            })
            .ok_or(TokenError::MissingScope)?;
        Ok(RelayClaims {
            subject: claims.subject,
            session,
            channel,
            party,
            expires_at: claims.expires_at,
        })
    }
    /// Convenience for a game server admitting a joiner: the token must carry
    /// `JoinSession { session, epoch }` for this server's audience and be bound to the
    /// certificate the player presented. Join tokens are single use.
    pub fn verify_join(
        &self,
        token: &[u8],
        audience: &str,
        session: u64,
        epoch: u64,
        peer_certificate_der: &[u8],
        now_unix: u64,
    ) -> Result<VerifiedToken, TokenError> {
        let scope = Scope::JoinSession { session, epoch };
        self.verify(
            token,
            &Expect {
                audience,
                scope: &scope,
                cert_sha256: &fingerprint(peer_certificate_der),
                single_use: true,
            },
            now_unix,
        )
    }
    /// Entries currently held by the replay cache (for tests and telemetry).
    pub fn replay_entries(&self) -> usize {
        self.replay
            .lock()
            .map(|cache| cache.by_nonce.len())
            .unwrap_or(0)
    }
    /// Live replay entries of one certificate fingerprint (for tests and telemetry).
    pub fn replay_entries_for(&self, fingerprint: &CertFingerprint) -> usize {
        self.replay
            .lock()
            .map(|cache| cache.live_for(fingerprint))
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn issuer(key_id: &str) -> TokenIssuer {
        TokenIssuer::from_pkcs8(
            key_id,
            "issuer.test",
            &TokenIssuer::generate_pkcs8().unwrap(),
            3600,
        )
        .unwrap()
    }
    fn request(cert: CertFingerprint) -> TokenRequest<'static> {
        TokenRequest {
            audience: "svc",
            subject: "alice",
            session: Some(7),
            cert_sha256: cert,
            lifetime_secs: 60,
            scopes: vec![
                Scope::Connect,
                Scope::JoinSession {
                    session: 7,
                    epoch: 2,
                },
            ],
        }
    }
    fn verifier(issuer: &TokenIssuer) -> TokenVerifier {
        TokenVerifier::new(VerifierConfig::default(), [issuer.trusted_key()]).unwrap()
    }
    fn expect<'a>(scope: &'a Scope, cert: &'a CertFingerprint, single_use: bool) -> Expect<'a> {
        Expect {
            audience: "svc",
            scope,
            cert_sha256: cert,
            single_use,
        }
    }
    fn expect_for<'a>(
        audience: &'a str,
        scope: &'a Scope,
        cert: &'a CertFingerprint,
    ) -> Expect<'a> {
        Expect {
            audience,
            scope,
            cert_sha256: cert,
            single_use: false,
        }
    }

    #[test]
    fn round_trip_preserves_every_claim() {
        let signer = issuer("k1");
        let cert = fingerprint(b"cert");
        let token = signer.issue(&request(cert), NOW).unwrap();
        assert!(token.len() <= MAX_TOKEN_BYTES);
        let verified = verifier(&signer)
            .verify(&token, &expect(&Scope::Connect, &cert, false), NOW + 1)
            .unwrap();
        let claims = verified.claims;
        assert_eq!(claims.subject, "alice");
        assert_eq!(claims.session, Some(7));
        assert_eq!((claims.issued_at, claims.expires_at), (NOW, NOW + 60));
        assert_eq!(claims.scopes.len(), 2);
        // The encoding is canonical: parse then re-encode reproduces the signed payload.
        let (parsed, payload, _) = parse(&token).unwrap();
        assert_eq!(parsed.encode_payload().unwrap(), payload);
    }

    #[test]
    fn rejects_expired_future_and_overlong_tokens() {
        let signer = issuer("k1");
        let cert = fingerprint(b"cert");
        let token = signer.issue(&request(cert), NOW).unwrap();
        let v = verifier(&signer);
        let e = expect(&Scope::Connect, &cert, false);
        assert_eq!(v.verify(&token, &e, NOW + 60 + 29).map(|_| ()), Ok(()));
        assert_eq!(
            v.verify(&token, &e, NOW + 60 + 30).unwrap_err(),
            TokenError::Expired
        );
        assert_eq!(
            v.verify(&token, &e, NOW - 31).unwrap_err(),
            TokenError::NotYetValid
        );
        let long = TokenVerifier::new(
            VerifierConfig {
                max_lifetime_secs: 30,
                ..VerifierConfig::default()
            },
            [signer.trusted_key()],
        )
        .unwrap();
        assert_eq!(
            long.verify(&token, &e, NOW).unwrap_err(),
            TokenError::LifetimeTooLong
        );
        assert!(
            signer
                .issue(
                    &TokenRequest {
                        lifetime_secs: 3601,
                        ..request(cert)
                    },
                    NOW
                )
                .is_err()
        );
    }

    #[test]
    fn rejects_wrong_audience_scope_key_issuer_and_certificate() {
        let signer = issuer("k1");
        let other = issuer("k1");
        let cert = fingerprint(b"cert");
        let token = signer.issue(&request(cert), NOW).unwrap();
        let v = verifier(&signer);
        let mut e = expect(&Scope::Connect, &cert, false);
        e.audience = "other";
        assert_eq!(v.verify(&token, &e, NOW), Err(TokenError::WrongAudience));
        let missing = Scope::Relay {
            session: 7,
            channel: [1; 16],
            party: RelayParty::A,
        };
        assert_eq!(
            v.verify(&token, &expect(&missing, &cert, false), NOW),
            Err(TokenError::MissingScope)
        );
        let wrong_epoch = Scope::JoinSession {
            session: 7,
            epoch: 3,
        };
        assert_eq!(
            v.verify(&token, &expect(&wrong_epoch, &cert, false), NOW),
            Err(TokenError::MissingScope)
        );
        let stranger = fingerprint(b"another certificate");
        assert_eq!(
            v.verify(&token, &expect(&Scope::Connect, &stranger, false), NOW),
            Err(TokenError::WrongCertificate)
        );
        // Same key id, different key material: signature check fails.
        assert_eq!(
            verifier(&other).verify(&token, &expect(&Scope::Connect, &cert, false), NOW),
            Err(TokenError::BadSignature)
        );
        // Unknown key id.
        let renamed = verifier(&issuer("k2"));
        assert_eq!(
            renamed.verify(&token, &expect(&Scope::Connect, &cert, false), NOW),
            Err(TokenError::UnknownKey)
        );
        // A trusted key may only assert its configured issuer.
        let pinned =
            TrustedKey::new("k1", "someone.else", *signer.trusted_key().public_key()).unwrap();
        let v = TokenVerifier::new(VerifierConfig::default(), [pinned]).unwrap();
        assert_eq!(
            v.verify(&token, &expect(&Scope::Connect, &cert, false), NOW),
            Err(TokenError::WrongIssuer)
        );
    }

    #[test]
    fn every_single_byte_tamper_is_rejected() {
        let signer = issuer("k1");
        let cert = fingerprint(b"cert");
        let token = signer.issue(&request(cert), NOW).unwrap();
        let v = verifier(&signer);
        let e = expect(&Scope::Connect, &cert, false);
        assert!(v.verify(&token, &e, NOW).is_ok());
        for i in 0..token.len() {
            for bit in [0x01, 0x80] {
                let mut bad = token.clone();
                bad[i] ^= bit;
                assert!(v.verify(&bad, &e, NOW).is_err(), "byte {i} bit {bit:#x}");
            }
        }
        for len in 0..token.len() {
            assert!(v.verify(&token[..len], &e, NOW).is_err(), "truncated {len}");
        }
        let mut extended = token.clone();
        extended.push(0);
        assert!(v.verify(&extended, &e, NOW).is_err());
    }

    #[test]
    fn single_use_tokens_are_consumed_only_after_all_checks_pass() {
        let signer = issuer("k1");
        let cert = fingerprint(b"cert");
        let token = signer.issue(&request(cert), NOW).unwrap();
        let v = verifier(&signer);
        let stranger = fingerprint(b"stranger");
        // A failed attempt (wrong certificate) must not burn the token for its owner.
        assert!(
            v.verify(&token, &expect(&Scope::Connect, &stranger, true), NOW)
                .is_err()
        );
        assert_eq!(v.replay_entries(), 0);
        assert!(
            v.verify(&token, &expect(&Scope::Connect, &cert, true), NOW)
                .is_ok()
        );
        assert_eq!(
            v.verify(&token, &expect(&Scope::Connect, &cert, true), NOW),
            Err(TokenError::Replayed)
        );
        // Entries disappear after the token can no longer verify.
        let later = signer.issue(&request(cert), NOW + 1000).unwrap();
        assert!(
            v.verify(&later, &expect(&Scope::Connect, &cert, true), NOW + 1000)
                .is_ok()
        );
        assert_eq!(v.replay_entries(), 1);
    }

    #[test]
    fn replay_cache_is_bounded_and_fails_closed() {
        let signer = issuer("k1");
        let v = TokenVerifier::new(
            VerifierConfig {
                replay_capacity: 3,
                ..VerifierConfig::default()
            },
            [signer.trusted_key()],
        )
        .unwrap();
        // Three different certificates fill the cache; a fourth is refused (fail closed).
        let certs = [
            fingerprint(b"cert-1"),
            fingerprint(b"cert-2"),
            fingerprint(b"cert-3"),
            fingerprint(b"cert-4"),
        ];
        for cert in &certs[..3] {
            let token = signer.issue(&request(*cert), NOW).unwrap();
            assert!(
                v.verify(&token, &expect(&Scope::Connect, cert, true), NOW)
                    .is_ok()
            );
        }
        let token = signer.issue(&request(certs[3]), NOW).unwrap();
        assert_eq!(
            v.verify(&token, &expect(&Scope::Connect, &certs[3], true), NOW),
            Err(TokenError::ReplayCacheFull)
        );
        assert_eq!(v.replay_entries(), 3);
        // Once the earlier tokens expire (plus skew) capacity returns.
        let fresh = signer.issue(&request(certs[3]), NOW + 200).unwrap();
        assert!(
            v.verify(&fresh, &expect(&Scope::Connect, &certs[3], true), NOW + 200)
                .is_ok()
        );
    }

    /// L4: one certificate used to be able to fill the global replay cache, which made every
    /// other certificate's single-use tokens fail until the entries expired (up to 24 h).
    #[test]
    fn one_fingerprint_cannot_fill_the_replay_cache_for_everyone() {
        let signer = issuer("k1");
        let v = TokenVerifier::new(
            VerifierConfig {
                replay_capacity: 6,
                ..VerifierConfig::default()
            },
            [signer.trusted_key()],
        )
        .unwrap();
        let (a, b) = (fingerprint(b"cert-a"), fingerprint(b"cert-b"));
        let expect_a = expect(&Scope::Connect, &a, true);
        for _ in 0..10 {
            let token = signer.issue(&request(a), NOW).unwrap();
            let _ = v.verify(&token, &expect_a, NOW);
        }
        assert!(v.replay_entries() <= 6);
        let token = signer.issue(&request(b), NOW).unwrap();
        assert!(
            v.verify(&token, &expect(&Scope::Connect, &b, true), NOW)
                .is_ok(),
            "another certificate's login is unaffected"
        );
    }

    #[test]
    fn a_fingerprint_over_its_share_evicts_only_its_own_oldest_nonce() {
        let signer = issuer("k1");
        let v = TokenVerifier::new(
            VerifierConfig {
                replay_capacity: 100,
                replay_per_fingerprint: 3,
                ..VerifierConfig::default()
            },
            [signer.trusted_key()],
        )
        .unwrap();
        let (a, b) = (fingerprint(b"cert-a"), fingerprint(b"cert-b"));
        let mine: Vec<Vec<u8>> = (0..5)
            .map(|_| signer.issue(&request(a), NOW).unwrap())
            .collect();
        let theirs = signer.issue(&request(b), NOW).unwrap();
        let e_a = expect(&Scope::Connect, &a, true);
        let e_b = expect(&Scope::Connect, &b, true);
        assert!(v.verify(&theirs, &e_b, NOW).is_ok());
        for token in &mine {
            assert!(v.verify(token, &e_a, NOW).is_ok());
        }
        assert_eq!(v.replay_entries_for(&a), 3, "capped at its share");
        assert_eq!(
            v.replay_entries_for(&b),
            1,
            "the other fingerprint is untouched"
        );
        // Recent tokens stay replay-protected; only the oldest were forgotten.
        assert_eq!(v.verify(&mine[4], &e_a, NOW), Err(TokenError::Replayed));
        assert_eq!(v.verify(&theirs, &e_b, NOW), Err(TokenError::Replayed));
        assert!(
            v.verify(&mine[0], &e_a, NOW).is_ok(),
            "the evicted nonce is the owner's own oldest"
        );
        assert!(v.replay_entries() <= 4);
    }

    #[test]
    fn relay_credentials_are_pair_scoped_bearer_tokens_without_certificate_binding() {
        let signer = issuer("k1");
        let v = verifier(&signer);
        let scope = Scope::Relay {
            session: 9,
            channel: [5; 16],
            party: RelayParty::B,
        };
        let mint = |scopes: Vec<Scope>, audience: &'static str| {
            signer
                .issue(
                    &TokenRequest {
                        audience,
                        subject: "bob",
                        session: Some(9),
                        cert_sha256: fingerprint(b"bob-cert"),
                        lifetime_secs: 60,
                        scopes,
                    },
                    NOW,
                )
                .unwrap()
        };
        let token = mint(vec![scope.clone()], "relay");
        let claims = v.verify_relay(&token, "relay", NOW + 1).unwrap();
        assert_eq!(
            (
                claims.session,
                claims.channel,
                claims.party,
                claims.expires_at
            ),
            (9, [5; 16], RelayParty::B, NOW + 60)
        );
        assert_eq!(claims.subject, "bob");
        // Retransmitted allocate requests present the credential repeatedly.
        assert!(v.verify_relay(&token, "relay", NOW + 2).is_ok());
        assert_eq!(v.replay_entries(), 0);
        // The exact scope (channel and party) is part of what the signature covers.
        let cert = fingerprint(b"bob-cert");
        assert!(
            v.verify(&token, &expect_for("relay", &scope, &cert), NOW)
                .is_ok()
        );
        let other_party = Scope::Relay {
            session: 9,
            channel: [5; 16],
            party: RelayParty::A,
        };
        assert_eq!(
            v.verify(&token, &expect_for("relay", &other_party, &cert), NOW),
            Err(TokenError::MissingScope)
        );
        let other_channel = Scope::Relay {
            session: 9,
            channel: [6; 16],
            party: RelayParty::B,
        };
        assert_eq!(
            v.verify(&token, &expect_for("relay", &other_channel, &cert), NOW),
            Err(TokenError::MissingScope)
        );
        // Audience, expiry, signature and scope are still enforced.
        assert_eq!(
            v.verify_relay(&token, "game", NOW).unwrap_err(),
            TokenError::WrongAudience
        );
        assert_eq!(
            v.verify_relay(&token, "relay", NOW + 60 + 30).unwrap_err(),
            TokenError::Expired
        );
        let connect_only = mint(vec![Scope::Connect], "relay");
        assert_eq!(
            v.verify_relay(&connect_only, "relay", NOW).unwrap_err(),
            TokenError::MissingScope
        );
        let mut forged = token.clone();
        let last = forged.len() - 1;
        forged[last] ^= 1;
        assert!(v.verify_relay(&forged, "relay", NOW).is_err());
        let mut tampered = token;
        tampered[40] ^= 1;
        assert!(v.verify_relay(&tampered, "relay", NOW).is_err());
        // An unknown party byte is malformed, not silently A.
        let mut bytes = vec![3];
        bytes.extend_from_slice(&1u64.to_be_bytes());
        bytes.extend_from_slice(&[0; 16]);
        bytes.push(2);
        assert!(matches!(
            Scope::decode(&mut Reader(&bytes)),
            Err(TokenError::Malformed("relay party"))
        ));
    }

    #[test]
    fn key_rotation_overlaps_and_revocation_is_immediate() {
        let old = issuer("2026-a");
        let new = issuer("2026-b");
        let cert = fingerprint(b"cert");
        let v = TokenVerifier::new(VerifierConfig::default(), [old.trusted_key()]).unwrap();
        let old_token = old.issue(&request(cert), NOW).unwrap();
        let new_token = new.issue(&request(cert), NOW).unwrap();
        let e = expect(&Scope::Connect, &cert, false);
        assert!(v.verify(&old_token, &e, NOW).is_ok());
        assert_eq!(v.verify(&new_token, &e, NOW), Err(TokenError::UnknownKey));
        v.add_key(new.trusted_key()).unwrap();
        assert!(v.verify(&old_token, &e, NOW).is_ok());
        assert!(v.verify(&new_token, &e, NOW).is_ok());
        assert!(v.remove_key("2026-a").unwrap());
        assert_eq!(v.verify(&old_token, &e, NOW), Err(TokenError::UnknownKey));
        assert_eq!(v.key_ids(), vec!["2026-b".to_owned()]);
    }

    #[test]
    fn invalid_requests_never_produce_tokens() {
        let signer = issuer("k1");
        let cert = fingerprint(b"c");
        let bad = |mutate: fn(&mut TokenRequest<'static>)| {
            let mut r = request(cert);
            mutate(&mut r);
            signer.issue(&r, NOW).is_err()
        };
        assert!(bad(|r| r.audience = ""));
        assert!(bad(|r| r.audience = "has space"));
        assert!(bad(|r| r.subject = ""));
        assert!(bad(|r| r.subject = "line\nbreak"));
        assert!(bad(|r| r.scopes = vec![]));
        assert!(bad(|r| r.scopes = vec![Scope::Connect, Scope::Connect]));
        assert!(bad(|r| r.scopes = vec![Scope::Custom("bad scope".into())]));
        assert!(bad(|r| r.lifetime_secs = 0));
        assert!(
            TokenIssuer::from_pkcs8("k", "i", b"not a key", 10).is_err(),
            "garbage key material"
        );
        assert!(
            TokenIssuer::from_pkcs8("bad id", "i", &TokenIssuer::generate_pkcs8().unwrap(), 10)
                .is_err()
        );
    }

    #[test]
    fn openssl_style_pkcs8_v1_keys_load() {
        // PKCS#8 v1 Ed25519 wrapper (as written by `openssl genpkey -algorithm ed25519`).
        let mut der = vec![
            0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22,
            0x04, 0x20,
        ];
        der.extend_from_slice(&[7u8; 32]);
        let signer = TokenIssuer::from_pkcs8("k1", "iss", &der, 600).unwrap();
        let cert = fingerprint(b"c");
        let token = signer.issue(&request(cert), NOW).unwrap();
        let v = TokenVerifier::new(VerifierConfig::default(), [signer.trusted_key()]).unwrap();
        assert!(
            v.verify(&token, &expect(&Scope::Connect, &cert, false), NOW)
                .is_ok()
        );
    }

    #[test]
    fn truncated_scope_and_garbage_inputs_do_not_panic() {
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let signer = issuer("k1");
        let cert = fingerprint(b"cert");
        let v = verifier(&signer);
        let e = expect(&Scope::Connect, &cert, true);
        for _ in 0..2000 {
            let len = (next() % 1200) as usize;
            let junk: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            assert!(v.verify(&junk, &e, NOW).is_err());
        }
        assert_eq!(v.replay_entries(), 0);
    }
}
