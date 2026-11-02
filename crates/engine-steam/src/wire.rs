//! Wire format of one Steam message: `[header][body]`.
//!
//! Data header: bits 0-3 lane, bits 4-5 delivery, bits 6-7 zero. `UnreliableSequenced`
//! data carries a 4-byte little-endian per-lane sequence number before the payload.
//! Control headers: `0x80` Hello (client to server) and `0x81` Accept (server to client).
//! Every byte received is untrusted; decoding never panics and never allocates.

use engine_network::{Delivery, Lane};

use crate::types::SteamId;

/// Protocol version carried by Hello and Accept.
pub(crate) const VERSION: u8 = 1;
/// Auth session tickets are at most 1024 bytes.
pub(crate) const MAX_TICKET: usize = 1024;
/// Bytes a data frame adds to the payload.
pub(crate) const DATA_OVERHEAD: usize = 1;
/// Extra bytes a sequenced frame adds.
pub(crate) const SEQUENCE_BYTES: usize = 4;

const HELLO: u8 = 0x80;
const ACCEPT: u8 = 0x81;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Frame<'a> {
    Data {
        lane: Lane,
        delivery: Delivery,
        sequence: Option<u32>,
        payload: &'a [u8],
    },
    Hello {
        user: SteamId,
        ticket: &'a [u8],
    },
    Accept {
        server: SteamId,
    },
}

/// Bytes added in front of the payload for `delivery`.
pub(crate) fn overhead(delivery: Delivery) -> usize {
    DATA_OVERHEAD
        + if delivery == Delivery::UnreliableSequenced {
            SEQUENCE_BYTES
        } else {
            0
        }
}

fn delivery_bits(delivery: Delivery) -> u8 {
    match delivery {
        Delivery::ReliableOrdered => 0,
        Delivery::ReliableUnordered => 1,
        Delivery::UnreliableSequenced => 2,
        Delivery::Unreliable => 3,
    }
}

pub(crate) fn encode_data(
    lane: Lane,
    delivery: Delivery,
    sequence: u32,
    payload: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(overhead(delivery) + payload.len());
    out.push(lane.0 | (delivery_bits(delivery) << 4));
    if delivery == Delivery::UnreliableSequenced {
        out.extend_from_slice(&sequence.to_le_bytes());
    }
    out.extend_from_slice(payload);
    out
}

pub(crate) fn encode_hello(user: SteamId, ticket: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(10 + ticket.len());
    out.push(HELLO);
    out.push(VERSION);
    out.extend_from_slice(&user.0.to_le_bytes());
    out.extend_from_slice(ticket);
    out
}

pub(crate) fn encode_accept(server: SteamId) -> Vec<u8> {
    let mut out = Vec::with_capacity(10);
    out.push(ACCEPT);
    out.push(VERSION);
    out.extend_from_slice(&server.0.to_le_bytes());
    out
}

fn steam_id(bytes: &[u8]) -> Option<SteamId> {
    let raw: [u8; 8] = bytes.get(..8)?.try_into().ok()?;
    Some(SteamId(u64::from_le_bytes(raw)))
}

pub(crate) fn decode(frame: &[u8]) -> Result<Frame<'_>, &'static str> {
    let (&header, body) = frame.split_first().ok_or("empty frame")?;
    if header & 0x80 != 0 {
        let (&version, body) = body.split_first().ok_or("truncated control frame")?;
        if version != VERSION {
            return Err("unsupported protocol version");
        }
        let user = steam_id(body).ok_or("truncated control frame")?;
        let rest = &body[8..];
        return match header {
            HELLO if rest.len() <= MAX_TICKET => Ok(Frame::Hello { user, ticket: rest }),
            HELLO => Err("auth ticket too large"),
            ACCEPT if rest.is_empty() => Ok(Frame::Accept { server: user }),
            ACCEPT => Err("trailing bytes in accept frame"),
            _ => Err("unknown control frame"),
        };
    }
    if header & 0x40 != 0 {
        return Err("reserved header bits set");
    }
    let lane = Lane(header & 0x0f);
    let delivery = match (header >> 4) & 3 {
        0 => Delivery::ReliableOrdered,
        1 => Delivery::ReliableUnordered,
        2 => Delivery::UnreliableSequenced,
        _ => Delivery::Unreliable,
    };
    if delivery == Delivery::UnreliableSequenced {
        let raw: [u8; 4] = body
            .get(..SEQUENCE_BYTES)
            .and_then(|b| b.try_into().ok())
            .ok_or("truncated sequence number")?;
        return Ok(Frame::Data {
            lane,
            delivery,
            sequence: Some(u32::from_le_bytes(raw)),
            payload: &body[SEQUENCE_BYTES..],
        });
    }
    Ok(Frame::Data {
        lane,
        delivery,
        sequence: None,
        payload: body,
    })
}

/// Wrapping "newer than": true when `a` is ahead of `b` by less than half the range.
pub(crate) fn newer(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_round_trips_for_every_delivery() {
        for delivery in [
            Delivery::ReliableOrdered,
            Delivery::ReliableUnordered,
            Delivery::UnreliableSequenced,
            Delivery::Unreliable,
        ] {
            for lane in [0u8, 7, 15] {
                let bytes = encode_data(Lane(lane), delivery, 77, b"abc");
                let seq = (delivery == Delivery::UnreliableSequenced).then_some(77);
                assert_eq!(
                    decode(&bytes),
                    Ok(Frame::Data {
                        lane: Lane(lane),
                        delivery,
                        sequence: seq,
                        payload: b"abc"
                    })
                );
            }
        }
    }

    #[test]
    fn control_frames_round_trip() {
        let hello = encode_hello(SteamId(42), &[1, 2, 3]);
        assert_eq!(
            decode(&hello),
            Ok(Frame::Hello {
                user: SteamId(42),
                ticket: &[1, 2, 3]
            })
        );
        assert_eq!(
            decode(&encode_accept(SteamId(9))),
            Ok(Frame::Accept { server: SteamId(9) })
        );
    }

    #[test]
    fn malformed_frames_are_errors_not_panics() {
        assert!(decode(&[]).is_err());
        assert!(decode(&[0x40]).is_err());
        assert!(decode(&[0x20, 1, 2]).is_err(), "sequenced needs 4 bytes");
        assert!(decode(&[0x80]).is_err());
        assert!(
            decode(&[0x80, 9, 0, 0, 0, 0, 0, 0, 0, 0]).is_err(),
            "bad version"
        );
        assert!(decode(&[0x80, VERSION, 1, 2]).is_err());
        assert!(decode(&[0x82, VERSION, 0, 0, 0, 0, 0, 0, 0, 0]).is_err());
        assert!(decode(&[0x81, VERSION, 0, 0, 0, 0, 0, 0, 0, 0, 1]).is_err());
        let big = encode_hello(SteamId(1), &vec![0; MAX_TICKET + 1]);
        assert!(decode(&big).is_err());
        let ok = encode_hello(SteamId(1), &vec![0; MAX_TICKET]);
        assert!(decode(&ok).is_ok());
    }

    #[test]
    fn sequence_comparison_wraps() {
        assert!(newer(2, 1));
        assert!(!newer(1, 1));
        assert!(!newer(1, 2));
        assert!(newer(0, u32::MAX));
        assert!(!newer(u32::MAX, 0));
    }
}
