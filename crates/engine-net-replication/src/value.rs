//! Typed property values, quantization and their bounded binary codec.
//!
//! The server keeps every property in its encoded form, so "changed" means "the quantized bytes
//! differ" and a client always decodes exactly what the server compares against.
use crate::error::{ReplicationError, Result};
use crate::wire::{Reader, put_varint, unzigzag, unzigzag64, varint_len, zigzag, zigzag64};
use serde::Serialize;

/// Hard cap on one string/bytes value, independent of configurable limits.
pub const MAX_VAR_BYTES: u16 = 4096;
/// Largest `|round(value / step)|` a [`Quantization::Step`] value may reach.
pub const STEP_LIMIT: i64 = 1 << 22;
const QUAT_RANGE: f64 = std::f64::consts::FRAC_1_SQRT_2;

/// Property value as seen by native code, the registry user and the client mirror.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum Value {
    Bool(bool),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    I32(i32),
    I64(i64),
    F32(f32),
    Vec2([f32; 2]),
    Vec3([f32; 3]),
    /// xyzw unit quaternion.
    Quat([f32; 4]),
    String(String),
    Bytes(Vec<u8>),
}

/// Storage type of a property or RPC argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ValueKind {
    Bool,
    U8,
    U16,
    U32,
    U64,
    I32,
    I64,
    F32,
    Vec2,
    Vec3,
    Quat,
    /// UTF-8 text of at most `max_len` bytes.
    String {
        max_len: u16,
    },
    /// Opaque bytes of at most `max_len` bytes.
    Bytes {
        max_len: u16,
    },
}

/// Lossy encoding of float-based kinds. Integers, bools, strings and bytes are always exact.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub enum Quantization {
    /// Raw `f32` bits (`Quat` uses 16-bit smallest-three).
    None,
    /// Fixed point over `[min, max]` with `bits` (1..=24) per component.
    Range { min: f32, max: f32, bits: u8 },
    /// Signed multiples of `step`, sent as zigzag varints; magnitude limited to `STEP_LIMIT` steps.
    Step { step: f32 },
    /// Quaternion smallest-three with `bits` (8..=16) per kept component.
    SmallestThree { bits: u8 },
}

/// Kind plus quantization of one value slot.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ValueSpec {
    pub kind: ValueKind,
    pub quant: Quantization,
}

impl ValueSpec {
    pub const fn new(kind: ValueKind) -> Self {
        Self {
            kind,
            quant: Quantization::None,
        }
    }
    pub const fn quantized(mut self, quant: Quantization) -> Self {
        self.quant = quant;
        self
    }
    pub const fn bool() -> Self {
        Self::new(ValueKind::Bool)
    }
    pub const fn u8() -> Self {
        Self::new(ValueKind::U8)
    }
    pub const fn u16() -> Self {
        Self::new(ValueKind::U16)
    }
    pub const fn u32() -> Self {
        Self::new(ValueKind::U32)
    }
    pub const fn u64() -> Self {
        Self::new(ValueKind::U64)
    }
    pub const fn i32() -> Self {
        Self::new(ValueKind::I32)
    }
    pub const fn i64() -> Self {
        Self::new(ValueKind::I64)
    }
    pub const fn f32() -> Self {
        Self::new(ValueKind::F32)
    }
    pub const fn vec2() -> Self {
        Self::new(ValueKind::Vec2)
    }
    pub const fn vec3() -> Self {
        Self::new(ValueKind::Vec3)
    }
    pub const fn quat() -> Self {
        Self::new(ValueKind::Quat)
    }
    pub const fn string(max_len: u16) -> Self {
        Self::new(ValueKind::String { max_len })
    }
    pub const fn bytes(max_len: u16) -> Self {
        Self::new(ValueKind::Bytes { max_len })
    }

    /// Checks that the kind/quantization combination is representable.
    pub fn validate(&self) -> Result<()> {
        let bad = |m: &'static str| Err(ReplicationError::Invalid(m));
        match (self.kind, self.quant) {
            (ValueKind::String { max_len } | ValueKind::Bytes { max_len }, _)
                if max_len == 0 || max_len > MAX_VAR_BYTES =>
            {
                bad("string/bytes max_len must be in 1..=4096")
            }
            (
                ValueKind::Bool
                | ValueKind::U8
                | ValueKind::U16
                | ValueKind::U32
                | ValueKind::U64
                | ValueKind::I32
                | ValueKind::I64
                | ValueKind::String { .. }
                | ValueKind::Bytes { .. },
                Quantization::None,
            ) => Ok(()),
            (ValueKind::F32 | ValueKind::Vec2 | ValueKind::Vec3, Quantization::None) => Ok(()),
            (
                ValueKind::F32 | ValueKind::Vec2 | ValueKind::Vec3,
                Quantization::Range { min, max, bits },
            ) => {
                if !min.is_finite()
                    || !max.is_finite()
                    || min >= max
                    || !(f64::from(max) - f64::from(min)).is_finite()
                {
                    return bad("range quantization needs finite min < max");
                }
                if !(1..=24).contains(&bits) {
                    return bad("range quantization bits must be in 1..=24");
                }
                Ok(())
            }
            (ValueKind::F32 | ValueKind::Vec2 | ValueKind::Vec3, Quantization::Step { step }) => {
                if !step.is_finite() || !(1e-6..=1e6).contains(&step) {
                    return bad("step quantization needs a step in 1e-6..=1e6");
                }
                Ok(())
            }
            (ValueKind::Quat, Quantization::None) => Ok(()),
            (ValueKind::Quat, Quantization::SmallestThree { bits }) => {
                if !(8..=16).contains(&bits) {
                    return bad("smallest-three bits must be in 8..=16");
                }
                Ok(())
            }
            _ => bad("quantization does not apply to this value kind"),
        }
    }

    /// Zero/empty value, always encodable.
    pub fn default_value(&self) -> Value {
        match self.kind {
            ValueKind::Bool => Value::Bool(false),
            ValueKind::U8 => Value::U8(0),
            ValueKind::U16 => Value::U16(0),
            ValueKind::U32 => Value::U32(0),
            ValueKind::U64 => Value::U64(0),
            ValueKind::I32 => Value::I32(0),
            ValueKind::I64 => Value::I64(0),
            ValueKind::F32 => Value::F32(self.clamp_default(0.0)),
            ValueKind::Vec2 => Value::Vec2([self.clamp_default(0.0); 2]),
            ValueKind::Vec3 => Value::Vec3([self.clamp_default(0.0); 3]),
            ValueKind::Quat => Value::Quat([0.0, 0.0, 0.0, 1.0]),
            ValueKind::String { .. } => Value::String(String::new()),
            ValueKind::Bytes { .. } => Value::Bytes(Vec::new()),
        }
    }

    fn clamp_default(&self, v: f32) -> f32 {
        match self.quant {
            Quantization::Range { min, max, .. } => v.clamp(min, max),
            _ => v,
        }
    }

    /// Upper bound of the encoded size in bytes.
    pub fn max_encoded_len(&self) -> usize {
        let scalar = match self.quant {
            Quantization::Range { bits, .. } => usize::from(bits).div_ceil(8),
            Quantization::Step { .. } => 5,
            _ => 4,
        };
        match self.kind {
            ValueKind::Bool | ValueKind::U8 => 1,
            ValueKind::U16 => 2,
            ValueKind::U32 | ValueKind::I32 => 5,
            ValueKind::U64 | ValueKind::I64 => 10,
            ValueKind::F32 => scalar,
            ValueKind::Vec2 => 2 * scalar,
            ValueKind::Vec3 => 3 * scalar,
            ValueKind::Quat => quat_bytes(self.quat_bits()),
            ValueKind::String { max_len } | ValueKind::Bytes { max_len } => {
                varint_len(u64::from(max_len)) + usize::from(max_len)
            }
        }
    }

    fn quat_bits(&self) -> u8 {
        match self.quant {
            Quantization::SmallestThree { bits } => bits,
            _ => 16,
        }
    }

    /// Appends the encoding of `value`. Errors on type mismatch, non-finite floats or
    /// over-long strings/bytes; out-of-range quantized floats are clamped.
    pub fn encode(&self, value: &Value, out: &mut Vec<u8>) -> Result<()> {
        let mismatch = || ReplicationError::Invalid("value type does not match its slot");
        match (self.kind, value) {
            (ValueKind::Bool, Value::Bool(v)) => out.push(u8::from(*v)),
            (ValueKind::U8, Value::U8(v)) => out.push(*v),
            (ValueKind::U16, Value::U16(v)) => out.extend_from_slice(&v.to_le_bytes()),
            (ValueKind::U32, Value::U32(v)) => put_varint(out, u64::from(*v)),
            (ValueKind::U64, Value::U64(v)) => put_varint(out, *v),
            (ValueKind::I32, Value::I32(v)) => put_varint(out, u64::from(zigzag(*v))),
            (ValueKind::I64, Value::I64(v)) => put_varint(out, zigzag64(*v)),
            (ValueKind::F32, Value::F32(v)) => scalar_encode(&self.quant, *v, out)?,
            (ValueKind::Vec2, Value::Vec2(v)) => {
                for c in v {
                    scalar_encode(&self.quant, *c, out)?;
                }
            }
            (ValueKind::Vec3, Value::Vec3(v)) => {
                for c in v {
                    scalar_encode(&self.quant, *c, out)?;
                }
            }
            (ValueKind::Quat, Value::Quat(v)) => quat_encode(self.quat_bits(), *v, out)?,
            (ValueKind::String { max_len }, Value::String(s)) => {
                if s.len() > usize::from(max_len) {
                    return Err(ReplicationError::Invalid("string exceeds its max_len"));
                }
                put_varint(out, s.len() as u64);
                out.extend_from_slice(s.as_bytes());
            }
            (ValueKind::Bytes { max_len }, Value::Bytes(b)) => {
                if b.len() > usize::from(max_len) {
                    return Err(ReplicationError::Invalid("bytes exceed their max_len"));
                }
                put_varint(out, b.len() as u64);
                out.extend_from_slice(b);
            }
            _ => return Err(mismatch()),
        }
        Ok(())
    }

    /// Decodes exactly one value from `bytes`, validating lengths before allocating.
    pub fn decode_exact(&self, bytes: &[u8]) -> Result<Value> {
        let mut r = Reader::new(bytes);
        let value = self.decode(&mut r)?;
        r.end()?;
        Ok(value)
    }

    pub(crate) fn decode(&self, r: &mut Reader<'_>) -> Result<Value> {
        Ok(match self.kind {
            ValueKind::Bool => match r.u8()? {
                0 => Value::Bool(false),
                1 => Value::Bool(true),
                _ => return Err(ReplicationError::Malformed("bool out of range")),
            },
            ValueKind::U8 => Value::U8(r.u8()?),
            ValueKind::U16 => Value::U16(r.u16()?),
            ValueKind::U32 => Value::U32(r.varint_u32()?),
            ValueKind::U64 => Value::U64(r.varint()?),
            ValueKind::I32 => Value::I32(unzigzag(r.varint_u32()?)),
            ValueKind::I64 => Value::I64(unzigzag64(r.varint()?)),
            ValueKind::F32 => Value::F32(scalar_decode(&self.quant, r)?),
            ValueKind::Vec2 => Value::Vec2([
                scalar_decode(&self.quant, r)?,
                scalar_decode(&self.quant, r)?,
            ]),
            ValueKind::Vec3 => Value::Vec3([
                scalar_decode(&self.quant, r)?,
                scalar_decode(&self.quant, r)?,
                scalar_decode(&self.quant, r)?,
            ]),
            ValueKind::Quat => Value::Quat(quat_decode(self.quat_bits(), r)?),
            ValueKind::String { max_len } => {
                let len = var_len(r, max_len)?;
                let raw = r.take(len)?;
                let text = std::str::from_utf8(raw)
                    .map_err(|_| ReplicationError::Malformed("string is not valid UTF-8"))?;
                Value::String(text.to_owned())
            }
            ValueKind::Bytes { max_len } => {
                let len = var_len(r, max_len)?;
                Value::Bytes(r.take(len)?.to_vec())
            }
        })
    }

    /// Canonical byte description hashed into the schema fingerprint.
    pub(crate) fn write_canonical(&self, out: &mut Vec<u8>) {
        let (tag, extra): (u8, u16) = match self.kind {
            ValueKind::Bool => (0, 0),
            ValueKind::U8 => (1, 0),
            ValueKind::U16 => (2, 0),
            ValueKind::U32 => (3, 0),
            ValueKind::U64 => (4, 0),
            ValueKind::I32 => (5, 0),
            ValueKind::I64 => (6, 0),
            ValueKind::F32 => (7, 0),
            ValueKind::Vec2 => (8, 0),
            ValueKind::Vec3 => (9, 0),
            ValueKind::Quat => (10, 0),
            ValueKind::String { max_len } => (11, max_len),
            ValueKind::Bytes { max_len } => (12, max_len),
        };
        out.push(tag);
        out.extend_from_slice(&extra.to_le_bytes());
        match self.quant {
            Quantization::None => out.push(0),
            Quantization::Range { min, max, bits } => {
                out.push(1);
                out.extend_from_slice(&min.to_bits().to_le_bytes());
                out.extend_from_slice(&max.to_bits().to_le_bytes());
                out.push(bits);
            }
            Quantization::Step { step } => {
                out.push(2);
                out.extend_from_slice(&step.to_bits().to_le_bytes());
            }
            Quantization::SmallestThree { bits } => {
                out.push(3);
                out.push(bits);
            }
        }
    }
}

fn var_len(r: &mut Reader<'_>, max_len: u16) -> Result<usize> {
    let len = r.varint()?;
    if len > u64::from(max_len) {
        return Err(ReplicationError::Limit(
            "string/bytes length above its max_len",
        ));
    }
    let len = len as usize;
    if len > r.remaining() {
        return Err(ReplicationError::Malformed(
            "string/bytes longer than packet",
        ));
    }
    Ok(len)
}

fn scalar_encode(quant: &Quantization, v: f32, out: &mut Vec<u8>) -> Result<()> {
    if !v.is_finite() {
        return Err(ReplicationError::Invalid("non-finite float"));
    }
    match *quant {
        Quantization::None => out.extend_from_slice(&v.to_le_bytes()),
        Quantization::Range { min, max, bits } => {
            let levels = ((1u64 << bits) - 1) as f64;
            let span = f64::from(max) - f64::from(min);
            let t = ((f64::from(v.clamp(min, max)) - f64::from(min)) / span).clamp(0.0, 1.0);
            let q = (t * levels).round() as u32;
            out.extend_from_slice(&q.to_le_bytes()[..usize::from(bits).div_ceil(8)]);
        }
        Quantization::Step { step } => {
            let q = (f64::from(v) / f64::from(step))
                .round()
                .clamp(-(STEP_LIMIT as f64), STEP_LIMIT as f64) as i32;
            put_varint(out, u64::from(zigzag(q)));
        }
        Quantization::SmallestThree { .. } => {
            return Err(ReplicationError::Invalid(
                "smallest-three applies to quaternions",
            ));
        }
    }
    Ok(())
}

fn scalar_decode(quant: &Quantization, r: &mut Reader<'_>) -> Result<f32> {
    match *quant {
        Quantization::None => {
            let v = f32::from_bits(r.u32()?);
            if v.is_finite() {
                Ok(v)
            } else {
                Err(ReplicationError::Malformed("non-finite float"))
            }
        }
        Quantization::Range { min, max, bits } => {
            let raw = r.take(usize::from(bits).div_ceil(8))?;
            let mut a = [0u8; 4];
            a[..raw.len()].copy_from_slice(raw);
            let q = u32::from_le_bytes(a);
            let levels = (1u64 << bits) - 1;
            if u64::from(q) > levels {
                return Err(ReplicationError::Malformed("quantized value above range"));
            }
            let span = f64::from(max) - f64::from(min);
            let v = f64::from(min) + (f64::from(q) / levels as f64) * span;
            Ok((v as f32).clamp(min, max))
        }
        Quantization::Step { step } => {
            let q = unzigzag(r.varint_u32()?);
            if i64::from(q).abs() > STEP_LIMIT {
                return Err(ReplicationError::Malformed("step value above limit"));
            }
            Ok((f64::from(q) * f64::from(step)) as f32)
        }
        Quantization::SmallestThree { .. } => Err(ReplicationError::Malformed(
            "smallest-three applies to quaternions",
        )),
    }
}

fn quat_bytes(bits: u8) -> usize {
    (2 + 3 * usize::from(bits)).div_ceil(8)
}

fn quat_encode(bits: u8, q: [f32; 4], out: &mut Vec<u8>) -> Result<()> {
    if q.iter().any(|c| !c.is_finite()) {
        return Err(ReplicationError::Invalid("non-finite quaternion"));
    }
    let len = q.iter().map(|c| f64::from(*c).powi(2)).sum::<f64>().sqrt();
    if len < 1e-6 {
        return Err(ReplicationError::Invalid("zero-length quaternion"));
    }
    let n = q.map(|c| f64::from(c) / len);
    let mut idx = 0;
    for k in 1..4 {
        if n[k].abs() > n[idx].abs() {
            idx = k;
        }
    }
    let sign = if n[idx] < 0.0 { -1.0 } else { 1.0 };
    let levels = ((1u64 << bits) - 1) as f64;
    let mut packed = idx as u64;
    let mut shift = 2;
    for (k, c) in n.iter().enumerate() {
        if k == idx {
            continue;
        }
        let c = (c * sign).clamp(-QUAT_RANGE, QUAT_RANGE);
        let t = (c + QUAT_RANGE) / (2.0 * QUAT_RANGE);
        packed |= ((t * levels).round() as u64) << shift;
        shift += bits;
    }
    out.extend_from_slice(&packed.to_le_bytes()[..quat_bytes(bits)]);
    Ok(())
}

fn quat_decode(bits: u8, r: &mut Reader<'_>) -> Result<[f32; 4]> {
    let raw = r.take(quat_bytes(bits))?;
    let mut a = [0u8; 8];
    a[..raw.len()].copy_from_slice(raw);
    let packed = u64::from_le_bytes(a);
    let total = 2 + 3 * u32::from(bits);
    if total < 64 && packed >> total != 0 {
        return Err(ReplicationError::Malformed("quaternion padding bits set"));
    }
    let idx = (packed & 3) as usize;
    let levels = ((1u64 << bits) - 1) as f64;
    let mask = (1u64 << bits) - 1;
    let mut c = [0f64; 4];
    let mut shift = 2;
    let mut sum = 0.0;
    for (k, slot) in c.iter_mut().enumerate() {
        if k == idx {
            continue;
        }
        let q = (packed >> shift) & mask;
        shift += u32::from(bits);
        let v = (q as f64 / levels) * (2.0 * QUAT_RANGE) - QUAT_RANGE;
        *slot = v;
        sum += v * v;
    }
    c[idx] = (1.0 - sum).max(0.0).sqrt();
    let len = c.iter().map(|v| v * v).sum::<f64>().sqrt();
    if len <= 0.0 || !len.is_finite() {
        return Err(ReplicationError::Malformed("degenerate quaternion"));
    }
    Ok(c.map(|v| (v / len) as f32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(spec: ValueSpec, value: &Value) -> Value {
        spec.validate().unwrap();
        let mut out = Vec::new();
        spec.encode(value, &mut out).unwrap();
        assert!(out.len() <= spec.max_encoded_len());
        let mut r = Reader::new(&out);
        let v = spec.decode(&mut r).unwrap();
        r.end().unwrap();
        v
    }

    #[test]
    fn exact_kinds_round_trip() {
        let cases = [
            (ValueSpec::bool(), Value::Bool(true)),
            (ValueSpec::u8(), Value::U8(200)),
            (ValueSpec::u16(), Value::U16(65_000)),
            (ValueSpec::u32(), Value::U32(u32::MAX)),
            (ValueSpec::u64(), Value::U64(u64::MAX)),
            (ValueSpec::i32(), Value::I32(i32::MIN)),
            (ValueSpec::i64(), Value::I64(i64::MIN)),
            (ValueSpec::f32(), Value::F32(-1.5e10)),
            (ValueSpec::vec3(), Value::Vec3([1.0, -2.0, 3.5])),
            (ValueSpec::string(16), Value::String("héllo".into())),
            (ValueSpec::bytes(8), Value::Bytes(vec![1, 2, 3])),
        ];
        for (spec, v) in cases {
            assert_eq!(round_trip(spec, &v), v);
        }
    }

    #[test]
    fn range_quantization_error_is_bounded_and_clamps() {
        let spec = ValueSpec::vec3().quantized(Quantization::Range {
            min: -100.0,
            max: 100.0,
            bits: 16,
        });
        let Value::Vec3(v) = round_trip(spec, &Value::Vec3([12.345, -99.9, 500.0])) else {
            panic!()
        };
        let step = 200.0 / 65535.0;
        assert!((v[0] - 12.345).abs() <= step);
        assert!((v[1] + 99.9).abs() <= step);
        assert_eq!(v[2], 100.0);
    }

    #[test]
    fn step_quantization_matches_grid() {
        let spec = ValueSpec::f32().quantized(Quantization::Step { step: 0.25 });
        assert_eq!(round_trip(spec, &Value::F32(3.3)), Value::F32(3.25));
        assert_eq!(round_trip(spec, &Value::F32(-0.1)), Value::F32(-0.0));
    }

    #[test]
    fn quaternion_round_trip_is_unit_and_close() {
        let q = [0.1f32, 0.7, -0.3, 0.6];
        let len = q.iter().map(|c| c * c).sum::<f32>().sqrt();
        let spec = ValueSpec::quat().quantized(Quantization::SmallestThree { bits: 12 });
        let Value::Quat(d) = round_trip(spec, &Value::Quat(q)) else {
            panic!()
        };
        let dl = d.iter().map(|c| c * c).sum::<f32>().sqrt();
        assert!((dl - 1.0).abs() < 1e-5);
        let dot: f32 = q.iter().zip(&d).map(|(a, b)| a / len * b).sum();
        assert!(dot.abs() > 0.9999, "dot {dot}");
        let mut again = Vec::new();
        spec.encode(&Value::Quat(d), &mut again).unwrap();
        let mut once = Vec::new();
        spec.encode(&Value::Quat(q), &mut once).unwrap();
        // Re-encoding the decoded value is stable at the value level.
        let Value::Quat(d2) = round_trip(spec, &Value::Quat(d)) else {
            panic!()
        };
        let dot2: f32 = d.iter().zip(&d2).map(|(a, b)| a * b).sum();
        assert!(dot2.abs() > 0.9999);
    }

    #[test]
    fn rejects_bad_input() {
        let spec = ValueSpec::string(4);
        let mut out = Vec::new();
        assert!(
            spec.encode(&Value::String("toolong".into()), &mut out)
                .is_err()
        );
        assert!(
            ValueSpec::f32()
                .encode(&Value::F32(f32::NAN), &mut out)
                .is_err()
        );
        assert!(
            ValueSpec::quat()
                .encode(&Value::Quat([0.0; 4]), &mut out)
                .is_err()
        );
        assert!(ValueSpec::u8().encode(&Value::U16(1), &mut out).is_err());
        // Bad UTF-8, oversize length and truncated string on decode.
        assert!(spec.decode(&mut Reader::new(&[2, 0xff, 0xfe])).is_err());
        assert!(spec.decode(&mut Reader::new(&[200])).is_err());
        assert!(spec.decode(&mut Reader::new(&[3, b'a'])).is_err());
        assert!(ValueSpec::bool().decode(&mut Reader::new(&[2])).is_err());
        assert!(
            ValueSpec::f32()
                .decode(&mut Reader::new(&f32::INFINITY.to_le_bytes()))
                .is_err()
        );
    }

    #[test]
    fn spec_validation_rejects_nonsense() {
        assert!(
            ValueSpec::u8()
                .quantized(Quantization::Step { step: 1.0 })
                .validate()
                .is_err()
        );
        assert!(
            ValueSpec::f32()
                .quantized(Quantization::Range {
                    min: 1.0,
                    max: 1.0,
                    bits: 8
                })
                .validate()
                .is_err()
        );
        assert!(
            ValueSpec::f32()
                .quantized(Quantization::Range {
                    min: 0.0,
                    max: 1.0,
                    bits: 25
                })
                .validate()
                .is_err()
        );
        assert!(
            ValueSpec::quat()
                .quantized(Quantization::SmallestThree { bits: 4 })
                .validate()
                .is_err()
        );
        assert!(ValueSpec::string(0).validate().is_err());
        assert!(ValueSpec::bytes(5000).validate().is_err());
    }
}
