//! Pair-scoped relay credentials and the adapter that lets an `engine-net-nat` relay accept them.
//!
//! A relay allocation is a two-party channel. The service mints a credential only for a member
//! who names another member of the same session (`RequestRelayToken`). The credential carries
//! `Scope::Relay { session, channel, party }`:
//!
//! - `channel` is `SHA-256(label || session || low subject || high subject)` truncated to 16
//!   bytes, where the two subjects are sorted by their bytes, so both members of a pair derive
//!   the same channel and nobody else can name it;
//! - `party` is `A` for the lexicographically lower subject and `B` for the higher one, so the
//!   two members of a pair hold different slots and neither can take the other's.
//!
//! **Security properties and limits.** A relay sees no TLS certificate, so a relay credential is
//! a *bearer* credential: whoever holds the bytes can occupy that one slot of that one channel
//! until the credential expires. It is bounded by (1) its expiry, which is also the hard end of
//! the allocation (`Grant::max_lifetime`), (2) the relay's return-routability cookie, so the
//! allocation binds to an address the holder can actually receive on, and (3) the fact that a
//! squatted slot is a denial of service, not interception: the relayed QUIC session still
//! requires mutual TLS with the expected certificate end to end.
use crate::token::{RelayClaims, RelayParty, TokenVerifier};
use engine_net_nat::{ChannelId, Grant, Party};
use ring::digest;
use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const CHANNEL_LABEL: &[u8] = b"engine-online/relay-channel/v1\0";

/// The two subjects in canonical (byte) order.
fn ordered<'a>(a: &'a str, b: &'a str) -> (&'a str, &'a str) {
    if a.as_bytes() <= b.as_bytes() {
        (a, b)
    } else {
        (b, a)
    }
}

/// The relay channel shared by exactly the pair `{a, b}` in `session`. Symmetric in `a` and `b`.
pub fn relay_channel(session: u64, a: &str, b: &str) -> [u8; 16] {
    let (low, high) = ordered(a, b);
    let mut context = digest::Context::new(&digest::SHA256);
    context.update(CHANNEL_LABEL);
    context.update(&session.to_be_bytes());
    for subject in [low, high] {
        context.update(&(subject.len() as u16).to_be_bytes());
        context.update(subject.as_bytes());
    }
    let mut channel = [0u8; 16];
    channel.copy_from_slice(&context.finish().as_ref()[..16]);
    channel
}

/// The slot `subject` holds in its channel with `peer`: `A` for the lower subject.
pub fn relay_party(subject: &str, peer: &str) -> RelayParty {
    if subject.as_bytes() <= peer.as_bytes() {
        RelayParty::A
    } else {
        RelayParty::B
    }
}

/// The relay grant a verified credential allows at `now_unix`: its channel and slot, ending at
/// the credential's expiry. `None` when it has no time left.
pub fn relay_grant(claims: &RelayClaims, now_unix: u64) -> Option<Grant> {
    let remaining = claims.expires_at.checked_sub(now_unix).filter(|&s| s > 0)?;
    Some(Grant {
        channel: ChannelId(claims.channel),
        party: match claims.party {
            RelayParty::A => Party::A,
            RelayParty::B => Party::B,
        },
        max_lifetime: Some(Duration::from_secs(remaining)),
    })
}

/// The credential policy for `engine_net_nat::RelayServer::new`: accepts exactly the relay
/// credentials this crate's service issues (valid signature from a key `verifier` trusts, the
/// relay `audience`, not expired, `Relay` scope) and grants that credential's channel and slot
/// with a hard deadline at its expiry. Uses the system clock; see [`relay_authorizer_with_clock`].
///
/// ```ignore
/// let authorizer = engine_online::relay::relay_authorizer(verifier, "engine-relay");
/// let server = RelayServer::new(Instant::now(), RelayLimits::default(), authorizer)?;
/// ```
pub fn relay_authorizer(
    verifier: Arc<TokenVerifier>,
    audience: impl Into<String>,
) -> impl Fn(&[u8], SocketAddr) -> Option<Grant> + Send + Sync + 'static {
    relay_authorizer_with_clock(verifier, audience, system_unix_secs)
}

/// [`relay_authorizer`] with an explicit wall clock (unix seconds), for tests and simulations.
pub fn relay_authorizer_with_clock(
    verifier: Arc<TokenVerifier>,
    audience: impl Into<String>,
    clock: impl Fn() -> u64 + Send + Sync + 'static,
) -> impl Fn(&[u8], SocketAddr) -> Option<Grant> + Send + Sync + 'static {
    let audience = audience.into();
    move |credential, _source| {
        let now = clock();
        let claims = verifier.verify_relay(credential, &audience, now).ok()?;
        relay_grant(&claims, now)
    }
}

fn system_unix_secs() -> u64 {
    // A clock before 1970 reads as 0, which makes every credential "not yet valid" (fail closed).
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_are_symmetric_per_session_and_distinct_per_pair() {
        let ab = relay_channel(7, "alice", "bob");
        assert_eq!(
            ab,
            relay_channel(7, "bob", "alice"),
            "same pair, same channel"
        );
        assert_ne!(ab, relay_channel(8, "alice", "bob"), "other session");
        assert_ne!(ab, relay_channel(7, "alice", "carol"), "other pair");
        assert_ne!(ab, relay_channel(7, "bob", "carol"));
        // Length prefixes keep "ab"+"c" and "a"+"bc" apart.
        assert_ne!(relay_channel(1, "ab", "c"), relay_channel(1, "a", "bc"));
    }

    #[test]
    fn the_two_members_of_a_pair_hold_different_slots() {
        assert_eq!(relay_party("alice", "bob"), RelayParty::A);
        assert_eq!(relay_party("bob", "alice"), RelayParty::B);
    }

    #[test]
    fn grants_end_at_the_credential_expiry() {
        let claims = RelayClaims {
            subject: "alice".into(),
            session: 1,
            channel: [3; 16],
            party: RelayParty::B,
            expires_at: 1_000,
        };
        let grant = relay_grant(&claims, 940).unwrap();
        assert_eq!(grant.channel, ChannelId([3; 16]));
        assert_eq!(grant.party, Party::B);
        assert_eq!(grant.max_lifetime, Some(Duration::from_secs(60)));
        assert!(relay_grant(&claims, 1_000).is_none());
        assert!(relay_grant(&claims, 2_000).is_none());
    }
}
