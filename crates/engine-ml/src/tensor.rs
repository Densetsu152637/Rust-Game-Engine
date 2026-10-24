//! Engine-facing tensors and conversions from engine data.
use crate::{MlError, Result};
use serde::{Deserialize, Serialize};
use tract_onnx::prelude::{DatumType, Tensor as TractTensor};

/// Element types the engine API exchanges with models.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DType {
    F32,
    I64,
    I32,
    U8,
}
impl DType {
    pub(crate) fn from_tract(datum: DatumType) -> Option<Self> {
        Some(match datum {
            DatumType::F32 => Self::F32,
            DatumType::I64 => Self::I64,
            DatumType::I32 => Self::I32,
            DatumType::U8 => Self::U8,
            _ => return None,
        })
    }
    pub(crate) fn to_tract(self) -> DatumType {
        match self {
            Self::F32 => DatumType::F32,
            Self::I64 => DatumType::I64,
            Self::I32 => DatumType::I32,
            Self::U8 => DatumType::U8,
        }
    }
}

/// Row-major element storage.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "dtype", content = "values", rename_all = "snake_case")]
pub enum TensorData {
    F32(Vec<f32>),
    I64(Vec<i64>),
    I32(Vec<i32>),
    U8(Vec<u8>),
}
impl TensorData {
    pub fn len(&self) -> usize {
        match self {
            Self::F32(v) => v.len(),
            Self::I64(v) => v.len(),
            Self::I32(v) => v.len(),
            Self::U8(v) => v.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn dtype(&self) -> DType {
        match self {
            Self::F32(_) => DType::F32,
            Self::I64(_) => DType::I64,
            Self::I32(_) => DType::I32,
            Self::U8(_) => DType::U8,
        }
    }
}

/// A dense row-major tensor. The element count always equals the shape product, and
/// deserialisation enforces the same invariant as [`Tensor::new`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawTensor", into = "RawTensor")]
pub struct Tensor {
    shape: Vec<usize>,
    data: TensorData,
}
#[derive(Serialize, Deserialize)]
struct RawTensor {
    shape: Vec<usize>,
    #[serde(flatten)]
    data: TensorData,
}
impl TryFrom<RawTensor> for Tensor {
    type Error = MlError;
    fn try_from(raw: RawTensor) -> Result<Self> {
        Self::new(raw.shape, raw.data)
    }
}
impl From<Tensor> for RawTensor {
    fn from(tensor: Tensor) -> Self {
        Self {
            shape: tensor.shape,
            data: tensor.data,
        }
    }
}
impl Tensor {
    /// Validates that `data` holds exactly `product(shape)` finite elements.
    pub fn new(shape: Vec<usize>, data: TensorData) -> Result<Self> {
        let expected = element_count(&shape)?;
        if expected != data.len() {
            return Err(MlError::new(
                "ml.tensor.shape",
                format!(
                    "shape {shape:?} needs {expected} elements, got {}",
                    data.len()
                ),
            ));
        }
        if let TensorData::F32(values) = &data
            && values.iter().any(|v| !v.is_finite())
        {
            return Err(MlError::new(
                "ml.tensor.non_finite",
                "f32 tensors must contain only finite values",
            ));
        }
        Ok(Self { shape, data })
    }
    pub fn f32(shape: Vec<usize>, values: Vec<f32>) -> Result<Self> {
        Self::new(shape, TensorData::F32(values))
    }
    pub fn i64(shape: Vec<usize>, values: Vec<i64>) -> Result<Self> {
        Self::new(shape, TensorData::I64(values))
    }
    /// A `[1, n]` batch-of-one row vector.
    pub fn row(values: &[f32]) -> Result<Self> {
        Self::f32(vec![1, values.len()], values.to_vec())
    }
    /// `[count, 3]` from engine vectors (positions, velocities, colours).
    pub fn from_vec3s(values: &[[f32; 3]]) -> Result<Self> {
        Self::f32(
            vec![values.len(), 3],
            values.iter().flat_map(|v| v.iter().copied()).collect(),
        )
    }
    /// RGBA8 pixels (row-major, `width * height * 4` bytes) to an NCHW `[1, 3, h, w]`
    /// f32 tensor scaled to `[0, 1]`; alpha is dropped. This is the layout most vision
    /// models expect.
    pub fn from_rgba8_nchw(width: usize, height: usize, rgba: &[u8]) -> Result<Self> {
        let pixels = width
            .checked_mul(height)
            .filter(|p| p.checked_mul(4) == Some(rgba.len()))
            .ok_or_else(|| {
                MlError::new(
                    "ml.tensor.image",
                    "RGBA buffer must hold width * height * 4 bytes",
                )
            })?;
        let mut out = vec![0.0; pixels * 3];
        for (index, pixel) in rgba.chunks_exact(4).enumerate() {
            for channel in 0..3 {
                out[channel * pixels + index] = pixel[channel] as f32 / 255.0;
            }
        }
        Self::f32(vec![1, 3, height, width], out)
    }
    pub fn shape(&self) -> &[usize] {
        &self.shape
    }
    pub fn data(&self) -> &TensorData {
        &self.data
    }
    pub fn dtype(&self) -> DType {
        self.data.dtype()
    }
    pub fn len(&self) -> usize {
        self.data.len()
    }
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
    pub fn as_f32(&self) -> Option<&[f32]> {
        match &self.data {
            TensorData::F32(v) => Some(v),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<&[i64]> {
        match &self.data {
            TensorData::I64(v) => Some(v),
            _ => None,
        }
    }
    /// Index of the largest f32 element (first on ties); `None` for empty or non-f32 data.
    pub fn argmax(&self) -> Option<usize> {
        let values = self.as_f32()?;
        let mut best: Option<(usize, f32)> = None;
        for (index, value) in values.iter().copied().enumerate() {
            if best.is_none_or(|(_, b)| value > b) {
                best = Some((index, value));
            }
        }
        best.map(|(index, _)| index)
    }
    pub(crate) fn to_tract(&self) -> Result<TractTensor> {
        let tensor = match &self.data {
            TensorData::F32(v) => TractTensor::from_shape(&self.shape, v),
            TensorData::I64(v) => TractTensor::from_shape(&self.shape, v),
            TensorData::I32(v) => TractTensor::from_shape(&self.shape, v),
            TensorData::U8(v) => TractTensor::from_shape(&self.shape, v),
        };
        tensor.map_err(|e| MlError::new("ml.tensor.convert", e.to_string()))
    }
    pub(crate) fn from_tract(tensor: &TractTensor) -> Result<Self> {
        let shape = tensor.shape().to_vec();
        let unsupported = || {
            MlError::new(
                "ml.tensor.dtype",
                format!("unsupported output type {:?}", tensor.datum_type()),
            )
        };
        let convert = |e: tract_onnx::prelude::TractError| {
            MlError::new("ml.tensor.convert", e.to_string())
        };
        let data = match DType::from_tract(tensor.datum_type()).ok_or_else(unsupported)? {
            DType::F32 => TensorData::F32(tensor.as_slice::<f32>().map_err(convert)?.to_vec()),
            DType::I64 => TensorData::I64(tensor.as_slice::<i64>().map_err(convert)?.to_vec()),
            DType::I32 => TensorData::I32(tensor.as_slice::<i32>().map_err(convert)?.to_vec()),
            DType::U8 => TensorData::U8(tensor.as_slice::<u8>().map_err(convert)?.to_vec()),
        };
        // Model outputs may legitimately contain NaN/inf; keep them rather than failing.
        Ok(Self { shape, data })
    }
}

pub(crate) fn element_count(shape: &[usize]) -> Result<usize> {
    shape
        .iter()
        .try_fold(1usize, |acc, d| acc.checked_mul(*d))
        .ok_or_else(|| MlError::new("ml.tensor.shape", "tensor element count overflows"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_mismatched_shape_and_non_finite_values() {
        assert_eq!(
            Tensor::f32(vec![2, 2], vec![1.0; 3]).unwrap_err().code,
            "ml.tensor.shape"
        );
        assert_eq!(
            Tensor::f32(vec![1], vec![f32::NAN]).unwrap_err().code,
            "ml.tensor.non_finite"
        );
        assert!(Tensor::f32(vec![usize::MAX, 2], vec![]).is_err());
    }

    #[test]
    fn rgba_image_becomes_planar_normalised_nchw() {
        let rgba = [255, 0, 51, 9, 0, 255, 102, 9];
        let tensor = Tensor::from_rgba8_nchw(2, 1, &rgba).unwrap();
        assert_eq!(tensor.shape(), &[1, 3, 1, 2]);
        assert_eq!(tensor.as_f32().unwrap(), &[1.0, 0.0, 0.0, 1.0, 0.2, 0.4]);
        assert!(Tensor::from_rgba8_nchw(2, 2, &rgba).is_err());
    }

    #[test]
    fn vec3_rows_and_argmax() {
        let tensor = Tensor::from_vec3s(&[[1.0, 2.0, 3.0], [9.0, -1.0, 0.0]]).unwrap();
        assert_eq!(tensor.shape(), &[2, 3]);
        assert_eq!(tensor.argmax(), Some(3));
        assert_eq!(Tensor::row(&[0.5, 0.5]).unwrap().argmax(), Some(0));
    }

    #[test]
    fn serde_shape_is_tagged_and_closed() {
        let tensor = Tensor::row(&[1.5]).unwrap();
        let json = serde_json::to_string(&tensor).unwrap();
        assert_eq!(json, r#"{"shape":[1,1],"dtype":"f32","values":[1.5]}"#);
        let back: Tensor = serde_json::from_str(&json).unwrap();
        assert_eq!(back, tensor);
        assert!(serde_json::from_str::<Tensor>(r#"{"shape":[1],"dtype":"f64","values":[1]}"#).is_err());
        assert!(serde_json::from_str::<Tensor>(r#"{"shape":[2],"dtype":"f32","values":[1]}"#).is_err());
    }
}
