//! The versioned binary foliage asset file (`.foliage`): the type definitions and the
//! placed instances.
//!
//! Layout, all integers and floats little-endian:
//!
//! | Offset | Size | Field |
//! | ------ | ---- | ----- |
//! | 0 | 4 | magic `EFOL` |
//! | 4 | 4 | `u32` version, currently 1 |
//! | 8 | 4 | `u32` header length `n` (1..=262144) |
//! | 12 | n | UTF-8 JSON [`AssetHeader`] |
//! | | `48 * instances` | per instance: `u32` type index, 3 x `f32` position, 4 x `f32` rotation (x y z w), 3 x `f32` scale, `f32` phase |
//! | end - 8 | 8 | `u64` FNV-1a 64 checksum of every preceding byte |
//!
//! Decoding rejects a wrong magic or version, a header that is not valid JSON or has unknown
//! fields, invalid types, an instance count above [`MAX_INSTANCES`], a length that does not
//! match the header, invalid instance values, a type index outside the type list and a
//! checksum mismatch. Instances are written in cell order, so encoding is deterministic.
use crate::field::{FoliageField, Instance, MAX_INSTANCES};
use crate::types::{FoliageType, validate_types};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

pub const MAGIC: [u8; 4] = *b"EFOL";
pub const VERSION: u32 = 1;
pub const MAX_HEADER_BYTES: usize = 256 * 1024;
const INSTANCE_BYTES: usize = 48;
/// Largest accepted file.
pub const MAX_FILE_BYTES: usize = 12 + MAX_HEADER_BYTES + MAX_INSTANCES * INSTANCE_BYTES + 8;

/// The JSON header of a foliage file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetHeader {
    /// Storage cell edge of the field.
    pub cell_size: f32,
    pub types: Vec<FoliageType>,
    /// Number of instances that follow.
    pub instances: u32,
}

/// A decoded foliage file.
#[derive(Clone, Debug, PartialEq)]
pub struct FoliageAsset {
    pub types: Vec<FoliageType>,
    pub field: FoliageField,
}

fn malformed(message: impl Into<String>) -> Error {
    Error::new("foliage.asset.malformed", message)
}

/// FNV-1a 64-bit hash.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Serializes the types and instances of `field`.
pub fn encode(types: &[FoliageType], field: &FoliageField) -> Result<Vec<u8>> {
    validate_types(types)?;
    let header = AssetHeader {
        cell_size: field.cell_size(),
        types: types.to_vec(),
        instances: field.len() as u32,
    };
    let json = serde_json::to_vec(&header).map_err(|e| malformed(e.to_string()))?;
    if json.len() > MAX_HEADER_BYTES {
        return Err(Error::new(
            "foliage.asset.size",
            "the type definitions exceed the 256 KiB header limit",
        ));
    }
    let mut out = Vec::with_capacity(20 + json.len() + field.len() * INSTANCE_BYTES);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(&json);
    for i in field.iter() {
        if usize::from(i.type_index) >= types.len() {
            return Err(Error::new(
                "foliage.asset.type_index",
                format!("instance type index {} has no type", i.type_index),
            ));
        }
        out.extend_from_slice(&u32::from(i.type_index).to_le_bytes());
        for v in i
            .position
            .iter()
            .chain(&i.rotation)
            .chain(&i.scale)
            .chain([&i.phase])
        {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    let checksum = fnv1a64(&out);
    out.extend_from_slice(&checksum.to_le_bytes());
    Ok(out)
}

/// Decodes and validates a `.foliage` file.
pub fn decode(bytes: &[u8]) -> Result<FoliageAsset> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(Error::new("foliage.asset.size", "foliage file too large"));
    }
    if bytes.len() < 20 || bytes[..4] != MAGIC {
        return Err(malformed("not a foliage file (bad magic or too short)"));
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().expect("4 bytes"));
    if version != VERSION {
        return Err(Error::new(
            "foliage.asset.version",
            format!("unsupported foliage file version {version}"),
        ));
    }
    let header_len = u32::from_le_bytes(bytes[8..12].try_into().expect("4 bytes")) as usize;
    if header_len == 0 || header_len > MAX_HEADER_BYTES || 12 + header_len + 8 > bytes.len() {
        return Err(malformed("bad header length"));
    }
    let (body, tail) = bytes.split_at(bytes.len() - 8);
    let stored = u64::from_le_bytes(tail.try_into().expect("8 bytes"));
    if fnv1a64(body) != stored {
        return Err(Error::new("foliage.asset.checksum", "checksum mismatch"));
    }
    let header: AssetHeader = serde_json::from_slice(&bytes[12..12 + header_len])
        .map_err(|e| malformed(format!("header: {e}")))?;
    validate_types(&header.types)?;
    let count = header.instances as usize;
    if count > MAX_INSTANCES {
        return Err(Error::new(
            "foliage.limit.instances",
            format!("{count} instances exceed {MAX_INSTANCES}"),
        ));
    }
    let data = &body[12 + header_len..];
    if data.len() != count * INSTANCE_BYTES {
        return Err(malformed("instance data length does not match the header"));
    }
    let mut field = FoliageField::new(header.cell_size)?;
    for chunk in data.as_chunks::<INSTANCE_BYTES>().0 {
        let type_index = u32::from_le_bytes(chunk[..4].try_into().expect("4 bytes"));
        if type_index as usize >= header.types.len() {
            return Err(Error::new(
                "foliage.asset.type_index",
                format!("instance type index {type_index} has no type"),
            ));
        }
        let f =
            |n: usize| f32::from_le_bytes(chunk[4 + 4 * n..8 + 4 * n].try_into().expect("4 bytes"));
        field.insert(Instance {
            type_index: type_index as u16,
            position: [f(0), f(1), f(2)],
            rotation: [f(3), f(4), f(5), f(6)],
            scale: [f(7), f(8), f(9)],
            phase: f(10),
        })?;
    }
    Ok(FoliageAsset {
        types: header.types,
        field,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (Vec<FoliageType>, FoliageField) {
        let types = vec![FoliageType::new("a", "m1"), FoliageType::new("b", "m2")];
        let mut field = FoliageField::new(16.0).unwrap();
        for k in 0..50 {
            let mut i = Instance::new((k % 2) as u16, [k as f32 * 3.0, 1.0, -(k as f32)]);
            i.phase = k as f32 / 50.0;
            i.scale = [1.0 + k as f32 * 0.01; 3];
            field.insert(i).unwrap();
        }
        (types, field)
    }

    #[test]
    fn round_trip_is_exact_and_deterministic() {
        let (types, field) = sample();
        let bytes = encode(&types, &field).unwrap();
        assert_eq!(bytes, encode(&types, &field).unwrap());
        assert_eq!(&bytes[..4], b"EFOL");
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.types, types);
        assert_eq!(
            decoded.field.iter().copied().collect::<Vec<_>>(),
            field.iter().copied().collect::<Vec<_>>()
        );
        assert_eq!(decoded.field.cell_size(), 16.0);
    }

    #[test]
    fn corruption_is_rejected_with_stable_codes() {
        let (types, field) = sample();
        let bytes = encode(&types, &field).unwrap();
        let mut flipped = bytes.clone();
        let mid = flipped.len() / 2;
        flipped[mid] ^= 1;
        assert_eq!(decode(&flipped).unwrap_err().code, "foliage.asset.checksum");
        let mut magic = bytes.clone();
        magic[0] = b'X';
        assert_eq!(decode(&magic).unwrap_err().code, "foliage.asset.malformed");
        let mut version = bytes.clone();
        version[4] = 9;
        assert_eq!(decode(&version).unwrap_err().code, "foliage.asset.version");
        assert!(decode(&bytes[..bytes.len() - 9]).is_err());
        assert!(decode(&[]).is_err());
    }

    #[test]
    fn invalid_instances_and_type_indices_are_rejected_even_with_a_good_checksum() {
        let (types, field) = sample();
        let mut bytes = encode(&types, &field).unwrap();
        let header_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let first = 12 + header_len;
        // Type index 7 has no type.
        bytes[first..first + 4].copy_from_slice(&7u32.to_le_bytes());
        let end = bytes.len() - 8;
        let sum = fnv1a64(&bytes[..end]);
        bytes[end..].copy_from_slice(&sum.to_le_bytes());
        assert_eq!(decode(&bytes).unwrap_err().code, "foliage.asset.type_index");
        // A NaN position.
        let (types, field) = sample();
        let mut bytes = encode(&types, &field).unwrap();
        bytes[first + 4..first + 8].copy_from_slice(&f32::NAN.to_le_bytes());
        let sum = fnv1a64(&bytes[..end]);
        bytes[end..].copy_from_slice(&sum.to_le_bytes());
        assert_eq!(decode(&bytes).unwrap_err().code, "foliage.instance.invalid");
    }

    #[test]
    fn instances_referring_to_missing_types_cannot_be_encoded() {
        let types = vec![FoliageType::new("a", "m")];
        let mut field = FoliageField::new(8.0).unwrap();
        field.insert(Instance::new(3, [0.0; 3])).unwrap();
        assert_eq!(
            encode(&types, &field).unwrap_err().code,
            "foliage.asset.type_index"
        );
    }
}
