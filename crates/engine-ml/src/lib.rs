//! ONNX model loading and bounded CPU inference for the engine.
//!
//! * [`Model`] parses an ONNX protobuf with the pure-Rust `tract` runtime after a bounded
//!   pre-validation pass (size, node count, no external data, no training graphs), fixes
//!   every input to a concrete shape, optimises it and checks every intermediate tensor
//!   against [`ModelLimits::max_tensor_elements`].
//! * [`Tensor`] is the engine-facing tensor type with helpers that build tensors from
//!   engine data (vectors, RGBA images).
//! * [`InferenceRuntime`] runs jobs on worker threads with a bounded queue, so ECS systems
//!   submit work and poll completions without blocking a frame.
//! * [`BackendStatus`] reports which execution backend is in use. Only the CPU backend
//!   exists; GPU execution is reported as unavailable with its reason.
mod error;
pub mod fixtures;
mod model;
mod runtime;
mod tensor;

pub use error::{MlError, Result};
pub use model::{
    DimInfo, LoadOptions, Model, ModelInfo, ModelLimits, TensorInfo, inspect, inspect_path,
};
pub use runtime::{
    BackendStatus, Completion, InferenceRuntime, JobId, RuntimeConfig, backend_status,
};
pub use tensor::{DType, Tensor, TensorData};
