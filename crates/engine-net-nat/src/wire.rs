//! Byte layouts of every NAT/relay control datagram. Decoders are strict: exact lengths,
//! canonical zero padding and validated addresses, so each message has one encoding.
use crate::{Error, Result};
use ring::rand::{SecureRandom, SystemRandom};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// Bytes 1..5 of every control datagram.
pub const MAGIC: [u8; 4] = *b"NAT1";
pub(crate) const HEADER_LEN: usize = 1 + MAGIC.len();
pub(crate) const TXID_LEN: usize = 12;
pub(crate) const ADDR_LEN: usize = 19;
pub(crate) const COOKIE_LEN: usize = 16;
pub(crate) const KEY_LEN: usize = 16;
pub(crate) type TxId = [u8; TXID_LEN];

/// First byte of each control datagram. Every value keeps the QUIC long-header (0x80) and
/// fixed (0x40) bits clear.
pub(crate) mod kind {
    pub const BINDING_REQUEST: u8 = 0x01;
    pub const BINDING_RESPONSE: u8 = 0x02;
    pub const PUNCH_PROBE: u8 = 0x10;
    pub const PUNCH_ACK: u8 = 0x11;
    pub const ALLOCATE: u8 = 0x20;
    pub const ALLOCATE_CHALLENGE: u8 = 0x21;
    pub const ALLOCATE_OK: u8 = 0x22;
    pub const REFRESH: u8 = 0x23;
    pub const REFRESH_OK: u8 = 0x24;
    pub const RELAY_ERROR: u8 = 0x25;
    pub const DATA: u8 = 0x30;
}

/// True when `datagram` belongs to the NAT/relay layer rather than QUIC: a first byte in
/// `0x01..=0x3F` (QUIC long-header and fixed bits clear) followed by [`MAGIC`].
pub fn is_control_datagram(datagram: &[u8]) -> bool {
    datagram.len() >= HEADER_LEN
        && datagram[0] & 0xC0 == 0
        && datagram[0] != 0
        && datagram[1..HEADER_LEN] == MAGIC
}

pub(crate) fn control_kind(datagram: &[u8]) -> Option<u8> {
    is_control_datagram(datagram).then(|| datagram[0])
}

pub(crate) fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| Error::Random)?;
    Ok(bytes)
}

/// Equality without early exit for secrets of public length.
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0_u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A usable unicast endpoint: nonzero port, not unspecified, multicast or broadcast.
pub(crate) fn valid_endpoint(addr: &SocketAddr) -> bool {
    let ip_ok = match addr.ip() {
        IpAddr::V4(v4) => !v4.is_unspecified() && !v4.is_multicast() && !v4.is_broadcast(),
        IpAddr::V6(v6) => !v6.is_unspecified() && !v6.is_multicast(),
    };
    addr.port() != 0 && ip_ok
}

pub(crate) fn header(out: &mut Vec<u8>, kind: u8) {
    out.clear();
    out.push(kind);
    out.extend_from_slice(&MAGIC);
}

/// Cursor over a datagram whose header was already checked.
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    /// Starts after the header when `datagram` is a control datagram of `expected` kind.
    pub(crate) fn new(datagram: &'a [u8], expected: u8) -> Option<Self> {
        (control_kind(datagram)? == expected).then_some(Self {
            data: datagram,
            pos: HEADER_LEN,
        })
    }
    pub(crate) fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(len)?;
        let bytes = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(bytes)
    }
    pub(crate) fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }
    pub(crate) fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    pub(crate) fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.array()?))
    }
    pub(crate) fn rest(&mut self) -> &'a [u8] {
        let rest = &self.data[self.pos..];
        self.pos = self.data.len();
        rest
    }
    /// True when only zero bytes remain (canonical padding).
    pub(crate) fn zero_rest(&mut self) -> bool {
        self.rest().iter().all(|&b| b == 0)
    }
}

/// STUN-style XOR mask over port (2 bytes) and IP (16 bytes) so address-rewriting middleboxes
/// do not alter observed addresses carried in payloads.
fn mask(txid: Option<&TxId>) -> [u8; ADDR_LEN - 1] {
    let mut mask = [0; ADDR_LEN - 1];
    if let Some(txid) = txid {
        mask[..2].copy_from_slice(&MAGIC[..2]);
        mask[2..6].copy_from_slice(&MAGIC);
        mask[6..].copy_from_slice(txid);
    }
    mask
}

/// Fixed 19-byte address: family (4 or 6), big-endian port, 16 IP bytes (IPv4 in the first
/// four, the rest zero), port and IP XOR-masked when `txid` is given.
pub(crate) fn put_addr(out: &mut Vec<u8>, addr: SocketAddr, txid: Option<&TxId>) {
    let mut plain = [0; ADDR_LEN - 1];
    plain[..2].copy_from_slice(&addr.port().to_be_bytes());
    let family = match addr.ip() {
        IpAddr::V4(v4) => {
            plain[2..6].copy_from_slice(&v4.octets());
            4
        }
        IpAddr::V6(v6) => {
            plain[2..].copy_from_slice(&v6.octets());
            6
        }
    };
    out.push(family);
    let mask = mask(txid);
    out.extend(plain.iter().zip(mask).map(|(p, m)| p ^ m));
}

pub(crate) fn read_addr(bytes: &[u8], txid: Option<&TxId>) -> Option<SocketAddr> {
    let (&family, masked) = bytes.split_first()?;
    if masked.len() != ADDR_LEN - 1 {
        return None;
    }
    let mut plain = [0; ADDR_LEN - 1];
    for ((p, m), k) in plain.iter_mut().zip(masked).zip(mask(txid)) {
        *p = m ^ k;
    }
    let port = u16::from_be_bytes([plain[0], plain[1]]);
    let ip = match family {
        4 if plain[6..].iter().all(|&b| b == 0) => {
            IpAddr::V4(Ipv4Addr::new(plain[2], plain[3], plain[4], plain[5]))
        }
        6 => IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(&plain[2..]).ok()?)),
        _ => return None,
    };
    let addr = SocketAddr::new(ip, port);
    valid_endpoint(&addr).then_some(addr)
}

// --- Reflexive discovery -------------------------------------------------------------------

/// Requests are padded so a response is never larger than the request it answers.
pub(crate) const BINDING_REQUEST_LEN: usize = 40;
pub(crate) const BINDING_RESPONSE_LEN: usize = HEADER_LEN + TXID_LEN + ADDR_LEN;

pub(crate) fn encode_binding_request(out: &mut Vec<u8>, txid: &TxId) {
    header(out, kind::BINDING_REQUEST);
    out.extend_from_slice(txid);
    out.resize(BINDING_REQUEST_LEN, 0);
}
pub(crate) fn decode_binding_request(datagram: &[u8]) -> Option<TxId> {
    if datagram.len() != BINDING_REQUEST_LEN {
        return None;
    }
    let mut r = Reader::new(datagram, kind::BINDING_REQUEST)?;
    let txid = r.array()?;
    r.zero_rest().then_some(txid)
}
pub(crate) fn encode_binding_response(out: &mut Vec<u8>, txid: &TxId, observed: SocketAddr) {
    header(out, kind::BINDING_RESPONSE);
    out.extend_from_slice(txid);
    put_addr(out, observed, Some(txid));
}
pub(crate) fn decode_binding_response(datagram: &[u8]) -> Option<(TxId, SocketAddr)> {
    if datagram.len() != BINDING_RESPONSE_LEN {
        return None;
    }
    let mut r = Reader::new(datagram, kind::BINDING_RESPONSE)?;
    let txid: TxId = r.array()?;
    let addr = read_addr(r.take(ADDR_LEN)?, Some(&txid))?;
    Some((txid, addr))
}

// --- Relay control -------------------------------------------------------------------------

/// Largest credential the wire format carries; [`crate::RelayLimits`] may lower it.
pub const MAX_CREDENTIAL_BYTES: usize = 1024;
/// Allocate requests are padded to this length so every response fits inside them.
pub(crate) const ALLOCATE_MIN_LEN: usize = 96;
const ALLOCATE_FIXED_LEN: usize = HEADER_LEN + TXID_LEN + COOKIE_LEN + 2;
pub(crate) const REFRESH_LEN: usize = 64;
/// Header plus source and destination virtual ports.
pub const DATA_HEADER_BYTES: usize = HEADER_LEN + 4;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Allocate<'a> {
    pub txid: TxId,
    pub cookie: [u8; COOKIE_LEN],
    pub credential: &'a [u8],
}
pub(crate) fn encode_allocate(out: &mut Vec<u8>, request: &Allocate<'_>) -> Result<()> {
    if request.credential.is_empty() || request.credential.len() > MAX_CREDENTIAL_BYTES {
        return Err(Error::Invalid("relay credential must be 1..=1024 bytes"));
    }
    header(out, kind::ALLOCATE);
    out.extend_from_slice(&request.txid);
    out.extend_from_slice(&request.cookie);
    out.extend_from_slice(&(request.credential.len() as u16).to_be_bytes());
    out.extend_from_slice(request.credential);
    if out.len() < ALLOCATE_MIN_LEN {
        out.resize(ALLOCATE_MIN_LEN, 0);
    }
    Ok(())
}
pub(crate) fn decode_allocate(datagram: &[u8]) -> Option<Allocate<'_>> {
    let mut r = Reader::new(datagram, kind::ALLOCATE)?;
    let txid = r.array()?;
    let cookie = r.array()?;
    let len = usize::from(r.u16()?);
    if len == 0 || len > MAX_CREDENTIAL_BYTES {
        return None;
    }
    let credential = r.take(len)?;
    let canonical = (ALLOCATE_FIXED_LEN + len).max(ALLOCATE_MIN_LEN);
    (datagram.len() == canonical && r.zero_rest()).then_some(Allocate {
        txid,
        cookie,
        credential,
    })
}

pub(crate) const CHALLENGE_LEN: usize = HEADER_LEN + TXID_LEN + COOKIE_LEN;
pub(crate) fn encode_challenge(out: &mut Vec<u8>, txid: &TxId, cookie: &[u8; COOKIE_LEN]) {
    header(out, kind::ALLOCATE_CHALLENGE);
    out.extend_from_slice(txid);
    out.extend_from_slice(cookie);
}
pub(crate) fn decode_challenge(datagram: &[u8]) -> Option<(TxId, [u8; COOKIE_LEN])> {
    if datagram.len() != CHALLENGE_LEN {
        return None;
    }
    let mut r = Reader::new(datagram, kind::ALLOCATE_CHALLENGE)?;
    Some((r.array()?, r.array()?))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AllocateOk {
    pub txid: TxId,
    pub vport: u16,
    pub key: [u8; KEY_LEN],
    pub lifetime_secs: u16,
    /// Counterpart virtual port, 0 while the other party has not allocated.
    pub counterpart: u16,
    pub observed: SocketAddr,
}
pub(crate) const ALLOCATE_OK_LEN: usize = HEADER_LEN + TXID_LEN + 2 + KEY_LEN + 2 + 2 + ADDR_LEN;
pub(crate) fn encode_allocate_ok(out: &mut Vec<u8>, ok: &AllocateOk) {
    header(out, kind::ALLOCATE_OK);
    out.extend_from_slice(&ok.txid);
    out.extend_from_slice(&ok.vport.to_be_bytes());
    out.extend_from_slice(&ok.key);
    out.extend_from_slice(&ok.lifetime_secs.to_be_bytes());
    out.extend_from_slice(&ok.counterpart.to_be_bytes());
    put_addr(out, ok.observed, Some(&ok.txid));
}
pub(crate) fn decode_allocate_ok(datagram: &[u8]) -> Option<AllocateOk> {
    if datagram.len() != ALLOCATE_OK_LEN {
        return None;
    }
    let mut r = Reader::new(datagram, kind::ALLOCATE_OK)?;
    let txid: TxId = r.array()?;
    let ok = AllocateOk {
        txid,
        vport: r.u16()?,
        key: r.array()?,
        lifetime_secs: r.u16()?,
        counterpart: r.u16()?,
        observed: read_addr(r.take(ADDR_LEN)?, Some(&txid))?,
    };
    (ok.vport != 0 && ok.lifetime_secs != 0 && ok.counterpart != ok.vport).then_some(ok)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Refresh {
    pub txid: TxId,
    pub vport: u16,
    pub key: [u8; KEY_LEN],
    /// 0 releases the allocation.
    pub lifetime_secs: u16,
}
pub(crate) fn encode_refresh(out: &mut Vec<u8>, refresh: &Refresh) {
    header(out, kind::REFRESH);
    out.extend_from_slice(&refresh.txid);
    out.extend_from_slice(&refresh.vport.to_be_bytes());
    out.extend_from_slice(&refresh.key);
    out.extend_from_slice(&refresh.lifetime_secs.to_be_bytes());
    out.resize(REFRESH_LEN, 0);
}
pub(crate) fn decode_refresh(datagram: &[u8]) -> Option<Refresh> {
    if datagram.len() != REFRESH_LEN {
        return None;
    }
    let mut r = Reader::new(datagram, kind::REFRESH)?;
    let refresh = Refresh {
        txid: r.array()?,
        vport: r.u16()?,
        key: r.array()?,
        lifetime_secs: r.u16()?,
    };
    (refresh.vport != 0 && r.zero_rest()).then_some(refresh)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RefreshOk {
    pub txid: TxId,
    pub lifetime_secs: u16,
    pub counterpart: u16,
}
pub(crate) const REFRESH_OK_LEN: usize = HEADER_LEN + TXID_LEN + 4;
pub(crate) fn encode_refresh_ok(out: &mut Vec<u8>, ok: &RefreshOk) {
    header(out, kind::REFRESH_OK);
    out.extend_from_slice(&ok.txid);
    out.extend_from_slice(&ok.lifetime_secs.to_be_bytes());
    out.extend_from_slice(&ok.counterpart.to_be_bytes());
}
pub(crate) fn decode_refresh_ok(datagram: &[u8]) -> Option<RefreshOk> {
    if datagram.len() != REFRESH_OK_LEN {
        return None;
    }
    let mut r = Reader::new(datagram, kind::REFRESH_OK)?;
    Some(RefreshOk {
        txid: r.array()?,
        lifetime_secs: r.u16()?,
        counterpart: r.u16()?,
    })
}

pub(crate) const ERROR_LEN: usize = HEADER_LEN + TXID_LEN + 1;
pub(crate) fn encode_error(out: &mut Vec<u8>, txid: &TxId, code: u8) {
    header(out, kind::RELAY_ERROR);
    out.extend_from_slice(txid);
    out.push(code);
}
pub(crate) fn decode_error(datagram: &[u8]) -> Option<(TxId, u8)> {
    if datagram.len() != ERROR_LEN {
        return None;
    }
    let mut r = Reader::new(datagram, kind::RELAY_ERROR)?;
    Some((r.array()?, r.u8()?))
}

pub(crate) fn encode_data_header(out: &mut Vec<u8>, source: u16, destination: u16) {
    header(out, kind::DATA);
    out.extend_from_slice(&source.to_be_bytes());
    out.extend_from_slice(&destination.to_be_bytes());
}
/// Returns (source vport, destination vport, nonempty payload).
pub(crate) fn decode_data(datagram: &[u8]) -> Option<(u16, u16, &[u8])> {
    let mut r = Reader::new(datagram, kind::DATA)?;
    let source = r.u16()?;
    let destination = r.u16()?;
    let payload = r.rest();
    (source != 0 && destination != 0 && source != destination && !payload.is_empty()).then_some((
        source,
        destination,
        payload,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_keeps_quic_bits_clear() {
        for k in [
            kind::BINDING_REQUEST,
            kind::BINDING_RESPONSE,
            kind::PUNCH_PROBE,
            kind::PUNCH_ACK,
            kind::ALLOCATE,
            kind::ALLOCATE_CHALLENGE,
            kind::ALLOCATE_OK,
            kind::REFRESH,
            kind::REFRESH_OK,
            kind::RELAY_ERROR,
            kind::DATA,
        ] {
            assert_eq!(k & 0xC0, 0);
            assert_ne!(k, 0);
        }
    }

    #[test]
    fn addresses_round_trip_masked_and_plain() {
        let txid = [7; TXID_LEN];
        for addr in [
            SocketAddr::from(([127, 0, 0, 1], 4242)),
            SocketAddr::from(([203, 0, 113, 9], 1)),
            "[2001:db8::1]:65535".parse().unwrap(),
        ] {
            for mask in [None, Some(&txid)] {
                let mut out = Vec::new();
                put_addr(&mut out, addr, mask);
                assert_eq!(out.len(), ADDR_LEN);
                assert_eq!(read_addr(&out, mask), Some(addr));
            }
        }
        let mut out = Vec::new();
        put_addr(&mut out, SocketAddr::from(([0, 0, 0, 0], 9)), None);
        assert_eq!(read_addr(&out, None), None, "unspecified rejected");
    }

    #[test]
    fn responses_never_exceed_their_requests() {
        assert!(BINDING_RESPONSE_LEN <= BINDING_REQUEST_LEN);
        for response in [CHALLENGE_LEN, ALLOCATE_OK_LEN, ERROR_LEN] {
            assert!(response <= ALLOCATE_MIN_LEN);
        }
        for response in [REFRESH_OK_LEN, ERROR_LEN] {
            assert!(response <= REFRESH_LEN);
        }
    }

    #[test]
    fn allocate_encoding_is_canonical() {
        let request = Allocate {
            txid: [1; TXID_LEN],
            cookie: [2; COOKIE_LEN],
            credential: b"token",
        };
        let mut out = Vec::new();
        encode_allocate(&mut out, &request).unwrap();
        assert_eq!(out.len(), ALLOCATE_MIN_LEN);
        assert_eq!(decode_allocate(&out), Some(request));
        out.push(0);
        assert_eq!(decode_allocate(&out), None, "extra padding rejected");
        let long = vec![3; 200];
        encode_allocate(
            &mut out,
            &Allocate {
                txid: [1; TXID_LEN],
                cookie: [0; COOKIE_LEN],
                credential: &long,
            },
        )
        .unwrap();
        assert_eq!(out.len(), ALLOCATE_FIXED_LEN + 200);
        assert_eq!(decode_allocate(&out).unwrap().credential, &long[..]);
    }
}
