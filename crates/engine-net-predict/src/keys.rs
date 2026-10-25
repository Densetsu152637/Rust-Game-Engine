//! Prediction keys for gameplay and ability systems (GAS-style).
//!
//! A client issues one [`PredictionKey`] per predicted action, tags every predicted side
//! effect with it, and sends the key with the action request. The server admits the key
//! through its per-peer [`KeyValidator`], runs the action authoritatively and answers with a
//! [`KeyVerdict`]. The client's [`PredictionLedger`] then commits or undoes the tagged side
//! effects through [`PredictionHooks`]. A key begun while another key is pending can name it
//! as its parent: undoing a parent undoes every pending descendant, and a child confirmed
//! before its parent commits only once the whole ancestor chain is confirmed.
//!
//! Verdict wire format: `u8 version | u8 kind (3) | u8 count (1..=255) |
//! count x (u32 key | u8 accepted)`.
use crate::{MessageKind, NetCodec, PredictError, Reader, Result, WIRE_VERSION, Writer};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;

/// Per-session action identifier; keys increase monotonically from 1 and are never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PredictionKey(NonZeroU32);
impl PredictionKey {
    pub fn new(raw: u32) -> Option<Self> {
        NonZeroU32::new(raw).map(Self)
    }
    pub fn get(self) -> u32 {
        self.0.get()
    }
}
impl NetCodec for PredictionKey {
    const MAX_ENCODED_BYTES: usize = 4;
    fn encode(&self, w: &mut Writer) -> Result<()> {
        w.put_u32(self.get())
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Self::new(r.u32()?).ok_or(PredictError::Malformed("prediction key 0 is invalid"))
    }
}

/// Why predicted side effects are being undone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UndoReason {
    /// The server rejected this key.
    Rejected,
    /// No verdict arrived within `LedgerConfig::timeout_ticks`.
    TimedOut,
    /// An ancestor key was rejected, timed out or cleared.
    ParentUndone,
    /// `PredictionLedger::clear` (disconnect, level change).
    Cleared,
}

/// Native callbacks that own the predicted side effects tagged with a key.
pub trait PredictionHooks {
    /// The key and its whole ancestor chain are confirmed: keep (or hand over to replicated
    /// state) every side effect tagged with `key`.
    fn commit(&mut self, key: PredictionKey);
    /// Revert every side effect tagged with `key`. Called newest key first, so effects unwind
    /// in reverse order of prediction.
    fn undo(&mut self, key: PredictionKey, reason: UndoReason);
}

/// Closure adapter: `HookFns(|key| .., |key, reason| ..)`.
pub struct HookFns<C, U>(pub C, pub U);
impl<C, U> PredictionHooks for HookFns<C, U>
where
    C: FnMut(PredictionKey),
    U: FnMut(PredictionKey, UndoReason),
{
    fn commit(&mut self, key: PredictionKey) {
        (self.0)(key)
    }
    fn undo(&mut self, key: PredictionKey, reason: UndoReason) {
        (self.1)(key, reason)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerConfig {
    /// Pending keys a client may hold; `begin` fails with `Full` beyond it.
    pub max_pending: usize,
    /// Ticks after `begin` before an unanswered key is undone.
    pub timeout_ticks: u64,
    /// Longest parent chain (a root key has depth 0).
    pub max_depth: u8,
}
impl Default for LedgerConfig {
    fn default() -> Self {
        Self {
            max_pending: 64,
            timeout_ticks: 120,
            max_depth: 8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryState {
    Pending,
    /// Server confirmed the key but an ancestor is still unresolved.
    ConfirmedAwaitingParent,
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    key: PredictionKey,
    parent: Option<PredictionKey>,
    began_tick: u64,
    depth: u8,
    state: EntryState,
}

/// Result of applying a verdict or timeout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyResolution {
    /// The key is not pending (already resolved, undone with a parent, or never issued).
    Unknown,
    /// Confirmed, but waiting for an ancestor's verdict.
    Deferred,
    /// `commit` ran for this many keys (the key plus deferred descendants it released).
    Committed(usize),
    /// `undo` ran for this many keys (the key plus pending descendants).
    Undone(usize),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct LedgerStats {
    pub begun: u64,
    pub committed: u64,
    pub rejected: u64,
    pub timed_out: u64,
    pub undone_with_parent: u64,
}

/// Client-side ledger of predicted actions.
#[derive(Clone, Debug)]
pub struct PredictionLedger {
    config: LedgerConfig,
    next: u32,
    entries: Vec<Entry>,
    stats: LedgerStats,
}
impl PredictionLedger {
    pub fn new(config: LedgerConfig) -> Result<Self> {
        if config.max_pending == 0 || config.max_pending > 4096 {
            return Err(PredictError::Invalid("max_pending must be 1..=4096"));
        }
        if config.timeout_ticks == 0 {
            return Err(PredictError::Invalid("timeout_ticks must be positive"));
        }
        Ok(Self {
            entries: Vec::with_capacity(config.max_pending),
            config,
            next: 1,
            stats: LedgerStats::default(),
        })
    }
    pub fn stats(&self) -> LedgerStats {
        self.stats
    }
    pub fn pending_len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_pending(&self, key: PredictionKey) -> bool {
        self.find(key).is_some()
    }
    pub fn parent_of(&self, key: PredictionKey) -> Option<PredictionKey> {
        self.find(key).and_then(|i| self.entries[i].parent)
    }
    fn find(&self, key: PredictionKey) -> Option<usize> {
        self.entries.binary_search_by_key(&key, |e| e.key).ok()
    }
    /// Issues a key for a predicted action begun at `now_tick`, optionally dependent on a
    /// still-pending `parent`.
    pub fn begin(&mut self, now_tick: u64, parent: Option<PredictionKey>) -> Result<PredictionKey> {
        if self.entries.len() >= self.config.max_pending {
            return Err(PredictError::Full("too many pending prediction keys"));
        }
        let depth = match parent {
            None => 0,
            Some(p) => {
                let i = self
                    .find(p)
                    .ok_or(PredictError::Invalid("parent key is not pending"))?;
                let depth = self.entries[i].depth + 1;
                if depth > self.config.max_depth {
                    return Err(PredictError::Invalid("prediction key chain too deep"));
                }
                depth
            }
        };
        let key = PredictionKey::new(self.next).ok_or_else(exhausted)?;
        self.next = self.next.checked_add(1).ok_or_else(exhausted)?;
        self.entries.push(Entry {
            key,
            parent,
            began_tick: now_tick,
            depth,
            state: EntryState::Pending,
        });
        self.stats.begun += 1;
        Ok(key)
    }
    /// Applies a server verdict.
    pub fn apply(
        &mut self,
        verdict: KeyVerdict,
        hooks: &mut impl PredictionHooks,
    ) -> KeyResolution {
        if verdict.accepted {
            self.confirm(verdict.key, hooks)
        } else {
            self.reject(verdict.key, hooks)
        }
    }
    pub fn confirm(
        &mut self,
        key: PredictionKey,
        hooks: &mut impl PredictionHooks,
    ) -> KeyResolution {
        let Some(i) = self.find(key) else {
            return KeyResolution::Unknown;
        };
        if self.entries[i]
            .parent
            .is_some_and(|p| self.find(p).is_some())
        {
            self.entries[i].state = EntryState::ConfirmedAwaitingParent;
            return KeyResolution::Deferred;
        }
        // Commit the key, then every deferred descendant whose chain is now fully confirmed.
        // Parents always precede children in key order, so one ascending pass suffices.
        let mut committed = vec![key];
        let mut keep = Vec::with_capacity(self.entries.len());
        for e in self.entries.drain(..) {
            let releases = e.key == key
                || (e.state == EntryState::ConfirmedAwaitingParent
                    && e.parent.is_some_and(|p| committed.contains(&p)));
            if releases {
                if e.key != key {
                    committed.push(e.key);
                }
            } else {
                keep.push(e);
            }
        }
        self.entries = keep;
        for &k in &committed {
            hooks.commit(k);
        }
        self.stats.committed += committed.len() as u64;
        KeyResolution::Committed(committed.len())
    }
    pub fn reject(
        &mut self,
        key: PredictionKey,
        hooks: &mut impl PredictionHooks,
    ) -> KeyResolution {
        if self.find(key).is_none() {
            return KeyResolution::Unknown;
        }
        let n = self.undo_roots(&[key], UndoReason::Rejected, hooks);
        self.stats.rejected += 1;
        KeyResolution::Undone(n)
    }
    /// Undoes pending keys older than the timeout; returns how many keys timed out (excluding
    /// descendants undone with them).
    pub fn expire(&mut self, now_tick: u64, hooks: &mut impl PredictionHooks) -> usize {
        let roots: Vec<PredictionKey> = self
            .entries
            .iter()
            .filter(|e| {
                e.state == EntryState::Pending
                    && now_tick.saturating_sub(e.began_tick) >= self.config.timeout_ticks
            })
            .map(|e| e.key)
            .collect();
        if roots.is_empty() {
            return 0;
        }
        self.undo_roots(&roots, UndoReason::TimedOut, hooks);
        self.stats.timed_out += roots.len() as u64;
        roots.len()
    }
    /// Undoes every pending key (disconnect, map change).
    pub fn clear(&mut self, hooks: &mut impl PredictionHooks) -> usize {
        let n = self.entries.len();
        for e in self.entries.drain(..).rev() {
            hooks.undo(e.key, UndoReason::Cleared);
        }
        n
    }
    fn undo_roots(
        &mut self,
        roots: &[PredictionKey],
        reason: UndoReason,
        hooks: &mut impl PredictionHooks,
    ) -> usize {
        let mut doomed: Vec<(PredictionKey, UndoReason)> = Vec::new();
        let mut keep = Vec::with_capacity(self.entries.len());
        for e in self.entries.drain(..) {
            if roots.contains(&e.key) {
                doomed.push((e.key, reason));
            } else if e
                .parent
                .is_some_and(|p| doomed.iter().any(|(k, _)| *k == p))
            {
                doomed.push((e.key, UndoReason::ParentUndone));
            } else {
                keep.push(e);
            }
        }
        self.entries = keep;
        for &(k, r) in doomed.iter().rev() {
            if r == UndoReason::ParentUndone {
                self.stats.undone_with_parent += 1;
            }
            hooks.undo(k, r);
        }
        doomed.len()
    }
}

fn exhausted() -> PredictError {
    PredictError::Invalid("prediction keys exhausted for this session")
}

/// Server answer for one key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyVerdict {
    pub key: PredictionKey,
    pub accepted: bool,
}
impl NetCodec for KeyVerdict {
    const MAX_ENCODED_BYTES: usize = 5;
    fn encode(&self, w: &mut Writer) -> Result<()> {
        w.put(&self.key)?;
        w.put_bool(self.accepted)
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            key: r.get()?,
            accepted: r.bool()?,
        })
    }
}

const MAX_VERDICTS: usize = 255;

/// Encodes 1..=255 verdicts into one reliable message.
pub fn encode_verdicts(verdicts: &[KeyVerdict]) -> Result<Vec<u8>> {
    if verdicts.is_empty() || verdicts.len() > MAX_VERDICTS {
        return Err(PredictError::Invalid(
            "verdict message needs 1..=255 verdicts",
        ));
    }
    let mut w = Writer::with_limit(3 + verdicts.len() * KeyVerdict::MAX_ENCODED_BYTES);
    w.put_u8(WIRE_VERSION)?;
    w.put_u8(MessageKind::KeyVerdicts.byte())?;
    w.put_u8(verdicts.len() as u8)?;
    for v in verdicts {
        w.put(v)?;
    }
    Ok(w.into_bytes())
}

/// Decodes an untrusted verdict message.
pub fn decode_verdicts(bytes: &[u8]) -> Result<Vec<KeyVerdict>> {
    if bytes.len() > 3 + MAX_VERDICTS * KeyVerdict::MAX_ENCODED_BYTES {
        return Err(PredictError::Malformed("verdict message above byte limit"));
    }
    let mut r = Reader::new(bytes);
    if r.u8()? != WIRE_VERSION || r.u8()? != MessageKind::KeyVerdicts.byte() {
        return Err(PredictError::Malformed("not a verdict message"));
    }
    let count = usize::from(r.u8()?);
    if count == 0 {
        return Err(PredictError::Malformed("empty verdict message"));
    }
    if r.remaining() != count * KeyVerdict::MAX_ENCODED_BYTES {
        return Err(PredictError::Malformed(
            "verdict count does not match length",
        ));
    }
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(r.get()?);
    }
    r.finish()?;
    Ok(out)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyValidatorConfig {
    /// Replay-detection window in keys (64..=65536, multiple of 64). Keys at or below
    /// `highest - window` are rejected as too old.
    pub window: u32,
    /// Largest jump above the highest key seen (stops a peer from exhausting the key space).
    pub max_ahead: u32,
    /// Admitted keys awaiting `resolve`.
    pub max_outstanding: usize,
}
impl Default for KeyValidatorConfig {
    fn default() -> Self {
        Self {
            window: 1024,
            max_ahead: 256,
            max_outstanding: 64,
        }
    }
}

/// Why the server refused a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyRejection {
    /// The key was already used.
    Replayed,
    /// Older than the replay window.
    TooOld,
    /// Further above the highest seen key than `max_ahead`.
    TooFarAhead,
    TooManyOutstanding,
    /// The parent is unknown, outside the window, or not older than the child.
    UnknownParent,
    /// The parent was rejected.
    ParentRejected,
    /// `resolve` for a key that is not outstanding.
    NotOutstanding,
}
impl KeyRejection {
    /// Whether to send a reject verdict so an honest client undoes its prediction. Replays
    /// and unknown resolutions are dropped silently.
    pub fn notify_client(self) -> bool {
        !matches!(self, Self::Replayed | Self::NotOutstanding)
    }
}

/// Server-side per-peer key validation.
#[derive(Clone, Debug)]
pub struct KeyValidator {
    config: KeyValidatorConfig,
    highest: Option<u32>,
    seen: Vec<u64>,
    rejected: Vec<u64>,
    outstanding: Vec<(PredictionKey, Option<PredictionKey>)>,
}
impl KeyValidator {
    pub fn new(config: KeyValidatorConfig) -> Result<Self> {
        if config.window < 64 || config.window > 65536 || !config.window.is_multiple_of(64) {
            return Err(PredictError::Invalid(
                "window must be 64..=65536 and a multiple of 64",
            ));
        }
        if config.max_ahead == 0 || config.max_ahead > config.window {
            return Err(PredictError::Invalid("max_ahead must be 1..=window"));
        }
        if config.max_outstanding == 0 || config.max_outstanding > 4096 {
            return Err(PredictError::Invalid("max_outstanding must be 1..=4096"));
        }
        let words = (config.window / 64) as usize;
        Ok(Self {
            seen: vec![0; words],
            rejected: vec![0; words],
            outstanding: Vec::with_capacity(config.max_outstanding),
            config,
            highest: None,
        })
    }
    pub fn outstanding_len(&self) -> usize {
        self.outstanding.len()
    }
    fn bit(&self, key: u32) -> (usize, u64) {
        let i = key % self.config.window;
        ((i / 64) as usize, 1u64 << (i % 64))
    }
    fn in_window(&self, key: u32) -> bool {
        self.highest
            .is_some_and(|h| key <= h && h - key < self.config.window)
    }
    fn test(bits: &[u64], (w, m): (usize, u64)) -> bool {
        bits[w] & m != 0
    }
    fn mark_rejected(&mut self, key: u32) {
        if self.in_window(key) {
            let (w, m) = self.bit(key);
            self.rejected[w] |= m;
        }
    }
    /// Checks a client key (and optional parent) before the action runs. On success the key
    /// is consumed and outstanding until [`KeyValidator::resolve`]. A key that fails after
    /// passing the replay check is still consumed and recorded as rejected.
    pub fn admit(
        &mut self,
        key: PredictionKey,
        parent: Option<PredictionKey>,
    ) -> std::result::Result<(), KeyRejection> {
        let k = key.get();
        match self.highest {
            None if k > self.config.max_ahead => return Err(KeyRejection::TooFarAhead),
            Some(h) if k > h && k - h > self.config.max_ahead => {
                return Err(KeyRejection::TooFarAhead);
            }
            Some(h) if k <= h && h - k >= self.config.window => return Err(KeyRejection::TooOld),
            Some(h) if k <= h && Self::test(&self.seen, self.bit(k)) => {
                return Err(KeyRejection::Replayed);
            }
            _ => {}
        }
        self.advance_to(k);
        let (w, m) = self.bit(k);
        self.seen[w] |= m;
        let failure = if let Some(p) = parent {
            let pk = p.get();
            if pk >= k || !self.in_window(pk) || !Self::test(&self.seen, self.bit(pk)) {
                Some(KeyRejection::UnknownParent)
            } else if Self::test(&self.rejected, self.bit(pk)) {
                Some(KeyRejection::ParentRejected)
            } else {
                None
            }
        } else {
            None
        };
        let failure = failure.or_else(|| {
            (self.outstanding.len() >= self.config.max_outstanding)
                .then_some(KeyRejection::TooManyOutstanding)
        });
        if let Some(f) = failure {
            self.mark_rejected(k);
            return Err(f);
        }
        self.outstanding.push((key, parent));
        Ok(())
    }
    fn advance_to(&mut self, k: u32) {
        let from = match self.highest {
            Some(h) if k <= h => return,
            Some(h) => h,
            None => 0,
        };
        if k - from >= self.config.window {
            self.seen.iter_mut().for_each(|w| *w = 0);
            self.rejected.iter_mut().for_each(|w| *w = 0);
        } else {
            for slot in from + 1..=k {
                let (w, m) = self.bit(slot);
                self.seen[w] &= !m;
                self.rejected[w] &= !m;
            }
        }
        self.highest = Some(k);
    }
    /// Resolves an outstanding key. A rejection also rejects outstanding descendants. Returns
    /// the verdicts to send (key first).
    pub fn resolve(
        &mut self,
        key: PredictionKey,
        accepted: bool,
    ) -> std::result::Result<Vec<KeyVerdict>, KeyRejection> {
        let i = self
            .outstanding
            .iter()
            .position(|(k, _)| *k == key)
            .ok_or(KeyRejection::NotOutstanding)?;
        self.outstanding.remove(i);
        let mut verdicts = vec![KeyVerdict { key, accepted }];
        if accepted {
            return Ok(verdicts);
        }
        self.mark_rejected(key.get());
        let mut doomed = vec![key];
        // Outstanding entries are in admission order, and parents are admitted before children.
        self.outstanding.retain(|(k, p)| {
            if p.is_some_and(|p| doomed.contains(&p)) {
                doomed.push(*k);
                false
            } else {
                true
            }
        });
        for &k in &doomed[1..] {
            self.mark_rejected(k.get());
            verdicts.push(KeyVerdict {
                key: k,
                accepted: false,
            });
        }
        Ok(verdicts)
    }
}
