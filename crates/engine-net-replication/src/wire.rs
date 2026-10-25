//! Hand-written bounded reader/writer primitives (little-endian, LEB128 varints).
use crate::error::{ReplicationError, Result};

pub(crate) fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

pub(crate) fn varint_len(v: u64) -> usize {
    let bits = 64 - (v | 1).leading_zeros() as usize;
    bits.div_ceil(7)
}

pub(crate) fn zigzag(v: i32) -> u32 {
    ((v << 1) ^ (v >> 31)) as u32
}

pub(crate) fn unzigzag(v: u32) -> i32 {
    ((v >> 1) as i32) ^ -((v & 1) as i32)
}

pub(crate) fn zigzag64(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

pub(crate) fn unzigzag64(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

/// Cursor over untrusted bytes. Every read checks the remaining length first.
pub(crate) struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    pub(crate) fn remaining(&self) -> usize {
        self.buf.len()
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.buf.len() < n {
            return Err(ReplicationError::Malformed("truncated packet"));
        }
        let (head, tail) = self.buf.split_at(n);
        self.buf = tail;
        Ok(head)
    }

    pub(crate) fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub(crate) fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub(crate) fn u64(&mut self) -> Result<u64> {
        let mut a = [0; 8];
        a.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(a))
    }

    pub(crate) fn varint(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for i in 0..10 {
            let b = self.u8()?;
            if i == 9 && b > 1 {
                return Err(ReplicationError::Malformed("varint overflow"));
            }
            v |= u64::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(ReplicationError::Malformed("varint too long"))
    }

    pub(crate) fn varint_u32(&mut self) -> Result<u32> {
        u32::try_from(self.varint()?).map_err(|_| ReplicationError::Malformed("varint above u32"))
    }

    pub(crate) fn end(&self) -> Result<()> {
        if self.buf.is_empty() {
            Ok(())
        } else {
            Err(ReplicationError::Malformed("trailing bytes"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_round_trips_and_lengths_match() {
        for v in [0u64, 1, 127, 128, 16_383, 16_384, u32::MAX as u64, u64::MAX] {
            let mut out = Vec::new();
            put_varint(&mut out, v);
            assert_eq!(out.len(), varint_len(v));
            let mut r = Reader::new(&out);
            assert_eq!(r.varint().unwrap(), v);
            assert!(r.end().is_ok());
        }
    }

    #[test]
    fn overlong_varint_is_rejected() {
        let bytes = [0xffu8; 11];
        assert!(Reader::new(&bytes).varint().is_err());
        let mut ten = vec![0xffu8; 9];
        ten.push(2);
        assert!(Reader::new(&ten).varint().is_err());
    }

    #[test]
    fn zigzag_round_trips() {
        for v in [0i32, 1, -1, i32::MAX, i32::MIN, 12345, -12345] {
            assert_eq!(unzigzag(zigzag(v)), v);
        }
        for v in [0i64, 1, -1, i64::MAX, i64::MIN] {
            assert_eq!(unzigzag64(zigzag64(v)), v);
        }
    }
}
