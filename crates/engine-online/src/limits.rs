//! Trusted resource limits. Every wire field is bounded by one of these before it is stored.
use crate::OnlineError;
use serde::Deserialize;

/// Hard limits for the control plane. Operator configuration is trusted; wire input is not.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OnlineLimits {
    /// Largest accepted or produced control message (JSON bytes).
    pub max_message_bytes: usize,
    /// Authenticated plus authenticating peers the service tracks.
    pub max_peers: usize,
    /// A peer must present a valid token within this time of being admitted by the transport.
    pub auth_timeout_ms: u64,
    pub max_sessions: usize,
    pub max_capacity: u16,
    /// Sessions one peer may be a member of at once ("max sessions per peer").
    pub max_memberships_per_peer: usize,
    pub max_name_bytes: usize,
    pub max_attributes: usize,
    pub max_attribute_key_bytes: usize,
    pub max_attribute_value_bytes: usize,
    /// Opaque per-member connectivity-candidate blob.
    pub max_candidates_bytes: usize,
    pub max_migration_state_bytes: usize,
    pub max_list_page: usize,
    pub max_filter_attributes: usize,
    /// Sustained requests per second per peer, and the burst allowed above it.
    pub requests_per_second: u32,
    pub request_burst: u32,
    /// Rate-limited requests tolerated per peer before it is disconnected.
    pub max_rate_violations: u32,
    /// Undecodable or invalid messages tolerated per authenticated peer before disconnect.
    pub max_malformed_messages: u32,
    /// Failed join-code guesses per peer / for the whole server inside `join_code_window_ms`.
    pub join_code_failures_per_peer: u32,
    pub join_code_failures_global: u32,
    pub join_code_window_ms: u64,
    pub max_tickets: usize,
    pub max_tickets_per_mode: usize,
    pub max_party_size: u8,
    pub max_latency_entries: usize,
    /// Minimum time between matchmaking passes.
    pub match_interval_ms: u64,
    /// Oldest tickets examined as match anchors per mode per pass (bounds tick cost).
    pub max_anchors_per_tick: usize,
}
impl Default for OnlineLimits {
    fn default() -> Self {
        Self {
            max_message_bytes: 64 * 1024,
            max_peers: 1024,
            auth_timeout_ms: 5_000,
            max_sessions: 1024,
            max_capacity: 32,
            max_memberships_per_peer: 1,
            max_name_bytes: 48,
            max_attributes: 8,
            max_attribute_key_bytes: 24,
            max_attribute_value_bytes: 64,
            max_candidates_bytes: 512,
            max_migration_state_bytes: 32 * 1024,
            max_list_page: 25,
            max_filter_attributes: 4,
            requests_per_second: 20,
            request_burst: 40,
            max_rate_violations: 200,
            max_malformed_messages: 8,
            join_code_failures_per_peer: 5,
            join_code_failures_global: 100,
            join_code_window_ms: 60_000,
            max_tickets: 4096,
            max_tickets_per_mode: 1024,
            max_party_size: 8,
            max_latency_entries: 8,
            match_interval_ms: 1_000,
            max_anchors_per_tick: 256,
        }
    }
}
impl OnlineLimits {
    /// Rejects limits that would make the service unsafe or unusable. The message ceilings keep
    /// the worst-case full session and session-list responses encodable.
    pub fn validate(&self) -> Result<(), OnlineError> {
        let ok = (4 * 1024..=1024 * 1024).contains(&self.max_message_bytes)
            && self.max_peers > 0
            && self.auth_timeout_ms > 0
            && self.max_sessions > 0
            && (1..=64).contains(&self.max_capacity)
            && (1..=8).contains(&self.max_memberships_per_peer)
            && (1..=128).contains(&self.max_name_bytes)
            && self.max_attributes <= 32
            && (1..=64).contains(&self.max_attribute_key_bytes)
            && self.max_attribute_value_bytes <= 256
            && (1..=1024).contains(&self.max_candidates_bytes)
            && self.max_migration_state_bytes <= 256 * 1024
            && (1..=100).contains(&self.max_list_page)
            && self.max_filter_attributes <= self.max_attributes
            && self.requests_per_second > 0
            && self.request_burst > 0
            && self.max_rate_violations > 0
            && self.max_malformed_messages > 0
            && self.join_code_failures_per_peer > 0
            && self.join_code_failures_global > 0
            && self.join_code_window_ms > 0
            && self.max_tickets > 0
            && self.max_tickets_per_mode > 0
            && (1..=16).contains(&self.max_party_size)
            && self.max_latency_entries <= 32
            && self.match_interval_ms > 0
            && self.max_anchors_per_tick > 0
            && self.worst_case_session_bytes() < self.max_message_bytes
            && self.worst_case_state_bytes() < self.max_message_bytes
            && self.worst_case_list_bytes() < self.max_message_bytes;
        if ok {
            Ok(())
        } else {
            Err(OnlineError::Invalid("invalid online limits"))
        }
    }
    fn attribute_bytes(&self) -> usize {
        self.max_attributes * (self.max_attribute_key_bytes + self.max_attribute_value_bytes + 12)
    }
    /// Conservative JSON size of a full session view.
    pub(crate) fn worst_case_session_bytes(&self) -> usize {
        let member = 64 + self.max_candidates_bytes * 4 / 3 + 96;
        usize::from(self.max_capacity) * member
            + self.attribute_bytes()
            + 2 * self.max_name_bytes
            + 2048
    }
    /// Conservative JSON size of a host-changed notification carrying migration state.
    pub(crate) fn worst_case_state_bytes(&self) -> usize {
        self.max_migration_state_bytes * 4 / 3 + self.max_candidates_bytes * 4 / 3 + 4096
    }
    pub(crate) fn worst_case_list_bytes(&self) -> usize {
        self.max_list_page * (self.attribute_bytes() + 2 * self.max_name_bytes + 128) + 256
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_dangerous_values_are_rejected() {
        assert!(OnlineLimits::default().validate().is_ok());
        for bad in [
            OnlineLimits {
                max_message_bytes: 100,
                ..OnlineLimits::default()
            },
            OnlineLimits {
                max_capacity: 0,
                ..OnlineLimits::default()
            },
            OnlineLimits {
                max_memberships_per_peer: 0,
                ..OnlineLimits::default()
            },
            OnlineLimits {
                max_migration_state_bytes: 200 * 1024,
                ..OnlineLimits::default()
            },
        ] {
            assert!(bad.validate().is_err());
        }
    }
}
