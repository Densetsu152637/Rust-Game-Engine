//! DLSS capabilities are separate features, never inferred from a marketing version.
//! The optional native adapter performs NGX super resolution only. Ordinary builds
//! need no NVIDIA SDK and must continue using their normal render path.

use std::{fmt, path::PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    SuperResolution,
    FrameGeneration,
    RayReconstruction,
    NeuralRendering,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    Performance,
    Balanced,
    Quality,
    UltraPerformance,
    Dlaa,
}

impl Quality {
    #[cfg(ngx_available)]
    fn native(self) -> i32 {
        match self {
            Self::Performance => 0,
            Self::Balanced => 1,
            Self::Quality => 2,
            Self::UltraPerformance => 3,
            Self::Dlaa => 5,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Unavailable(String),
    InvalidInput(String),
    Native { operation: &'static str, code: u32 },
    Gpu(String),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(s) | Self::InvalidInput(s) | Self::Gpu(s) => f.write_str(s),
            Self::Native { operation, code } => write!(f, "NGX {operation} failed (0x{code:08x})"),
        }
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

/// Build availability only; hardware/driver/plugin support is checked by Session::new.
pub fn build_support(feature: Feature) -> Result<()> {
    match feature {
        Feature::SuperResolution if cfg!(ngx_available) => Ok(()),
        Feature::SuperResolution => Err(Error::Unavailable("NGX Vulkan adapter not built; enable ngx-vulkan with NGX_SDK_DIR and VULKAN_HEADERS_DIR on Windows/Linux".into())),
        Feature::FrameGeneration => Err(Error::Unavailable("frame generation requires a separate presentation integration; this adapter does not implement it".into())),
        Feature::RayReconstruction => Err(Error::Unavailable("ray reconstruction requires ray-traced lighting inputs and a separate NGX integration".into())),
        Feature::NeuralRendering => Err(Error::Unavailable("DLSS 5 neural rendering has no ABI in the inspected public SDK v310.9.16; NVIDIA developer SDK/runtime/model access is required".into())),
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Frame {
    /// Projection jitter in render pixels. Motion vectors exclude jitter.
    pub jitter: [f32; 2],
    /// Multiply stored motion components by these values to obtain pixel motion.
    pub motion_scale: [f32; 2],
    pub reset: bool,
    pub delta_ms: f32,
}
impl Default for Frame {
    fn default() -> Self {
        Self {
            jitter: [0.; 2],
            motion_scale: [1.; 2],
            reset: true,
            delta_ms: 16.6667,
        }
    }
}
impl Frame {
    pub fn validate(&self) -> Result<()> {
        if !self
            .jitter
            .into_iter()
            .chain(self.motion_scale)
            .chain([self.delta_ms])
            .all(f32::is_finite)
            || self.delta_ms <= 0.
            || self.motion_scale.contains(&0.)
        {
            return Err(Error::InvalidInput(
                "DLSS frame requires finite jitter, nonzero motion scale, and positive frame time"
                    .into(),
            ));
        }
        Ok(())
    }
}

/// Runtime and log paths must be explicit absolute directories. The runtime
/// directory must contain the official NVIDIA DLSS SR plugin.
#[derive(Clone, Debug)]
pub struct SessionOptions {
    pub project_id: String,
    pub runtime_dir: PathBuf,
    pub log_dir: PathBuf,
    pub output_size: [u32; 2],
    pub quality: Quality,
}
#[derive(Clone, Debug)]
pub struct Status {
    pub device_name: String,
    pub driver_version: u32,
    pub minimum_driver: [u32; 2],
    pub sdk_api_version: u32,
    /// None means the plugin version was not queried; no marketing version is inferred.
    pub runtime_version: Option<String>,
    pub render_size: [u32; 2],
    pub output_size: [u32; 2],
    pub evaluated_frames: u64,
}

#[cfg(ngx_available)]
mod native;
#[cfg(ngx_available)]
pub use native::{Context, Session, create_device, create_device_for_features};
#[cfg(all(feature = "ngx-vulkan", not(ngx_available)))]
mod unavailable;
#[cfg(all(feature = "ngx-vulkan", not(ngx_available)))]
pub use unavailable::{Context, Session, create_device, create_device_for_features};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn version_does_not_enable_unimplemented_features() {
        for feature in [
            Feature::FrameGeneration,
            Feature::RayReconstruction,
            Feature::NeuralRendering,
        ] {
            assert!(build_support(feature).is_err());
        }
    }
    #[test]
    fn invalid_temporal_inputs_are_rejected() {
        assert!(Frame::default().validate().is_ok());
        assert!(
            Frame {
                jitter: [f32::NAN, 0.],
                ..Frame::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Frame {
                delta_ms: 0.,
                ..Frame::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Frame {
                motion_scale: [0., 1.],
                ..Frame::default()
            }
            .validate()
            .is_err()
        );
    }
}
