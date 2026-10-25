//! Versioned, bounded, fail-closed control protocol carried on `Lane::SESSION`
//! (`Delivery::ReliableOrdered`, one JSON document per transport payload).
//!
//! JSON was chosen over a bespoke binary format for this control plane because the traffic is low
//! rate (rate-limited per peer), it must be inspectable and easy to implement from other
//! languages, and serde's derive with `deny_unknown_fields` gives strict, schema-checked parsing:
//! unknown fields, duplicate fields, wrong types, unknown variants and trailing data are all
//! rejected. Safety does not depend on the parser alone: the payload size is capped *before*
//! parsing (so no allocation exceeds the cap), `serde_json` bounds nesting depth, and every
//! decoded field is then re-validated against [`OnlineLimits`] before any state is touched.
//! Binary blobs (tokens, connectivity candidates, migration state) are base64url strings without
//! padding. Latency-critical gameplay traffic never uses this lane.
//!
//! ```text
//! {"v":1,"id":7,"body":{"type":"create_session","name":"Lobby","mode":"duel","capacity":4}}
//! ```
use crate::{
    limits::OnlineLimits,
    text::{valid_attributes, valid_ident, valid_text},
    token::MAX_TOKEN_BYTES,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{collections::BTreeMap, fmt};

pub const PROTOCOL_VERSION: u16 = 1;
pub type SessionId = u64;
pub type TicketId = u64;
pub type Attributes = BTreeMap<String, String>;
/// Longest join code accepted on the wire (real codes are [`crate::lobby::CODE_LEN`] characters).
pub const MAX_CODE_BYTES: usize = 32;
const MAX_IDENT_BYTES: usize = 32;
/// Hard ceiling on ratings; keeps skill arithmetic far from overflow.
pub const MAX_RATING: i32 = 100_000;

/// Opaque bytes carried as base64url. `Debug` never prints the content (tokens travel in Blobs).
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Blob(pub Vec<u8>);
impl fmt::Debug for Blob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Blob({} bytes)", self.0.len())
    }
}
impl Serialize for Blob {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&URL_SAFE_NO_PAD.encode(&self.0))
    }
}
impl<'de> Deserialize<'de> for Blob {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        URL_SAFE_NO_PAD
            .decode(text.as_bytes())
            .map(Blob)
            .map_err(|_| serde::de::Error::custom("invalid base64url blob"))
    }
}

/// Who runs a session's authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hosting {
    /// A member hosts and the host migrates when it leaves.
    Player,
    /// A dedicated server runs the game; the session has no player host.
    Dedicated,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSession {
    pub name: String,
    pub mode: String,
    pub capacity: u16,
    #[serde(default)]
    pub private: bool,
    #[serde(default)]
    pub attributes: Attributes,
    /// The creator's connectivity candidates (opaque; consumed by the NAT traversal layer).
    #[serde(default)]
    pub candidates: Option<Blob>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionFilter {
    #[serde(default)]
    pub mode: Option<String>,
    /// Exact-match attribute constraints.
    #[serde(default)]
    pub attributes: Attributes,
    /// Only sessions with a free slot.
    #[serde(default)]
    pub has_space: bool,
    #[serde(default)]
    pub name_prefix: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case", deny_unknown_fields)]
pub enum JoinTarget {
    /// Public sessions only.
    Id { session_id: SessionId },
    /// Private join code.
    Code { code: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketSpec {
    pub mode: String,
    pub region: String,
    /// Skill rating. Servers with a [`crate::PlayerDirectory`] rating override this claim.
    pub rating: i32,
    /// Local players this connection represents (split-screen guests count toward team size).
    #[serde(default = "one")]
    pub party_size: u8,
    #[serde(default)]
    pub attributes: Attributes,
    /// Measured latency to regions, in milliseconds.
    #[serde(default)]
    pub latencies: BTreeMap<String, u16>,
    #[serde(default)]
    pub candidates: Option<Blob>,
}
fn one() -> u8 {
    1
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientBody {
    Hello {
        token: Blob,
    },
    CreateSession(CreateSession),
    ListSessions {
        #[serde(default)]
        filter: SessionFilter,
        #[serde(default)]
        after: Option<SessionId>,
        limit: u16,
    },
    JoinSession {
        target: JoinTarget,
        #[serde(default)]
        candidates: Option<Blob>,
    },
    LeaveSession {
        session_id: SessionId,
    },
    SetReady {
        session_id: SessionId,
        ready: bool,
    },
    /// Host only; `epoch` must equal the session's current epoch.
    Kick {
        session_id: SessionId,
        epoch: u64,
        player: String,
    },
    /// Host only; replaces all session attributes.
    SetAttributes {
        session_id: SessionId,
        epoch: u64,
        attributes: Attributes,
    },
    SetCandidates {
        session_id: SessionId,
        candidates: Blob,
    },
    /// Fresh join token for the game server (members only).
    RequestJoinToken {
        session_id: SessionId,
    },
    /// Relay credential for the pair (you, `peer`): both must be members of the session. The
    /// credential names the channel derived from the session and the two subjects and the slot
    /// (A or B) fixed by subject order, so it cannot be used for any other pair or slot.
    /// Requires a configured relay audience.
    RequestRelayToken {
        session_id: SessionId,
        peer: String,
    },
    /// Host only: replaces the private join code (the old one stops working).
    RotateCode {
        session_id: SessionId,
        epoch: u64,
    },
    /// Registers the SHA-256 (32 bytes) of the certificate your game server presents when you
    /// are the session's host. Joiners pin the current host's fingerprint.
    SetGameIdentity {
        session_id: SessionId,
        cert_sha256: Blob,
    },
    /// Introduces you to the member `peer` for NAT traversal: their candidates plus a punch
    /// secret derived for exactly your pair. Both sides obtain the same secret by asking.
    RequestPeerIntro {
        session_id: SessionId,
        peer: String,
    },
    /// Host only: newest game state for host migration.
    UploadMigrationState {
        session_id: SessionId,
        epoch: u64,
        state: Blob,
    },
    EnqueueTicket(TicketSpec),
    CancelTicket {
        ticket: TicketId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Full,
    RateLimited,
    LimitExceeded,
    Conflict,
    StaleEpoch,
    Unsupported,
    Internal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaveReason {
    Left,
    Disconnected,
    Kicked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenKind {
    Join,
    Relay,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberView {
    pub player: String,
    pub ready: bool,
    pub host: bool,
    pub team: Option<u8>,
    pub candidates: Option<Blob>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionView {
    pub id: SessionId,
    pub name: String,
    pub mode: String,
    pub capacity: u16,
    pub private: bool,
    pub hosting: Hosting,
    /// Host epoch; increments on every host change.
    pub epoch: u64,
    pub host: Option<String>,
    /// SHA-256 of the certificate the current host's game server presents, once the host
    /// registered it (`SetGameIdentity`). Joiners pin this value.
    #[serde(default)]
    pub host_fingerprint: Option<Blob>,
    pub attributes: Attributes,
    pub members: Vec<MemberView>,
    /// Private join code, shown only to the current host.
    pub code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSummary {
    pub id: SessionId,
    pub name: String,
    pub mode: String,
    pub capacity: u16,
    pub members: u16,
    pub hosting: Hosting,
    pub host: Option<String>,
    pub attributes: Attributes,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServerBody {
    HelloOk {
        subject: String,
        expires_at: u64,
    },
    Error {
        code: ErrorCode,
        message: String,
    },
    Ack {},
    SessionCreated {
        session: SessionView,
        join_token: Blob,
    },
    SessionList {
        sessions: Vec<SessionSummary>,
        next: Option<SessionId>,
    },
    Joined {
        session: SessionView,
        join_token: Blob,
    },
    Left {
        session_id: SessionId,
    },
    Token {
        session_id: SessionId,
        kind: TokenKind,
        epoch: u64,
        token: Blob,
    },
    /// Answer to `RotateCode`: the new join code.
    CodeRotated {
        session_id: SessionId,
        epoch: u64,
        code: String,
    },
    /// Answer to `RequestPeerIntro`. `punch_secret` (32 bytes) is known only to the two members
    /// of the pair and the service; `controlling` is the role you take when punching.
    PeerIntro {
        session_id: SessionId,
        peer: String,
        candidates: Option<Blob>,
        punch_secret: Blob,
        controlling: bool,
    },
    TicketQueued {
        ticket: TicketId,
    },
    TicketCancelled {
        ticket: TicketId,
    },
    // Notifications (envelope id 0).
    MemberJoined {
        session_id: SessionId,
        member: MemberView,
    },
    MemberLeft {
        session_id: SessionId,
        player: String,
        reason: LeaveReason,
    },
    MemberReady {
        session_id: SessionId,
        player: String,
        ready: bool,
    },
    MemberCandidates {
        session_id: SessionId,
        player: String,
        candidates: Blob,
    },
    AttributesChanged {
        session_id: SessionId,
        epoch: u64,
        attributes: Attributes,
    },
    /// The host changed. Carries a join token for the new epoch, the new host's registered game
    /// certificate fingerprint (if any) and, to the new host only, the newest migration state
    /// and the *rotated* join code (the old code stops working at every host change).
    HostChanged {
        session_id: SessionId,
        epoch: u64,
        host: String,
        you_are_host: bool,
        candidates: Option<Blob>,
        #[serde(default)]
        host_fingerprint: Option<Blob>,
        join_token: Option<Blob>,
        state: Option<Blob>,
        code: Option<String>,
    },
    /// The current host registered or changed its game certificate fingerprint.
    HostIdentityChanged {
        session_id: SessionId,
        epoch: u64,
        host_fingerprint: Blob,
    },
    /// To the host after a kick: the join code was rotated because the kicked member may
    /// have known it.
    CodeChanged {
        session_id: SessionId,
        epoch: u64,
        code: String,
    },
    Kicked {
        session_id: SessionId,
    },
    MatchFound {
        ticket: TicketId,
        session: SessionView,
        join_token: Blob,
    },
    TicketExpired {
        ticket: TicketId,
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientEnvelope {
    pub v: u16,
    pub id: u32,
    pub body: ClientBody,
}

/// `id` echoes the request; notifications use 0.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerEnvelope {
    pub v: u16,
    pub id: u32,
    pub body: ServerBody,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    Oversized,
    Malformed,
    Version(u16),
    Invalid(&'static str),
}
impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Oversized => f.write_str("message exceeds the size limit"),
            Self::Malformed => f.write_str("malformed message"),
            Self::Version(v) => write!(f, "unsupported protocol version {v}"),
            Self::Invalid(why) => write!(f, "invalid message: {why}"),
        }
    }
}
impl std::error::Error for ProtocolError {}

impl ClientEnvelope {
    pub fn encode(&self, limits: &OnlineLimits) -> Result<Vec<u8>, ProtocolError> {
        encode(self, limits.max_message_bytes)
    }
    /// Size cap, strict parse, version check, then field validation.
    pub fn decode(payload: &[u8], limits: &OnlineLimits) -> Result<Self, ProtocolError> {
        Self::decode_keeping_id(payload, limits).map_err(|(error, _)| error)
    }
    /// Like [`decode`](Self::decode), but a failure after the envelope itself parsed also returns
    /// the request ID so the reply can echo it (0 when the ID is unknown).
    pub fn decode_keeping_id(
        payload: &[u8],
        limits: &OnlineLimits,
    ) -> Result<Self, (ProtocolError, u32)> {
        if payload.len() > limits.max_message_bytes {
            return Err((ProtocolError::Oversized, 0));
        }
        let envelope: Self =
            serde_json::from_slice(payload).map_err(|_| (ProtocolError::Malformed, 0))?;
        if envelope.v != PROTOCOL_VERSION {
            return Err((ProtocolError::Version(envelope.v), envelope.id));
        }
        envelope
            .body
            .validate(limits)
            .map_err(|why| (ProtocolError::Invalid(why), envelope.id))?;
        Ok(envelope)
    }
}
impl ServerEnvelope {
    pub fn encode(&self, limits: &OnlineLimits) -> Result<Vec<u8>, ProtocolError> {
        encode(self, limits.max_message_bytes)
    }
    pub fn decode(payload: &[u8], limits: &OnlineLimits) -> Result<Self, ProtocolError> {
        if payload.len() > limits.max_message_bytes {
            return Err(ProtocolError::Oversized);
        }
        let envelope: Self =
            serde_json::from_slice(payload).map_err(|_| ProtocolError::Malformed)?;
        if envelope.v != PROTOCOL_VERSION {
            return Err(ProtocolError::Version(envelope.v));
        }
        Ok(envelope)
    }
}
fn encode<T: Serialize>(value: &T, max: usize) -> Result<Vec<u8>, ProtocolError> {
    let bytes = serde_json::to_vec(value).map_err(|_| ProtocolError::Malformed)?;
    if bytes.len() > max {
        return Err(ProtocolError::Oversized);
    }
    Ok(bytes)
}

impl ClientBody {
    /// Field-level bounds. Called by [`ClientEnvelope::decode`]; nothing is stored before it passes.
    pub fn validate(&self, limits: &OnlineLimits) -> Result<(), &'static str> {
        let attributes_ok = |attributes: &Attributes, max: usize| {
            valid_attributes(
                attributes,
                max,
                limits.max_attribute_key_bytes,
                limits.max_attribute_value_bytes,
            )
        };
        let candidates_ok = |blob: &Blob| blob.0.len() <= limits.max_candidates_bytes;
        match self {
            Self::Hello { token } => {
                if token.0.is_empty() || token.0.len() > MAX_TOKEN_BYTES {
                    return Err("token size");
                }
            }
            Self::CreateSession(create) => {
                if !valid_text(&create.name, limits.max_name_bytes) {
                    return Err("session name");
                }
                if !valid_ident(&create.mode, MAX_IDENT_BYTES) {
                    return Err("mode");
                }
                if create.capacity == 0 || create.capacity > limits.max_capacity {
                    return Err("capacity");
                }
                if !attributes_ok(&create.attributes, limits.max_attributes) {
                    return Err("attributes");
                }
                if create
                    .candidates
                    .as_ref()
                    .is_some_and(|b| !candidates_ok(b))
                {
                    return Err("candidates size");
                }
            }
            Self::ListSessions {
                filter,
                after: _,
                limit,
            } => {
                if *limit == 0 || usize::from(*limit) > limits.max_list_page {
                    return Err("page size");
                }
                if filter
                    .mode
                    .as_ref()
                    .is_some_and(|m| !valid_ident(m, MAX_IDENT_BYTES))
                {
                    return Err("filter mode");
                }
                if filter
                    .name_prefix
                    .as_ref()
                    .is_some_and(|p| !valid_text(p, limits.max_name_bytes))
                {
                    return Err("filter name");
                }
                if !attributes_ok(&filter.attributes, limits.max_filter_attributes) {
                    return Err("filter attributes");
                }
            }
            Self::JoinSession { target, candidates } => {
                if let JoinTarget::Code { code } = target
                    && (code.is_empty() || code.len() > MAX_CODE_BYTES)
                {
                    return Err("join code");
                }
                if candidates.as_ref().is_some_and(|b| !candidates_ok(b)) {
                    return Err("candidates size");
                }
            }
            Self::LeaveSession { .. }
            | Self::SetReady { .. }
            | Self::RequestJoinToken { .. }
            | Self::RotateCode { .. }
            | Self::CancelTicket { .. } => {}
            Self::RequestRelayToken { peer, .. } | Self::RequestPeerIntro { peer, .. } => {
                if !valid_text(peer, crate::token::MAX_NAME_BYTES) {
                    return Err("peer");
                }
            }
            Self::SetGameIdentity { cert_sha256, .. } => {
                if cert_sha256.0.len() != 32 {
                    return Err("certificate fingerprint");
                }
            }
            Self::Kick { player, .. } => {
                if !valid_text(player, crate::token::MAX_NAME_BYTES) {
                    return Err("player");
                }
            }
            Self::SetAttributes { attributes, .. } => {
                if !attributes_ok(attributes, limits.max_attributes) {
                    return Err("attributes");
                }
            }
            Self::SetCandidates { candidates, .. } => {
                if !candidates_ok(candidates) {
                    return Err("candidates size");
                }
            }
            Self::UploadMigrationState { state, .. } => {
                if state.0.len() > limits.max_migration_state_bytes {
                    return Err("state size");
                }
            }
            Self::EnqueueTicket(ticket) => {
                if !valid_ident(&ticket.mode, MAX_IDENT_BYTES) {
                    return Err("mode");
                }
                if !valid_ident(&ticket.region, MAX_IDENT_BYTES) {
                    return Err("region");
                }
                if !(0..=MAX_RATING).contains(&ticket.rating) {
                    return Err("rating");
                }
                if ticket.party_size == 0 || ticket.party_size > limits.max_party_size {
                    return Err("party size");
                }
                if !attributes_ok(&ticket.attributes, limits.max_attributes) {
                    return Err("attributes");
                }
                if ticket.latencies.len() > limits.max_latency_entries
                    || ticket
                        .latencies
                        .keys()
                        .any(|region| !valid_ident(region, MAX_IDENT_BYTES))
                {
                    return Err("latencies");
                }
                if ticket
                    .candidates
                    .as_ref()
                    .is_some_and(|b| !candidates_ok(b))
                {
                    return Err("candidates size");
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> OnlineLimits {
        OnlineLimits::default()
    }
    fn decode(json: &str) -> Result<ClientEnvelope, ProtocolError> {
        ClientEnvelope::decode(json.as_bytes(), &limits())
    }

    #[test]
    fn valid_messages_round_trip() {
        let create = ClientEnvelope {
            v: PROTOCOL_VERSION,
            id: 9,
            body: ClientBody::CreateSession(CreateSession {
                name: "Friday night".into(),
                mode: "duel".into(),
                capacity: 4,
                private: true,
                attributes: Attributes::from([("map".into(), "dust".into())]),
                candidates: Some(Blob(vec![1, 2, 3])),
            }),
        };
        let bytes = create.encode(&limits()).unwrap();
        assert_eq!(ClientEnvelope::decode(&bytes, &limits()).unwrap(), create);
        let list = decode(r#"{"v":1,"id":1,"body":{"type":"list_sessions","limit":10}}"#).unwrap();
        assert!(matches!(
            list.body,
            ClientBody::ListSessions { limit: 10, .. }
        ));
    }

    #[test]
    fn rejects_unknown_fields_variants_versions_and_trailing_data() {
        for bad in [
            r#"{"v":1,"id":1,"body":{"type":"leave_session","session_id":1,"extra":1}}"#,
            r#"{"v":1,"id":1,"body":{"type":"nope"}}"#,
            r#"{"v":1,"id":1,"body":{"type":"leave_session","session_id":1},"x":0}"#,
            r#"{"v":1,"id":1,"id":2,"body":{"type":"leave_session","session_id":1}}"#,
            r#"{"v":1,"id":1,"body":{"type":"leave_session","session_id":-1}}"#,
            r#"{"v":1,"id":1,"body":{"type":"leave_session","session_id":1}} trailing"#,
            r#"{"v":1,"id":1,"body":{"type":"hello","token":"not base64!"}}"#,
            r#"{"v":1,"id":1,"body":{"type":"hello","token":""}}"#,
            r#"[]"#,
            "",
            "\u{0}",
        ] {
            assert!(decode(bad).is_err(), "{bad}");
        }
        assert_eq!(
            decode(r#"{"v":2,"id":1,"body":{"type":"leave_session","session_id":1}}"#),
            Err(ProtocolError::Version(2))
        );
    }

    #[test]
    fn session_names_reject_bidi_controls_and_zero_width_characters() {
        let name = |name: &str| {
            let text = serde_json::json!({"v":1,"id":1,"body":{
                "type":"create_session","name":name,"mode":"m","capacity":2}});
            decode(&text.to_string())
        };
        assert!(name("Friday night 🎮").is_ok());
        for bad in [
            "evil\u{202E}gnp.exe",
            "a\u{200B}b",
            "a\u{200D}b",
            "a\u{2066}b",
            "\u{FEFF}lead",
        ] {
            assert!(name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn peer_and_identity_fields_are_validated() {
        for ok in [
            r#"{"v":1,"id":1,"body":{"type":"request_relay_token","session_id":1,"peer":"bob"}}"#,
            r#"{"v":1,"id":1,"body":{"type":"request_peer_intro","session_id":1,"peer":"bob"}}"#,
            r#"{"v":1,"id":1,"body":{"type":"rotate_code","session_id":1,"epoch":1}}"#,
        ] {
            assert!(decode(ok).is_ok(), "{ok}");
        }
        let fingerprint = |bytes: usize| {
            let blob = URL_SAFE_NO_PAD.encode(vec![7u8; bytes]);
            format!(
                r#"{{"v":1,"id":1,"body":{{"type":"set_game_identity","session_id":1,"cert_sha256":"{blob}"}}}}"#
            )
        };
        assert!(decode(&fingerprint(32)).is_ok());
        assert!(decode(&fingerprint(31)).is_err());
        assert!(decode(&fingerprint(33)).is_err());
        for bad in [
            r#"{"v":1,"id":1,"body":{"type":"request_relay_token","session_id":1}}"#,
            r#"{"v":1,"id":1,"body":{"type":"request_relay_token","session_id":1,"peer":""}}"#,
            r#"{"v":1,"id":1,"body":{"type":"request_peer_intro","session_id":1,"peer":"a\nb"}}"#,
        ] {
            assert!(decode(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn rejects_out_of_bounds_fields() {
        let big_name = "n".repeat(limits().max_name_bytes + 1);
        let cases = [
            format!(
                r#"{{"v":1,"id":1,"body":{{"type":"create_session","name":"{big_name}","mode":"m","capacity":2}}}}"#
            ),
            r#"{"v":1,"id":1,"body":{"type":"create_session","name":"a\nb","mode":"m","capacity":2}}"#.into(),
            r#"{"v":1,"id":1,"body":{"type":"create_session","name":"a","mode":"m","capacity":0}}"#.into(),
            r#"{"v":1,"id":1,"body":{"type":"create_session","name":"a","mode":"m","capacity":33}}"#.into(),
            r#"{"v":1,"id":1,"body":{"type":"create_session","name":"a","mode":"bad mode","capacity":2}}"#.into(),
            r#"{"v":1,"id":1,"body":{"type":"list_sessions","limit":0}}"#.into(),
            r#"{"v":1,"id":1,"body":{"type":"list_sessions","limit":26}}"#.into(),
            r#"{"v":1,"id":1,"body":{"type":"enqueue_ticket","mode":"m","region":"r","rating":-1}}"#.into(),
            r#"{"v":1,"id":1,"body":{"type":"enqueue_ticket","mode":"m","region":"r","rating":5,"party_size":0}}"#.into(),
            r#"{"v":1,"id":1,"body":{"type":"enqueue_ticket","mode":"m","region":"r","rating":5,"party_size":9}}"#.into(),
            r#"{"v":1,"id":1,"body":{"type":"join_session","target":{"by":"code","code":""}}}"#.into(),
        ];
        for case in cases {
            assert!(decode(&case).is_err(), "{case}");
        }
        let too_many: String = (0..=limits().max_attributes)
            .map(|i| format!(r#""k{i}":"v""#))
            .collect::<Vec<_>>()
            .join(",");
        assert!(
            decode(&format!(
                r#"{{"v":1,"id":1,"body":{{"type":"set_attributes","session_id":1,"epoch":1,"attributes":{{{too_many}}}}}}}"#
            ))
            .is_err()
        );
        let oversized = vec![b' '; limits().max_message_bytes + 1];
        assert_eq!(
            ClientEnvelope::decode(&oversized, &limits()),
            Err(ProtocolError::Oversized)
        );
    }

    #[test]
    fn deeply_nested_input_is_rejected_without_overflowing_the_stack() {
        let deep = format!("{}{}", "[".repeat(50_000), "]".repeat(50_000));
        assert!(
            ClientEnvelope::decode(
                deep.as_bytes(),
                &OnlineLimits {
                    max_message_bytes: 1024 * 1024,
                    ..limits()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn blob_debug_does_not_leak_contents() {
        let text = format!("{:?}", Blob(b"super-secret-token".to_vec()));
        assert!(!text.contains("secret") && text.contains("18"));
    }

    #[test]
    fn worst_case_session_view_fits_the_message_limit() {
        let l = limits();
        let member = |i: usize| MemberView {
            player: format!("{i:0>64}"),
            ready: true,
            host: i == 0,
            team: Some(255),
            candidates: Some(Blob(vec![0xFF; l.max_candidates_bytes])),
        };
        let view = SessionView {
            id: u64::MAX,
            name: "n".repeat(l.max_name_bytes),
            mode: "m".repeat(32),
            capacity: l.max_capacity,
            private: true,
            hosting: Hosting::Player,
            epoch: u64::MAX,
            host: Some("h".repeat(64)),
            host_fingerprint: Some(Blob(vec![0xFF; 32])),
            attributes: (0..l.max_attributes)
                .map(|i| {
                    (
                        format!("{i:0>width$}", width = l.max_attribute_key_bytes),
                        "v".repeat(l.max_attribute_value_bytes),
                    )
                })
                .collect(),
            members: (0..usize::from(l.max_capacity)).map(member).collect(),
            code: Some("A".repeat(16)),
        };
        let envelope = ServerEnvelope {
            v: PROTOCOL_VERSION,
            id: u32::MAX,
            body: ServerBody::Joined {
                session: view,
                join_token: Blob(vec![0xFF; MAX_TOKEN_BYTES]),
            },
        };
        let bytes = envelope.encode(&l).expect("worst case must fit");
        assert!(bytes.len() <= l.max_message_bytes);
        assert_eq!(ServerEnvelope::decode(&bytes, &l).unwrap(), envelope);
        let host_changed = ServerEnvelope {
            v: PROTOCOL_VERSION,
            id: 0,
            body: ServerBody::HostChanged {
                session_id: u64::MAX,
                epoch: u64::MAX,
                host: "h".repeat(64),
                you_are_host: true,
                candidates: Some(Blob(vec![0xFF; l.max_candidates_bytes])),
                host_fingerprint: Some(Blob(vec![0xFF; 32])),
                join_token: Some(Blob(vec![0xFF; MAX_TOKEN_BYTES])),
                state: Some(Blob(vec![0xFF; l.max_migration_state_bytes])),
                code: Some("A".repeat(16)),
            },
        };
        assert!(host_changed.encode(&l).is_ok());
    }
}
