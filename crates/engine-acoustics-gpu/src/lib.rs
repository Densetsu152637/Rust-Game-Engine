//! GPU propagation backends for `engine-acoustics`
//! (docs/spatial-audio-design.md sections 6 and 7.2).
//!
//! Two tiers implement [`engine_acoustics::PropagationBackend`] with the same
//! stochastic energy model as the CPU tracer:
//!
//! - **Tier 1, [`GpuBackend::ray_query`]:** hardware ray query through wgpu's
//!   `EXPERIMENTAL_RAY_QUERY` (one BLAS per mesh, one TLAS of instances), enabled
//!   on Vulkan adapters only.
//! - **Tier 2, [`GpuBackend::compute`]:** GPU-accelerated traversal of the
//!   world's flat BVHs in a compute shader. This is **not** hardware ray tracing.
//!
//! A tier is usable only after `initialize` opened its own device and passed a
//! self-test (exact hits on a transformed quad, and a shoebox mean free path
//! within 2% of 4V/S). [`select_backend`] applies the preference order
//! `ray query -> compute -> CPU` and reports why better tiers were skipped.
//!
//! Histograms are accumulated with integer atomics in 64-bit fixed point, so a
//! fixed seed gives bitwise-identical results on the same device and driver.
//!
//! This crate opts out of the workspace `unsafe_code = "forbid"` table (as
//! `engine-dlss` does) and denies unsafe code instead. Its only `unsafe`
//! expression is the experimental-feature token in `device.rs`.

mod backend;
mod capability;
mod device;
mod error;
mod kernel;
mod scene;
mod select;
mod selftest;
mod upload;

pub use backend::GpuBackend;
pub use capability::probe_capabilities;
pub use device::wgpu_instances_created;
pub use select::{MAX_REASON_BYTES, TierSelection, bounded_reason, select_backend, tier_order};
pub use selftest::SelfTestReport;

/// Adapter selection and test hooks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuOptions {
    /// Case-insensitive substring of the adapter name.
    pub adapter_name_contains: Option<String>,
    /// Readback wait per trace; a timeout is `acoustics.backend.timeout`
    /// (non-fatal).
    pub readback_timeout_ms: u32,
    /// Test hook: behave as if no adapter exposed `EXPERIMENTAL_RAY_QUERY`.
    pub deny_ray_query: bool,
    /// Test hook: fail the self-test at its first step.
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

/// Graphics APIs a [`GpuBackend`] may open (addition to section 7.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GraphicsApi {
    /// Vulkan for tier 1; Vulkan, DX12 and Metal for tier 2.
    #[default]
    Primary,
    Vulkan,
    Dx12,
    Metal,
}
impl GraphicsApi {
    pub(crate) fn backends(self) -> wgpu::Backends {
        match self {
            Self::Primary => wgpu::Backends::PRIMARY,
            Self::Vulkan => wgpu::Backends::VULKAN,
            Self::Dx12 => wgpu::Backends::DX12,
            Self::Metal => wgpu::Backends::METAL,
        }
    }
}

/// The GPU tiers in preference order, for `ServiceConfig::backends` (the CPU
/// tier is appended by the service).
pub fn gpu_backends(options: &GpuOptions) -> Vec<Box<dyn engine_acoustics::PropagationBackend>> {
    vec![
        Box::new(GpuBackend::ray_query(options.clone())),
        Box::new(GpuBackend::compute(options.clone())),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_acoustics::{BackendKind, PropagationBackend};

    #[test]
    fn uninitialized_backends_claim_nothing() {
        for backend in [
            GpuBackend::ray_query(GpuOptions::default()),
            GpuBackend::compute(GpuOptions::default()),
        ] {
            let info = backend.info();
            assert!(!info.hardware_rt && info.adapter.is_none() && info.driver.is_none());
            assert!(backend.self_test_report().is_none());
        }
        assert_eq!(
            GpuBackend::ray_query(GpuOptions::default()).info().kind,
            BackendKind::GpuRayQuery
        );
        assert!(
            GpuBackend::compute(GpuOptions::default())
                .description()
                .contains("not hardware ray tracing")
        );
    }

    #[test]
    fn denied_ray_query_fails_before_touching_wgpu() {
        let before = wgpu_instances_created();
        let mut backend = GpuBackend::ray_query(GpuOptions {
            deny_ray_query: true,
            ..Default::default()
        });
        let e = backend.initialize().unwrap_err();
        assert_eq!(e.code, "acoustics.backend.unavailable");
        assert!(!e.fatal);
        assert!(
            e.message.contains("EXPERIMENTAL_RAY_QUERY"),
            "{}",
            e.message
        );
        assert_eq!(wgpu_instances_created(), before);
        assert!(!backend.info().hardware_rt);
    }

    #[test]
    fn reasons_are_bounded_on_char_boundaries() {
        let long = "é".repeat(400);
        let bounded = bounded_reason(&long);
        assert!(bounded.len() <= MAX_REASON_BYTES);
        assert!(bounded.len() >= MAX_REASON_BYTES - 1);
        assert_eq!(bounded_reason("short"), "short");
    }

    #[test]
    fn tier_order_follows_the_preference() {
        use engine_acoustics::BackendPreference as P;
        assert_eq!(
            tier_order(P::Auto),
            [
                BackendKind::GpuRayQuery,
                BackendKind::GpuCompute,
                BackendKind::Cpu
            ]
        );
        assert_eq!(tier_order(P::GpuRayQuery), tier_order(P::Auto));
        assert_eq!(
            tier_order(P::GpuCompute),
            [BackendKind::GpuCompute, BackendKind::Cpu]
        );
        assert_eq!(tier_order(P::Cpu), [BackendKind::Cpu]);
    }

    #[test]
    fn both_shader_variants_validate_with_naga() {
        // Parses and validates the composed WGSL for both tiers without a GPU,
        // so a shader error fails `cargo test` on any machine.
        for tier in [device::Tier::Compute, device::Tier::RayQuery] {
            let source = kernel::shader_source(tier);
            let module = naga::front::wgsl::parse_str(&source)
                .unwrap_or_else(|e| panic!("{tier:?}: {}", e.emit_to_string(&source)));
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .unwrap_or_else(|e| panic!("{tier:?}: {e:?}"));
        }
    }
}
