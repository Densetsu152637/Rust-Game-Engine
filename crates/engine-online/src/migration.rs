//! Host migration for player-hosted sessions: deterministic election and the state hook.
//!
//! The lobby tracks a host and an `epoch`. When the host leaves, disconnects or is force-moved,
//! [`elect_host`] picks the remaining member with the lowest transport-measured RTT to the
//! service (unmeasured sorts last), then the earliest join, then the lowest peer ID. The epoch
//! increases by one and every member receives the new host, its connectivity candidates and a
//! fresh join token bound to the new epoch. Requests that carry the old epoch are rejected, and
//! a game server that verifies `Scope::JoinSession { session, epoch }` refuses tokens minted for
//! an older epoch, so a deposed host cannot admit players or alter the session.
use crate::protocol::SessionId;
use engine_network::PeerId;
use std::collections::BTreeMap;

/// One election candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub peer: PeerId,
    /// RTT measured by the transport between the member and the service; `None` if unmeasured.
    pub rtt_ms: Option<u32>,
    /// Monotonic join order within the session (smaller is earlier).
    pub join_seq: u64,
}

/// Deterministic election: lowest RTT, then earliest join, then lowest peer ID. The result does
/// not depend on slice order.
pub fn elect_host(candidates: &[Candidate]) -> Option<PeerId> {
    candidates
        .iter()
        .min_by_key(|c| (c.rtt_ms.unwrap_or(u32::MAX), c.join_seq, c.peer))
        .map(|c| c.peer)
}

/// Application hook that carries bounded game state across a host change.
///
/// The current host periodically uploads opaque bytes (`UploadMigrationState`, accepted only from
/// the current host at the current epoch). On migration the service asks [`restore`] for the
/// newest state and hands it to the new host. The default [`InMemoryMigrationState`] keeps the
/// latest upload per session; implement the trait to validate against your own schema, compress,
/// or persist elsewhere. Implementations must be fast and must not trust the bytes.
///
/// [`restore`]: MigrationState::restore
pub trait MigrationState: Send {
    /// Called with state from the current host. Return false to reject it.
    fn accept(&mut self, session: SessionId, epoch: u64, state: &[u8]) -> bool;
    /// State to give the new host as epoch `new_epoch` begins; `None` if there is none.
    fn restore(&mut self, session: SessionId, new_epoch: u64) -> Option<Vec<u8>>;
    /// The session ended; release anything held for it.
    fn discard(&mut self, session: SessionId);
}

/// Keeps the newest accepted state per session, bounded in bytes per session and in sessions.
pub struct InMemoryMigrationState {
    max_bytes: usize,
    max_sessions: usize,
    states: BTreeMap<SessionId, Vec<u8>>,
}
impl InMemoryMigrationState {
    pub fn new(max_bytes: usize, max_sessions: usize) -> Self {
        Self {
            max_bytes,
            max_sessions,
            states: BTreeMap::new(),
        }
    }
}
impl MigrationState for InMemoryMigrationState {
    fn accept(&mut self, session: SessionId, _epoch: u64, state: &[u8]) -> bool {
        if state.len() > self.max_bytes {
            return false;
        }
        if !self.states.contains_key(&session) && self.states.len() >= self.max_sessions {
            return false;
        }
        self.states.insert(session, state.to_vec());
        true
    }
    fn restore(&mut self, session: SessionId, _new_epoch: u64) -> Option<Vec<u8>> {
        self.states.get(&session).cloned()
    }
    fn discard(&mut self, session: SessionId) {
        self.states.remove(&session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(peer: PeerId, rtt_ms: Option<u32>, join_seq: u64) -> Candidate {
        Candidate {
            peer,
            rtt_ms,
            join_seq,
        }
    }

    #[test]
    fn election_prefers_low_rtt_then_early_join_then_low_peer() {
        assert_eq!(elect_host(&[]), None);
        assert_eq!(
            elect_host(&[c(1, Some(80), 0), c(2, Some(20), 1), c(3, Some(50), 2)]),
            Some(2)
        );
        // Equal RTT: the earlier joiner wins even with a higher peer ID.
        assert_eq!(elect_host(&[c(1, Some(30), 5), c(9, Some(30), 2)]), Some(9));
        // Unmeasured RTT sorts after any measurement.
        assert_eq!(elect_host(&[c(1, None, 0), c(2, Some(900), 9)]), Some(2));
        assert_eq!(elect_host(&[c(5, None, 3), c(4, None, 3)]), Some(4));
    }

    #[test]
    fn election_is_independent_of_input_order() {
        let set = [
            c(1, Some(40), 3),
            c(2, Some(40), 1),
            c(3, Some(70), 0),
            c(4, None, 2),
        ];
        let expected = elect_host(&set);
        for rotation in 0..set.len() {
            let mut rotated = set.to_vec();
            rotated.rotate_left(rotation);
            assert_eq!(elect_host(&rotated), expected);
            rotated.reverse();
            assert_eq!(elect_host(&rotated), expected);
        }
        assert_eq!(expected, Some(2));
    }

    #[test]
    fn in_memory_state_is_bounded_and_discarded() {
        let mut store = InMemoryMigrationState::new(4, 1);
        assert!(store.accept(1, 1, b"abcd"));
        assert!(!store.accept(1, 1, b"abcde"));
        assert!(!store.accept(2, 1, b"x"), "session table is bounded");
        assert!(store.accept(1, 1, b"zz"));
        assert_eq!(store.restore(1, 2), Some(b"zz".to_vec()));
        store.discard(1);
        assert_eq!(store.restore(1, 2), None);
    }
}
