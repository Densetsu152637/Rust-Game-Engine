//! Bounded little-endian binary codec for application input and prediction messages.
//!
//! [`Writer`] refuses to grow past its limit; [`Reader`] never reads past its slice and every
//! accessor returns [`PredictError::Malformed`] instead of panicking. Floats must be finite and
//! booleans must be exactly 0 or 1, so every decoded value has one canonical encoding.
use crate::{PredictError, Result};
use glam::{Quat, Vec2, Vec3};

/// Binary encoding for values carried inside prediction messages.
///
/// Implementations must write at most [`NetCodec::MAX_ENCODED_BYTES`] bytes; message encoders
/// verify this and fail with [`PredictError::Invalid`] otherwise. Decoders must reject any
/// value the encoder would not produce (fail closed) and must not allocate based on unchecked
/// lengths read from the input.
pub trait NetCodec: Sized {
    const MAX_ENCODED_BYTES: usize;
    fn encode(&self, writer: &mut Writer) -> Result<()>;
    fn decode(reader: &mut Reader<'_>) -> Result<Self>;
}

/// Append-only byte buffer with a hard size limit.
#[derive(Debug)]
pub struct Writer {
    bytes: Vec<u8>,
    limit: usize,
}
impl Writer {
    pub fn with_limit(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit.min(4096)),
            limit,
        }
    }
    pub fn len(&self) -> usize {
        self.bytes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
    pub fn limit(&self) -> usize {
        self.limit
    }
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
    pub fn put_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(PredictError::Invalid(
                "encoded message exceeds its byte limit",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
    pub fn put_u8(&mut self, v: u8) -> Result<()> {
        self.put_bytes(&[v])
    }
    pub fn put_u16(&mut self, v: u16) -> Result<()> {
        self.put_bytes(&v.to_le_bytes())
    }
    pub fn put_u32(&mut self, v: u32) -> Result<()> {
        self.put_bytes(&v.to_le_bytes())
    }
    pub fn put_u64(&mut self, v: u64) -> Result<()> {
        self.put_bytes(&v.to_le_bytes())
    }
    pub fn put_i32(&mut self, v: i32) -> Result<()> {
        self.put_bytes(&v.to_le_bytes())
    }
    pub fn put_bool(&mut self, v: bool) -> Result<()> {
        self.put_u8(u8::from(v))
    }
    /// Non-finite floats are rejected on encode as well as decode.
    pub fn put_f32(&mut self, v: f32) -> Result<()> {
        if !v.is_finite() {
            return Err(PredictError::Invalid("non-finite float"));
        }
        self.put_bytes(&v.to_le_bytes())
    }
    /// Encodes one value and checks it honoured its declared maximum size.
    pub fn put<T: NetCodec>(&mut self, value: &T) -> Result<()> {
        let before = self.bytes.len();
        value.encode(self)?;
        if self.bytes.len() - before > T::MAX_ENCODED_BYTES {
            return Err(PredictError::Invalid(
                "NetCodec value exceeded its MAX_ENCODED_BYTES",
            ));
        }
        Ok(())
    }
}

/// Bounded cursor over untrusted bytes.
#[derive(Debug)]
pub struct Reader<'a> {
    bytes: &'a [u8],
}
impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }
    pub fn remaining(&self) -> usize {
        self.bytes.len()
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.bytes.len() < n {
            return Err(PredictError::Malformed("truncated message"));
        }
        let (head, tail) = self.bytes.split_at(n);
        self.bytes = tail;
        Ok(head)
    }
    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut out = [0; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.array()?))
    }
    pub fn bool(&mut self) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(PredictError::Malformed("boolean byte must be 0 or 1")),
        }
    }
    pub fn f32(&mut self) -> Result<f32> {
        let v = f32::from_le_bytes(self.array()?);
        if !v.is_finite() {
            return Err(PredictError::Malformed("non-finite float"));
        }
        Ok(v)
    }
    pub fn get<T: NetCodec>(&mut self) -> Result<T> {
        T::decode(self)
    }
    /// Fails when unread bytes remain.
    pub fn finish(&self) -> Result<()> {
        if self.bytes.is_empty() {
            Ok(())
        } else {
            Err(PredictError::Malformed("trailing bytes"))
        }
    }
}

/// Encodes one value into an exact-size buffer.
pub fn encode_value<T: NetCodec>(value: &T) -> Result<Vec<u8>> {
    let mut w = Writer::with_limit(T::MAX_ENCODED_BYTES);
    w.put(value)?;
    Ok(w.into_bytes())
}

/// Decodes one value, rejecting oversized input and trailing bytes.
pub fn decode_value<T: NetCodec>(bytes: &[u8]) -> Result<T> {
    if bytes.len() > T::MAX_ENCODED_BYTES {
        return Err(PredictError::Malformed("value larger than its codec bound"));
    }
    let mut r = Reader::new(bytes);
    let v = T::decode(&mut r)?;
    r.finish()?;
    Ok(v)
}

macro_rules! int_codec {
    ($($t:ty),*) => {$(
        impl NetCodec for $t {
            const MAX_ENCODED_BYTES: usize = std::mem::size_of::<$t>();
            fn encode(&self, w: &mut Writer) -> Result<()> {
                w.put_bytes(&self.to_le_bytes())
            }
            fn decode(r: &mut Reader<'_>) -> Result<Self> {
                Ok(<$t>::from_le_bytes(r.array()?))
            }
        }
    )*};
}
int_codec!(u8, u16, u32, u64, i8, i16, i32, i64);

impl NetCodec for bool {
    const MAX_ENCODED_BYTES: usize = 1;
    fn encode(&self, w: &mut Writer) -> Result<()> {
        w.put_bool(*self)
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        r.bool()
    }
}
impl NetCodec for f32 {
    const MAX_ENCODED_BYTES: usize = 4;
    fn encode(&self, w: &mut Writer) -> Result<()> {
        w.put_f32(*self)
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        r.f32()
    }
}
impl NetCodec for () {
    const MAX_ENCODED_BYTES: usize = 0;
    fn encode(&self, _: &mut Writer) -> Result<()> {
        Ok(())
    }
    fn decode(_: &mut Reader<'_>) -> Result<Self> {
        Ok(())
    }
}
impl<T: NetCodec + Copy + Default, const N: usize> NetCodec for [T; N] {
    const MAX_ENCODED_BYTES: usize = N * T::MAX_ENCODED_BYTES;
    fn encode(&self, w: &mut Writer) -> Result<()> {
        self.iter().try_for_each(|v| w.put(v))
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let mut out = [T::default(); N];
        for slot in &mut out {
            *slot = T::decode(r)?;
        }
        Ok(out)
    }
}
impl<T: NetCodec> NetCodec for Option<T> {
    const MAX_ENCODED_BYTES: usize = 1 + T::MAX_ENCODED_BYTES;
    fn encode(&self, w: &mut Writer) -> Result<()> {
        match self {
            None => w.put_u8(0),
            Some(v) => {
                w.put_u8(1)?;
                w.put(v)
            }
        }
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        match r.u8()? {
            0 => Ok(None),
            1 => Ok(Some(T::decode(r)?)),
            _ => Err(PredictError::Malformed("option tag must be 0 or 1")),
        }
    }
}
impl NetCodec for Vec2 {
    const MAX_ENCODED_BYTES: usize = 8;
    fn encode(&self, w: &mut Writer) -> Result<()> {
        w.put_f32(self.x)?;
        w.put_f32(self.y)
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self::new(r.f32()?, r.f32()?))
    }
}
impl NetCodec for Vec3 {
    const MAX_ENCODED_BYTES: usize = 12;
    fn encode(&self, w: &mut Writer) -> Result<()> {
        w.put_f32(self.x)?;
        w.put_f32(self.y)?;
        w.put_f32(self.z)
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self::new(r.f32()?, r.f32()?, r.f32()?))
    }
}
/// Rotations must be unit quaternions (|q| within 1e-3 of 1).
impl NetCodec for Quat {
    const MAX_ENCODED_BYTES: usize = 16;
    fn encode(&self, w: &mut Writer) -> Result<()> {
        if (self.length() - 1.0).abs() > 1.0e-3 {
            return Err(PredictError::Invalid("rotation is not a unit quaternion"));
        }
        for v in self.to_array() {
            w.put_f32(v)?;
        }
        Ok(())
    }
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let q = Self::from_xyzw(r.f32()?, r.f32()?, r.f32()?, r.f32()?);
        if (q.length() - 1.0).abs() > 1.0e-3 {
            return Err(PredictError::Malformed("rotation is not a unit quaternion"));
        }
        Ok(q)
    }
}
macro_rules! tuple_codec {
    ($($name:ident),+) => {
        impl<$($name: NetCodec),+> NetCodec for ($($name,)+) {
            const MAX_ENCODED_BYTES: usize = 0 $(+ $name::MAX_ENCODED_BYTES)+;
            #[allow(non_snake_case)]
            fn encode(&self, w: &mut Writer) -> Result<()> {
                let ($($name,)+) = self;
                $(w.put($name)?;)+
                Ok(())
            }
            fn decode(r: &mut Reader<'_>) -> Result<Self> {
                Ok(($($name::decode(r)?,)+))
            }
        }
    };
}
tuple_codec!(A, B);
tuple_codec!(A, B, C);
tuple_codec!(A, B, C, D);
