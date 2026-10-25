//! Connectivity candidates and their bounded out-of-band encoding.
use crate::{Error, Result, wire};
use serde::Serialize;
use std::net::SocketAddr;

/// How a candidate address was learned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateKind {
    /// The socket's own address (reachable on the same LAN or without NAT).
    Host,
    /// The NAT mapping a discovery server observed.
    ServerReflexive,
    /// A source address first seen on an authenticated probe (learned while punching).
    PeerReflexive,
    /// A relay server where this side holds (or will request) an allocation. Never probed;
    /// it tells the peer which relay to use when punching fails.
    Relay,
}
impl CandidateKind {
    /// Pair preference when several paths work (higher wins), as in ICE.
    pub fn preference(self) -> u8 {
        match self {
            Self::Host => 126,
            Self::PeerReflexive => 110,
            Self::ServerReflexive => 100,
            Self::Relay => 0,
        }
    }
    fn code(self) -> u8 {
        match self {
            Self::Host => 1,
            Self::ServerReflexive => 2,
            Self::PeerReflexive => 3,
            Self::Relay => 4,
        }
    }
    fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => Self::Host,
            2 => Self::ServerReflexive,
            3 => Self::PeerReflexive,
            4 => Self::Relay,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct Candidate {
    pub kind: CandidateKind,
    pub addr: SocketAddr,
}

/// A bounded, validated list of one side's candidates, exchanged out of band (for example
/// carried by the lobby as an opaque blob). It contains no secrets: the punch secret travels
/// separately over the authenticated lobby channel. Duplicates are rejected.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CandidateSet {
    candidates: Vec<Candidate>,
}

const FORMAT_VERSION: u8 = 1;
const ENTRY_LEN: usize = 1 + wire::ADDR_LEN;

impl CandidateSet {
    /// Most candidates one set carries.
    pub const MAX_CANDIDATES: usize = 8;
    /// Largest encoded size in bytes.
    pub const MAX_ENCODED_BYTES: usize = 2 + Self::MAX_CANDIDATES * ENTRY_LEN;

    pub fn new() -> Self {
        Self::default()
    }
    /// Host candidate from the socket address (skipped when unspecified, for example a
    /// `0.0.0.0` bind), server-reflexive candidates when they differ from it, and relays.
    pub fn gather(
        local: SocketAddr,
        reflexive: impl IntoIterator<Item = SocketAddr>,
        relays: impl IntoIterator<Item = SocketAddr>,
    ) -> Result<Self> {
        let mut set = Self::new();
        if wire::valid_endpoint(&local) {
            set.push(Candidate {
                kind: CandidateKind::Host,
                addr: local,
            })?;
        }
        for addr in reflexive {
            if !set.contains(addr) {
                set.push(Candidate {
                    kind: CandidateKind::ServerReflexive,
                    addr,
                })?;
            }
        }
        for addr in relays {
            set.push(Candidate {
                kind: CandidateKind::Relay,
                addr,
            })?;
        }
        Ok(set)
    }
    pub fn push(&mut self, candidate: Candidate) -> Result<()> {
        if !wire::valid_endpoint(&candidate.addr) {
            return Err(Error::Invalid(
                "candidate address is not a unicast endpoint",
            ));
        }
        if self.candidates.len() >= Self::MAX_CANDIDATES {
            return Err(Error::Invalid("too many candidates"));
        }
        if self.candidates.iter().any(|c| {
            c.addr == candidate.addr
                && (c.kind == CandidateKind::Relay) == (candidate.kind == CandidateKind::Relay)
        }) {
            return Err(Error::Invalid("duplicate candidate"));
        }
        self.candidates.push(candidate);
        Ok(())
    }
    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }
    fn contains(&self, addr: SocketAddr) -> bool {
        self.candidates.iter().any(|c| c.addr == addr)
    }
    /// Relay servers the peer advertised, in order.
    pub fn relays(&self) -> impl Iterator<Item = SocketAddr> + '_ {
        self.candidates
            .iter()
            .filter(|c| c.kind == CandidateKind::Relay)
            .map(|c| c.addr)
    }
    /// `[version 1][count]` then per candidate `[kind][19-byte address]`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(2 + self.candidates.len() * ENTRY_LEN);
        out.push(FORMAT_VERSION);
        out.push(self.candidates.len() as u8);
        for candidate in &self.candidates {
            out.push(candidate.kind.code());
            wire::put_addr(&mut out, candidate.addr, None);
        }
        out
    }
    /// Strict decoder for untrusted peer input: exact length, known kinds, valid unicast
    /// addresses, no duplicates, at most [`Self::MAX_CANDIDATES`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let invalid = Error::Invalid("malformed candidate set");
        let [version, count, entries @ ..] = bytes else {
            return Err(invalid);
        };
        let count = usize::from(*count);
        if *version != FORMAT_VERSION
            || count > Self::MAX_CANDIDATES
            || entries.len() != count * ENTRY_LEN
        {
            return Err(invalid);
        }
        let mut set = Self::new();
        let (entries, _) = entries.as_chunks::<ENTRY_LEN>();
        for entry in entries {
            let kind = CandidateKind::from_code(entry[0])
                .ok_or(Error::Invalid("unknown candidate kind"))?;
            let addr = wire::read_addr(&entry[1..], None)
                .ok_or(Error::Invalid("invalid candidate address"))?;
            set.push(Candidate { kind, addr })?;
        }
        Ok(set)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gather_round_trips_and_rejects_malformed_sets() {
        let set = CandidateSet::gather(
            "127.0.0.1:5000".parse().unwrap(),
            [
                "203.0.113.1:6000".parse().unwrap(),
                "127.0.0.1:5000".parse().unwrap(),
            ],
            ["198.51.100.2:3478".parse().unwrap()],
        )
        .unwrap();
        assert_eq!(
            set.candidates().len(),
            3,
            "reflexive duplicate of host skipped"
        );
        let bytes = set.to_bytes();
        assert!(bytes.len() <= CandidateSet::MAX_ENCODED_BYTES);
        assert_eq!(CandidateSet::from_bytes(&bytes).unwrap(), set);
        assert_eq!(set.relays().count(), 1);
        let unspecified = CandidateSet::gather("0.0.0.0:5000".parse().unwrap(), [], []).unwrap();
        assert!(unspecified.candidates().is_empty());

        let mut wrong_count = bytes.clone();
        wrong_count[1] += 1;
        let mut bad_kind = bytes.clone();
        bad_kind[2] = 9;
        let mut duplicate = vec![1, 2];
        duplicate.extend_from_slice(&bytes[2..2 + ENTRY_LEN]);
        duplicate.extend_from_slice(&bytes[2..2 + ENTRY_LEN]);
        for bad in [
            &[][..],
            &[2, 0],
            &wrong_count,
            &bad_kind,
            &duplicate,
            &bytes[..bytes.len() - 1],
        ] {
            assert!(CandidateSet::from_bytes(bad).is_err());
        }
        let mut full = CandidateSet::new();
        for port in 1..=8 {
            full.push(Candidate {
                kind: CandidateKind::Host,
                addr: SocketAddr::from(([127, 0, 0, 1], port)),
            })
            .unwrap();
        }
        assert!(
            full.push(Candidate {
                kind: CandidateKind::Host,
                addr: SocketAddr::from(([127, 0, 0, 1], 9)),
            })
            .is_err()
        );
    }
}
