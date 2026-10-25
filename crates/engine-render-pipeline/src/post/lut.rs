//! 3D colour lookup tables from `.cube` files, validated at the boundary.

use anyhow::{Result, bail, ensure};

/// Smallest and largest accepted `LUT_3D_SIZE`.
pub const LUT_MIN_SIZE: u32 = 2;
pub const LUT_MAX_SIZE: u32 = 64;
/// Largest accepted `.cube` file, in bytes.
pub const LUT_MAX_BYTES: usize = 16 * 1024 * 1024;

/// A cubic RGB lookup table: `size^3` entries, red varying fastest.
#[derive(Clone, Debug, PartialEq)]
pub struct Lut3d {
    pub size: u32,
    /// Input domain per channel (`DOMAIN_MIN` / `DOMAIN_MAX`).
    pub domain_min: [f32; 3],
    pub domain_max: [f32; 3],
    /// `size^3` RGB triples.
    pub data: Vec<[f32; 3]>,
}

impl Lut3d {
    /// The identity table of `size` per axis.
    pub fn identity(size: u32) -> Result<Self> {
        ensure!(
            (LUT_MIN_SIZE..=LUT_MAX_SIZE).contains(&size),
            "LUT size must be {LUT_MIN_SIZE}..={LUT_MAX_SIZE}"
        );
        let n = size as usize;
        let step = 1.0 / (size - 1) as f32;
        let mut data = Vec::with_capacity(n * n * n);
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    data.push([r as f32 * step, g as f32 * step, b as f32 * step]);
                }
            }
        }
        Ok(Self {
            size,
            domain_min: [0.0; 3],
            domain_max: [1.0; 3],
            data,
        })
    }

    /// Parse and validate a 3D `.cube` file. Rejects 1D tables, a missing or
    /// out-of-range size, wrong entry counts, non-finite values and bad
    /// domains; `TITLE` and comments are ignored.
    pub fn parse_cube(text: &str) -> Result<Self> {
        ensure!(text.len() <= LUT_MAX_BYTES, "the .cube file is too large");
        let mut size = None;
        let mut domain_min = [0.0f32; 3];
        let mut domain_max = [1.0f32; 3];
        let mut data: Vec<[f32; 3]> = Vec::new();
        for (index, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let at = index + 1;
            let mut words = line.split_whitespace();
            let first = words.next().unwrap_or("");
            let numbers = |words: &mut std::str::SplitWhitespace<'_>| -> Result<[f32; 3]> {
                let mut out = [0.0f32; 3];
                for slot in &mut out {
                    let word = words
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("line {at}: expected three numbers"))?;
                    let value: f32 = word
                        .parse()
                        .map_err(|_| anyhow::anyhow!("line {at}: invalid number {word:?}"))?;
                    ensure!(value.is_finite(), "line {at}: non-finite number");
                    *slot = value;
                }
                ensure!(words.next().is_none(), "line {at}: trailing tokens");
                Ok(out)
            };
            match first {
                "TITLE" => {}
                "LUT_1D_SIZE" => bail!("line {at}: 1D LUTs are not supported"),
                "LUT_3D_SIZE" => {
                    let value: u32 = words
                        .next()
                        .and_then(|w| w.parse().ok())
                        .ok_or_else(|| anyhow::anyhow!("line {at}: invalid LUT_3D_SIZE"))?;
                    ensure!(
                        (LUT_MIN_SIZE..=LUT_MAX_SIZE).contains(&value),
                        "line {at}: LUT_3D_SIZE must be {LUT_MIN_SIZE}..={LUT_MAX_SIZE}"
                    );
                    ensure!(size.is_none(), "line {at}: duplicate LUT_3D_SIZE");
                    size = Some(value);
                }
                "DOMAIN_MIN" => domain_min = numbers(&mut words)?,
                "DOMAIN_MAX" => domain_max = numbers(&mut words)?,
                _ => {
                    ensure!(
                        first.starts_with(|c: char| c.is_ascii_digit() || "+-.".contains(c)),
                        "line {at}: unknown keyword {first:?}"
                    );
                    let mut all = line.split_whitespace();
                    let triple = numbers(&mut all)?;
                    let size =
                        size.ok_or_else(|| anyhow::anyhow!("line {at}: data before LUT_3D_SIZE"))?;
                    ensure!(
                        data.len() < (size as usize).pow(3),
                        "line {at}: more entries than size^3"
                    );
                    data.push(triple);
                }
            }
        }
        let size = size.ok_or_else(|| anyhow::anyhow!("missing LUT_3D_SIZE"))?;
        ensure!(
            data.len() == (size as usize).pow(3),
            "expected {} entries, found {}",
            (size as usize).pow(3),
            data.len()
        );
        ensure!(
            (0..3).all(|i| domain_max[i] > domain_min[i]),
            "DOMAIN_MAX must exceed DOMAIN_MIN"
        );
        Ok(Self {
            size,
            domain_min,
            domain_max,
            data,
        })
    }
}

/// IEEE half-precision bits of `value` (round to nearest, saturating).
pub(crate) fn f32_to_f16_bits(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mantissa = bits & 0x007f_ffff;
    if value.is_nan() {
        return sign | 0x7e00;
    }
    if exponent >= 31 {
        return sign | 0x7bff;
    }
    if exponent <= 0 {
        if exponent < -10 {
            return sign;
        }
        let m = (mantissa | 0x0080_0000) >> (1 - exponent);
        return sign | ((m + 0x1000) >> 13) as u16;
    }
    let rounded = ((exponent as u32) << 10) | ((mantissa + 0x1000) >> 13);
    sign | rounded as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube(size: u32, body: &str) -> String {
        format!("TITLE \"t\"\n# c\nLUT_3D_SIZE {size}\n{body}")
    }

    #[test]
    fn parses_identity() {
        let mut body = String::new();
        let id = Lut3d::identity(2).unwrap();
        for [r, g, b] in &id.data {
            body.push_str(&format!("{r} {g} {b}\n"));
        }
        assert_eq!(Lut3d::parse_cube(&cube(2, &body)).unwrap(), id);
    }

    #[test]
    fn rejects_malformed() {
        assert!(Lut3d::parse_cube("LUT_1D_SIZE 4\n").is_err());
        assert!(Lut3d::parse_cube(&cube(2, "0 0 0\n")).is_err(), "too few");
        assert!(Lut3d::parse_cube(&cube(1, "")).is_err(), "size");
        assert!(Lut3d::parse_cube(&cube(65, "")).is_err(), "size");
        assert!(Lut3d::parse_cube("0 0 0\nLUT_3D_SIZE 2\n").is_err());
        assert!(Lut3d::parse_cube(&cube(2, &"0 0 nan\n".repeat(8))).is_err());
        assert!(Lut3d::parse_cube(&cube(2, &"0 0 x\n".repeat(8))).is_err());
        assert!(Lut3d::parse_cube(&cube(2, &"0 0 0 0\n".repeat(8))).is_err());
        assert!(Lut3d::parse_cube(&cube(2, &"0 0 0\n".repeat(9))).is_err());
        let bad_domain = format!(
            "DOMAIN_MIN 1 1 1\nDOMAIN_MAX 0 0 0\n{}",
            cube(2, &"0 0 0\n".repeat(8))
        );
        assert!(Lut3d::parse_cube(&bad_domain).is_err());
        assert!(Lut3d::parse_cube("BOGUS 1\n").is_err());
    }

    #[test]
    fn half_conversion() {
        assert_eq!(f32_to_f16_bits(0.0), 0);
        assert_eq!(f32_to_f16_bits(1.0), 0x3c00);
        assert_eq!(f32_to_f16_bits(-2.0), 0xc000);
        assert_eq!(f32_to_f16_bits(0.5), 0x3800);
        assert_eq!(f32_to_f16_bits(1e9), 0x7bff);
    }
}
