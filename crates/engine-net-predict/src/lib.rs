//! Deterministic, sans-IO prediction toolkit for engine multiplayer sessions.
//!
//! The crate never owns sockets or clocks. Every component consumes and yields bounded byte
//! payloads that the application sends over any `engine_network::MessageEndpoint` on
//! [`PREDICTION_LANE`], and every component advances only when the caller passes it a tick.
//!
//! - [`FixedTimestep`] and [`TickRing`]: fixed-step accumulation and bounded tick-keyed history.
//! - [`PredictionClient`] over a [`PredictedSimulation`]: client-side prediction with server
//!   reconciliation, plus [`ErrorSmoother`] for visual correction smoothing.
//! - [`InputSender`] / [`InputReceiver`]: redundant unreliable input transport through the
//!   bounded [`NetCodec`] binary codec, with server-side dedup, lead window and missing-input
//!   policy.
//! - [`PredictionLedger`], [`PredictionHooks`] and [`KeyValidator`]: prediction keys for
//!   gameplay/ability systems (begin, confirm, reject, timeout, dependent keys).
//! - [`RollbackSession`] over a [`RollbackGame`]: GGPO-style rollback for peer-to-peer or
//!   relayed sessions.
//! - [`ColliderHistory`] and [`LagCompensator`]: bounded hitbox history with rewound queries.
//!
//! Payload bytes from peers are untrusted: decoders are hand-written, bounded, never panic and
//! fail closed with [`PredictError::Malformed`].
mod codec;
mod input;
mod keys;
mod lagcomp;
mod prediction;
mod ring;
mod rollback;
mod smoothing;
mod timestep;

pub use codec::{NetCodec, Reader, Writer, decode_value, encode_value};
pub use input::{
    InputAck, InputMessage, InputReceiver, InputReceiverConfig, InputReceiverStats, InputSender,
    InputSenderConfig, MAX_INPUTS_PER_MESSAGE, MissingInputPolicy, ReceiveSummary, StoreOutcome,
    TakenInput,
};
pub use keys::{
    HookFns, KeyRejection, KeyResolution, KeyValidator, KeyValidatorConfig, KeyVerdict,
    LedgerConfig, LedgerStats, PredictionHooks, PredictionKey, PredictionLedger, UndoReason,
    decode_verdicts, encode_verdicts,
};
pub use lagcomp::{
    ColliderHistory, ColliderHistoryConfig, HitEntity, HitShape, Hitbox, LagCompensator,
    OverlapHit, Pose, QueryFilter, Ray, RayHit, Rewind, RewindError,
};
pub use prediction::{
    PredictedSimulation, PredictionClient, PredictionConfig, ReconcileRejection, ReconcileReport,
};
pub use ring::{MAX_RING_CAPACITY, TickRing};
pub use rollback::{
    AdvanceStatus, MAX_PLAYERS, RollbackConfig, RollbackEvent, RollbackGame, RollbackSession,
    RollbackStats, TimeSync,
};
pub use smoothing::ErrorSmoother;
pub use timestep::{FixedTimestep, TimestepTicks};

use engine_network::{Delivery, Lane};
use std::fmt;

/// Lane for all prediction, input, prediction-key and rollback traffic.
pub const PREDICTION_LANE: Lane = Lane::INPUT;
/// Client input messages are latency-critical and carry their own redundancy.
pub const INPUT_DELIVERY: Delivery = Delivery::Unreliable;
/// Input acknowledgements may be lost; a newer acknowledgement supersedes an older one.
pub const INPUT_ACK_DELIVERY: Delivery = Delivery::Unreliable;
/// Prediction-key verdicts must arrive: a lost reject would leave a predicted side effect.
pub const KEY_DELIVERY: Delivery = Delivery::ReliableOrdered;
/// Rollback messages resend every unacknowledged input, so loss is tolerated.
pub const ROLLBACK_DELIVERY: Delivery = Delivery::Unreliable;

/// Wire version byte that starts every message this crate encodes.
pub const WIRE_VERSION: u8 = 1;

/// Message kind byte (second byte of every message) so one lane can carry all message types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MessageKind {
    Inputs,
    InputAck,
    KeyVerdicts,
    Rollback,
}
impl MessageKind {
    pub(crate) fn byte(self) -> u8 {
        match self {
            Self::Inputs => 1,
            Self::InputAck => 2,
            Self::KeyVerdicts => 3,
            Self::Rollback => 4,
        }
    }
    /// Classifies a payload by its two-byte header without decoding the body.
    pub fn peek(bytes: &[u8]) -> Option<Self> {
        match bytes {
            [WIRE_VERSION, 1, ..] => Some(Self::Inputs),
            [WIRE_VERSION, 2, ..] => Some(Self::InputAck),
            [WIRE_VERSION, 3, ..] => Some(Self::KeyVerdicts),
            [WIRE_VERSION, 4, ..] => Some(Self::Rollback),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PredictError {
    /// Malformed or out-of-limit bytes from a peer.
    Malformed(&'static str),
    /// Caller-side misuse or invalid configuration.
    Invalid(&'static str),
    /// A bounded buffer is full; nothing was changed.
    Full(&'static str),
}
impl fmt::Display for PredictError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(e) => write!(f, "malformed prediction message: {e}"),
            Self::Invalid(e) => write!(f, "invalid prediction usage: {e}"),
            Self::Full(e) => write!(f, "prediction buffer full: {e}"),
        }
    }
}
impl std::error::Error for PredictError {}
pub type Result<T> = std::result::Result<T, PredictError>;
