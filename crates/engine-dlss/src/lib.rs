//! DLSS capabilities are separate features, never inferred from a marketing version.
//! The optional native adapter performs NGX super resolution and frame generation
//! (generation only; presenting generated frames is the renderer's job, see
//! docs/dlss.md). Ordinary builds need no
//! NVIDIA SDK and must continue using their normal render path.

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
        Feature::FrameGeneration if cfg!(ngx_available) => Ok(()),
        Feature::FrameGeneration => Err(Error::Unavailable("NGX DLSS Frame Generation adapter not built; enable ngx-vulkan with NGX_SDK_DIR and VULKAN_HEADERS_DIR on Windows/Linux. Generated frames still need the renderer's presentation integration".into())),
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

/// NGX project IDs are GUID-shaped (`8-4-4-4-12` hexadecimal digits).
pub fn is_project_id(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, n)| g.len() == n && g.bytes().all(|b| b.is_ascii_hexdigit()))
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

mod framegen;
#[cfg(ngx_available)]
pub use framegen::FrameGenerator;
#[cfg(feature = "ngx-vulkan")]
pub use framegen::GenerationInputs;
pub use framegen::{
    Camera, ColorFormat, FrameGenerationOptions, FrameGenerationStatus, Generated, GenerationFrame,
    Interpolation,
};
#[cfg(all(feature = "ngx-vulkan", not(ngx_available)))]
pub use unavailable::FrameGenerator;

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
        for feature in [Feature::RayReconstruction, Feature::NeuralRendering] {
            assert!(build_support(feature).is_err());
        }
    }
    #[test]
    fn project_ids_must_be_guid_shaped() {
        assert!(is_project_id("c8091997-e7fa-48f6-8ef7-bf2211936fb5"));
        assert!(is_project_id("C8091997-E7FA-48F6-8EF7-BF2211936FB5"));
        for bad in [
            "",
            "test",
            "c8091997e7fa48f68ef7bf2211936fb5",
            "c8091997-e7fa-48f6-8ef7-bf2211936fb",
            "g8091997-e7fa-48f6-8ef7-bf2211936fb5",
            "c8091997-e7fa-48f6-8ef7-bf2211936fb5-00",
        ] {
            assert!(!is_project_id(bad), "{bad}");
        }
    }
    #[test]
    fn frame_generation_support_follows_the_native_build() {
        let support = build_support(Feature::FrameGeneration);
        if cfg!(ngx_available) {
            assert!(support.is_ok());
        } else {
            let Err(Error::Unavailable(message)) = support else {
                panic!("expected an unavailable FG build: {support:?}");
            };
            assert!(message.contains("Frame Generation adapter not built"));
            assert!(message.contains("NGX_SDK_DIR"));
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
