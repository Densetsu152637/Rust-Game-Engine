//! The versioned binary terrain asset file (`.terrain`).
//!
//! Layout, all integers and floats little-endian:
//!
//! | Offset | Size | Field |
//! | ------ | ---- | ----- |
//! | 0 | 4 | magic `ETRN` |
//! | 4 | 4 | `u32` version, currently 1 |
//! | 8 | 4 | `u32` header length `n` (1..=65536) |
//! | 12 | n | UTF-8 JSON [`AssetHeader`] |
//! | | `4 * columns * rows` | `f32` heights, row-major (rows along Z) |
//! | | `layers * columns * rows` | `u8` splat weights, `layers` per sample, summing to 255 |
//! | | `ceil(cells / 8)` if `holes` | hole bits, cell `i = row * (columns - 1) + column` is bit `i % 8` of byte `i / 8` |
//! | end - 8 | 8 | `u64` FNV-1a 64 checksum of every preceding byte |
//!
//! Decoding rejects a wrong magic or version, a header that is not valid JSON or has unknown
//! fields, sizes beyond the heightfield limits, a length that does not match the header
//! exactly, non-finite or out-of-range heights, unnormalized weights and a checksum mismatch.
use crate::heightfield::{Heightfield, TerrainLayer};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

/// File magic.
pub const MAGIC: [u8; 4] = *b"ETRN";
/// Current format version.
pub const VERSION: u32 = 1;
/// Largest accepted header.
pub const MAX_HEADER_BYTES: usize = 64 * 1024;
/// Largest accepted file (a 2049 x 2049 grid with 8 layers and holes is about 51 MiB).
pub const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;

/// The JSON header of a terrain file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetHeader {
    pub columns: u32,
    pub rows: u32,
    pub spacing: f32,
    pub layers: Vec<TerrainLayer>,
    /// Whether the hole bits follow the splat weights.
    pub holes: bool,
}

fn malformed(message: impl Into<String>) -> Error {
    Error::new("terrain.asset.malformed", message)
}

/// FNV-1a 64-bit hash.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Serializes `field` (holes are written only when present).
pub fn encode(field: &Heightfield) -> Vec<u8> {
    let header = AssetHeader {
        columns: field.columns(),
        rows: field.rows(),
        spacing: field.spacing(),
        layers: field.layers().to_vec(),
        holes: field.hole_count() > 0,
    };
    let json = serde_json::to_vec(&header).expect("header serializes");
    let mut out = Vec::with_capacity(
        20 + json.len()
            + field.sample_count() * (4 + field.layers().len())
            + field.cell_count() / 8,
    );
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(&json);
    for h in field.heights() {
        out.extend_from_slice(&h.to_le_bytes());
    }
    out.extend_from_slice(field.splat());
    if header.holes {
        let mut bits = vec![0u8; field.cell_count().div_ceil(8)];
        for (i, hole) in field.hole_flags().into_iter().enumerate() {
            if hole {
                bits[i / 8] |= 1 << (i % 8);
            }
        }
        out.extend_from_slice(&bits);
    }
    let checksum = fnv1a64(&out);
    out.extend_from_slice(&checksum.to_le_bytes());
    out
}

/// Parses and validates a terrain file.
pub fn decode(bytes: &[u8]) -> Result<Heightfield> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(Error::new(
            "terrain.asset.too_large",
            format!("terrain files are limited to {MAX_FILE_BYTES} bytes"),
        ));
    }
    if bytes.len() < 20 || bytes[0..4] != MAGIC {
        return Err(malformed("not a terrain file (magic ETRN missing)"));
    }
    let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"));
    let version = word(4);
    if version != VERSION {
        return Err(Error::new(
            "terrain.asset.version",
            format!("unsupported terrain file version {version}"),
        ));
    }
    let header_len = word(8) as usize;
    if header_len == 0 || header_len > MAX_HEADER_BYTES || 12 + header_len + 8 > bytes.len() {
        return Err(malformed("invalid header length"));
    }
    let header: AssetHeader = serde_json::from_slice(&bytes[12..12 + header_len])
        .map_err(|e| malformed(format!("invalid header: {e}")))?;
    // Dimension limits before any size arithmetic or allocation.
    crate::heightfield::check_dims(header.columns, header.rows, header.spacing)?;
    crate::heightfield::check_layers(&header.layers)?;
    let samples = header.columns as usize * header.rows as usize;
    let cells = (header.columns as usize - 1) * (header.rows as usize - 1);
    let heights_at = 12 + header_len;
    let splat_at = heights_at + samples * 4;
    let holes_at = splat_at + samples * header.layers.len();
    let end = holes_at + if header.holes { cells.div_ceil(8) } else { 0 };
    if end + 8 != bytes.len() {
        return Err(malformed(format!(
            "expected {} bytes for this header, found {}",
            end + 8,
            bytes.len()
        )));
    }
    let stored = u64::from_le_bytes(bytes[end..end + 8].try_into().expect("8 bytes"));
    if stored != fnv1a64(&bytes[..end]) {
        return Err(Error::new(
            "terrain.asset.checksum",
            "checksum mismatch: the file is corrupt",
        ));
    }
    let heights = bytes[heights_at..splat_at]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    let splat = bytes[splat_at..holes_at].to_vec();
    let holes = header.holes.then(|| {
        (0..cells)
            .map(|i| bytes[holes_at + i / 8] & (1 << (i % 8)) != 0)
            .collect()
    });
    Heightfield::from_parts(
        header.columns,
        header.rows,
        header.spacing,
        heights,
        header.layers,
        splat,
        holes,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::{Brush, SculptOp, paint_holes, paint_layer, sculpt};

    fn sample() -> Heightfield {
        let mut f = Heightfield::new(9, 5, 0.5).unwrap();
        f.add_layer(TerrainLayer::new("rock", [0.4, 0.4, 0.4, 1.0]))
            .unwrap();
        sculpt(&mut f, &Brush::new([2.0, 1.0], 1.5), SculptOp::Raise).unwrap();
        paint_layer(&mut f, &Brush::new([1.0, 1.0], 1.0), 1).unwrap();
        paint_holes(&mut f, &Brush::new([3.0, 1.0], 0.4), true).unwrap();
        f
    }

    #[test]
    fn round_trip_preserves_every_part() {
        let f = sample();
        assert!(f.hole_count() > 0);
        let bytes = encode(&f);
        let g = decode(&bytes).unwrap();
        assert_eq!(g.heights(), f.heights());
        assert_eq!(g.splat(), f.splat());
        assert_eq!(g.hole_flags(), f.hole_flags());
        assert_eq!(g.layers(), f.layers());
        assert_eq!(g.spacing(), 0.5);
        // Without holes no hole section is written.
        let plain = encode(&Heightfield::new(3, 3, 1.0).unwrap());
        assert_eq!(plain.len(), 12 + plain_header_len(&plain) + 9 * 4 + 9 + 8);
    }

    fn plain_header_len(bytes: &[u8]) -> usize {
        u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize
    }

    #[test]
    fn malformed_and_oversized_input_is_rejected() {
        let bytes = encode(&sample());
        let code = |b: &[u8]| decode(b).unwrap_err().code;
        assert_eq!(code(&bytes[..bytes.len() - 1]), "terrain.asset.malformed");
        assert_eq!(code(b"nope"), "terrain.asset.malformed");
        let mut corrupt = bytes.clone();
        let mid = corrupt.len() / 2;
        corrupt[mid] ^= 0x55;
        assert_eq!(code(&corrupt), "terrain.asset.checksum");
        let mut version = bytes.clone();
        version[4] = 2;
        assert_eq!(code(&version), "terrain.asset.version");
        // A header claiming a huge grid is rejected before allocating.
        let header = br#"{"columns":4097,"rows":4097,"spacing":1,"layers":[{"name":"a","tint":[1,1,1,1],"uv_tiling":1}],"holes":false}"#;
        let mut huge = Vec::new();
        huge.extend_from_slice(&MAGIC);
        huge.extend_from_slice(&1u32.to_le_bytes());
        huge.extend_from_slice(&(header.len() as u32).to_le_bytes());
        huge.extend_from_slice(header);
        huge.extend_from_slice(&[0; 8]);
        assert_eq!(code(&huge), "terrain.size");
        let unknown = br#"{"columns":2,"rows":2,"spacing":1,"layers":[],"holes":false,"x":1}"#;
        let mut bad = huge[..12].to_vec();
        bad[8..12].copy_from_slice(&(unknown.len() as u32).to_le_bytes());
        bad.extend_from_slice(unknown);
        bad.extend_from_slice(&[0; 8]);
        assert_eq!(code(&bad), "terrain.asset.malformed");
        assert_eq!(
            code(&vec![0u8; MAX_FILE_BYTES + 1]),
            "terrain.asset.too_large"
        );
    }
}
