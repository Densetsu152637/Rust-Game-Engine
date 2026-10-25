//! Reference credential schemes for standalone relay deployments. Integration with the
//! online services passes its own [`crate::Authorizer`] instead.
use crate::{ChannelId, Error, Grant, Party, Result, wire};
use ring::hmac;
use std::{
    fmt,
    net::SocketAddr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const LABEL: &[u8] = b"engine-net-nat/relay-credential/v1";
const VERSION: u8 = 1;
const SIGNED_LEN: usize = 1 + 16 + 1 + 8;

/// Stateless HMAC-SHA256 credentials shared between an issuer (lobby/backend) and the relay:
/// `[version 1][channel 16][party][expiry unix seconds u64 BE][tag 32]` (58 bytes). The tag
/// covers a domain label and every field; verification is constant time.
pub struct HmacCredentials {
    key: hmac::Key,
}
impl fmt::Debug for HmacCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HmacCredentials(..)")
    }
}
impl HmacCredentials {
    pub const CREDENTIAL_LEN: usize = SIGNED_LEN + 32;

    /// `secret` must be at least 32 bytes of key material.
    pub fn new(secret: &[u8]) -> Result<Self> {
        if secret.len() < 32 {
            return Err(Error::Invalid(
                "relay HMAC secret must be at least 32 bytes",
            ));
        }
        Ok(Self {
            key: hmac::Key::new(hmac::HMAC_SHA256, secret),
        })
    }
    fn tag(&self, signed: &[u8]) -> hmac::Tag {
        let mut context = hmac::Context::with_key(&self.key);
        context.update(LABEL);
        context.update(signed);
        context.sign()
    }
    pub fn mint(&self, channel: ChannelId, party: Party, expires_unix_secs: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::CREDENTIAL_LEN);
        out.push(VERSION);
        out.extend_from_slice(&channel.0);
        out.push(party.code());
        out.extend_from_slice(&expires_unix_secs.to_be_bytes());
        let tag = self.tag(&out);
        out.extend_from_slice(tag.as_ref());
        out
    }
    /// The grant of a valid, unexpired credential; its lifetime is capped at the expiry.
    pub fn verify(&self, credential: &[u8], now_unix_secs: u64) -> Option<Grant> {
        if credential.len() != Self::CREDENTIAL_LEN || credential[0] != VERSION {
            return None;
        }
        let (signed, tag) = credential.split_at(SIGNED_LEN);
        let expected = self.tag(signed);
        if !wire::ct_eq(expected.as_ref(), tag) {
            return None;
        }
        let channel = ChannelId(signed[1..17].try_into().ok()?);
        let party = Party::from_code(signed[17])?;
        let expires = u64::from_be_bytes(signed[18..26].try_into().ok()?);
        let remaining = expires.checked_sub(now_unix_secs).filter(|&s| s > 0)?;
        Some(Grant {
            channel,
            party,
            max_lifetime: Some(Duration::from_secs(remaining)),
        })
    }
    /// An authorizer using the system clock.
    pub fn into_authorizer(self) -> impl Fn(&[u8], SocketAddr) -> Option<Grant> + Send + Sync {
        move |credential, _| self.verify(credential, unix_now())
    }
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A fixed list of opaque tokens, each granting one party slot of one channel. Lookups
/// compare against every entry in constant time per entry.
#[derive(Default)]
pub struct StaticCredentials {
    entries: Vec<(Vec<u8>, Grant)>,
}
impl fmt::Debug for StaticCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "StaticCredentials({} entries)", self.entries.len())
    }
}
impl StaticCredentials {
    pub const MAX_ENTRIES: usize = 4096;
    pub fn new() -> Self {
        Self::default()
    }
    pub fn add(&mut self, token: &[u8], channel: ChannelId, party: Party) -> Result<()> {
        if token.len() < 16 || token.len() > wire::MAX_CREDENTIAL_BYTES {
            return Err(Error::Invalid(
                "static relay tokens must be 16..=1024 bytes",
            ));
        }
        if self.entries.len() >= Self::MAX_ENTRIES {
            return Err(Error::Invalid("too many static relay tokens"));
        }
        if self.entries.iter().any(|(t, _)| t == token) {
            return Err(Error::Invalid("duplicate static relay token"));
        }
        self.entries.push((
            token.to_vec(),
            Grant {
                channel,
                party,
                max_lifetime: None,
            },
        ));
        Ok(())
    }
    pub fn verify(&self, credential: &[u8]) -> Option<Grant> {
        let mut found = None;
        for (token, grant) in &self.entries {
            if wire::ct_eq(token, credential) {
                found = Some(*grant);
            }
        }
        found
    }
    pub fn into_authorizer(self) -> impl Fn(&[u8], SocketAddr) -> Option<Grant> + Send + Sync {
        move |credential, _| self.verify(credential)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_credentials_verify_expire_and_reject_tampering() {
        let issuer = HmacCredentials::new(&[7; 32]).unwrap();
        let channel = ChannelId([3; 16]);
        let token = issuer.mint(channel, Party::B, 1_000);
        let grant = issuer.verify(&token, 900).unwrap();
        assert_eq!((grant.channel, grant.party), (channel, Party::B));
        assert_eq!(grant.max_lifetime, Some(Duration::from_secs(100)));
        assert!(issuer.verify(&token, 1_000).is_none(), "expired");
        for i in 0..token.len() {
            let mut bad = token.clone();
            bad[i] ^= 0x10;
            assert!(issuer.verify(&bad, 900).is_none(), "byte {i}");
        }
        let other = HmacCredentials::new(&[8; 32]).unwrap();
        assert!(other.verify(&token, 900).is_none());
        assert!(HmacCredentials::new(&[0; 16]).is_err());
    }

    #[test]
    fn static_credentials_match_exactly() {
        let mut list = StaticCredentials::new();
        list.add(b"0123456789abcdef", ChannelId([1; 16]), Party::A)
            .unwrap();
        assert!(list.add(b"short", ChannelId([1; 16]), Party::A).is_err());
        assert!(list.verify(b"0123456789abcdef").is_some());
        assert!(list.verify(b"0123456789abcdeF").is_none());
        assert!(list.verify(b"0123456789abcdef0").is_none());
    }
}
