//! HDR post-processing: tonemapping, exposure (manual and histogram
//! auto-exposure), bloom, HDR colour grading with an optional 3D LUT,
//! vignette and film grain. See docs/rendering-post.md.
//!
//! A host sets [`crate::ViewData::post`]; the pipeline then runs
//! `engine.post` (PostProcess stage) and `engine.tonemap` (Tonemap stage,
//! superseding `engine.output`). Views without settings are unchanged.

mod gpu;
pub mod lut;
pub mod settings;
pub mod tonemap;

pub(crate) use gpu::{PostShared, install};
pub use lut::Lut3d;
pub use settings::{
    BloomSettings, ExposureMode, ExposureSettings, GradingSettings, GrainSettings, PostSettings,
    TonemapOperator, VignetteSettings, white_balance,
};
