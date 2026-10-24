//! Native authoring editor. The controller is platform-independent; Win32 FFI is
//! confined to the native module. Tooling injects a compiler-backed `PlayHost`.

mod controller;
#[cfg(windows)]
mod native;
mod portable;

pub use controller::{EditorController, PlayHost, PlayStatus};
pub use engine_core::Result;
pub use portable::ViewNavigation;

/// Opens the portable desktop editor on the calling UI thread.
pub fn run(manifest: impl AsRef<std::path::Path>, host: Box<dyn PlayHost>) -> Result<()> {
    portable::run(manifest.as_ref(), host)
}

/// Native controls verified by the bounded smoke test. This is structural UI
/// evidence, not a claim about subjective layout quality or physical input.
#[derive(Debug, Clone)]
pub struct NativeSmokeReport {
    pub controls_created: usize,
    pub entities_listed: usize,
    pub picker_edit_applied: bool,
    pub window_closed: bool,
}

/// Creates a real native window, exercises hierarchy/inspector/picker command
/// paths, and closes it automatically. The smoke does not save authored edits.
pub fn native_smoke(
    manifest: impl AsRef<std::path::Path>,
    host: Box<dyn PlayHost>,
) -> Result<NativeSmokeReport> {
    #[cfg(windows)]
    {
        native::run(
            manifest.as_ref(),
            host,
            Some(std::time::Duration::from_millis(150)),
        )
    }
    #[cfg(not(windows))]
    {
        let _ = (manifest, host);
        engine_core::anyhow::bail!("native editor smoke is supported on Windows only")
    }
}
