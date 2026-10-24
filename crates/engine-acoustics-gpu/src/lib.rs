//! GPU propagation backends for `engine-acoustics` (contract skeleton).
//!
//! No wgpu code exists yet: every operation reports `acoustics.backend.unavailable`
//! rather than pretending a GPU tier works. This crate opts out of the workspace
//! `unsafe_code = "forbid"` table in `Cargo.toml` (as `engine-dlss` does) and denies
//! unsafe code instead; the only place the lint may be lifted is the single audited
//! function that will create wgpu's `ExperimentalFeatures::enabled()` token.

use engine_acoustics::{
    AcousticWorld, BackendError, BackendInfo, BackendKind, EnergyHistogram, EnergyJob,
    PropagationBackend, TraceSettings, TraceStats,
};

/// Adapter selection and test hooks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuOptions {
    pub adapter_name_contains: Option<String>,
    pub readback_timeout_ms: u32,
    /// Test hook: pretend `EXPERIMENTAL_RAY_QUERY` is unavailable.
    pub deny_ray_query: bool,
    /// Test hook: force the self-test to fail.
    #[doc(hidden)]
    pub force_self_test_failure: bool,
}
impl Default for GpuOptions {
    fn default() -> Self {
        Self {
            adapter_name_contains: None,
            readback_timeout_ms: 100,
            deny_ray_query: false,
            force_self_test_failure: false,
        }
    }
}

/// One adapter and whether it can run the ray-query tier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterCapability {
    pub name: String,
    pub backend: String,
    pub ray_query: bool,
    pub reason: Option<String>,
}

fn unavailable(what: &str) -> BackendError {
    BackendError {
        code: "acoustics.backend.unavailable",
        message: format!("not implemented: {what}"),
        fatal: false,
    }
}

/// Deviation from section 7.2: returns `Result` so the unimplemented state is not
/// reported as "no adapters".
pub fn probe_capabilities() -> Result<Vec<AdapterCapability>, BackendError> {
    Err(unavailable("probe_capabilities"))
}

/// Tier 1 (`ray_query`) or tier 2 (`compute`) backend.
#[derive(Clone, Debug)]
pub struct GpuBackend {
    kind: BackendKind,
    options: GpuOptions,
}
impl GpuBackend {
    /// Hardware ray-query tier.
    pub fn ray_query(options: GpuOptions) -> Self {
        Self {
            kind: BackendKind::GpuRayQuery,
            options,
        }
    }
    /// GPU compute BVH traversal tier (not hardware ray tracing).
    pub fn compute(options: GpuOptions) -> Self {
        Self {
            kind: BackendKind::GpuCompute,
            options,
        }
    }
    pub fn options(&self) -> &GpuOptions {
        &self.options
    }
}
impl PropagationBackend for GpuBackend {
    fn info(&self) -> BackendInfo {
        // Hardware RT is only claimed after a verified self-test, which cannot
        // have happened yet.
        BackendInfo {
            kind: self.kind,
            hardware_rt: false,
            adapter: None,
            driver: None,
        }
    }
    fn initialize(&mut self) -> Result<(), BackendError> {
        Err(unavailable("GpuBackend::initialize"))
    }
    fn sync_world(&mut self, _world: &AcousticWorld) -> Result<(), BackendError> {
        Err(unavailable("GpuBackend::sync_world"))
    }
    fn trace(
        &mut self,
        _world: &AcousticWorld,
        _settings: &TraceSettings,
        _jobs: &[EnergyJob],
        _out: &mut Vec<EnergyHistogram>,
    ) -> Result<TraceStats, BackendError> {
        Err(unavailable("GpuBackend::trace"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_reports_unavailable_without_claiming_hardware() {
        for mut backend in [
            GpuBackend::ray_query(GpuOptions::default()),
            GpuBackend::compute(GpuOptions::default()),
        ] {
            let info = backend.info();
            assert!(!info.hardware_rt && info.adapter.is_none());
            let error = backend.initialize().unwrap_err();
            assert_eq!(error.code, "acoustics.backend.unavailable");
            assert!(error.message.contains("not implemented"));
        }
        assert_eq!(
            GpuBackend::ray_query(GpuOptions::default()).info().kind,
            BackendKind::GpuRayQuery
        );
        assert!(probe_capabilities().is_err());
    }
}
