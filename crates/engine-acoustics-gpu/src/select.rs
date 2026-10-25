//! Explicit tier selection with self-tests and bounded fallback reasons.

use crate::{GpuBackend, GpuOptions, GraphicsApi, error};
use engine_acoustics::{
    AcousticState, BackendError, BackendKind, BackendPreference, PropagationBackend,
};

/// Fallback reasons are bounded like `AcousticStatus::fallback_reason`.
pub const MAX_REASON_BYTES: usize = 512;

/// Truncates `text` to at most [`MAX_REASON_BYTES`] on a char boundary.
pub fn bounded_reason(text: &str) -> String {
    if text.len() <= MAX_REASON_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_REASON_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// The tiers tried for `preference`, best first. `Auto` and `GpuRayQuery` start
/// at hardware ray query; every preference ends at the CPU.
pub fn tier_order(preference: BackendPreference) -> &'static [BackendKind] {
    match preference {
        BackendPreference::Auto | BackendPreference::GpuRayQuery => &[
            BackendKind::GpuRayQuery,
            BackendKind::GpuCompute,
            BackendKind::Cpu,
        ],
        BackendPreference::GpuCompute => &[BackendKind::GpuCompute, BackendKind::Cpu],
        BackendPreference::Cpu => &[BackendKind::Cpu],
    }
}

/// The outcome of [`select_backend`]: an initialized backend and its status.
pub struct TierSelection {
    pub backend: Box<dyn PropagationBackend>,
    pub requested: BackendPreference,
    pub active: BackendKind,
    /// `Active` when the first tier of [`tier_order`] passed its self-test,
    /// otherwise `Fallback`.
    pub state: AcousticState,
    /// Why each better tier was skipped (`<tier>: <reason>; ...`), at most
    /// [`MAX_REASON_BYTES`].
    pub fallback_reason: Option<String>,
    pub adapter: Option<String>,
    pub hardware_rt: bool,
}

impl std::fmt::Debug for TierSelection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TierSelection")
            .field("requested", &self.requested)
            .field("active", &self.active)
            .field("state", &self.state)
            .field("fallback_reason", &self.fallback_reason)
            .field("adapter", &self.adapter)
            .field("hardware_rt", &self.hardware_rt)
            .finish()
    }
}

/// Tries the tiers of [`tier_order`] in order and returns the first whose
/// `initialize` (capability probe plus self-test) succeeds. `cpu` is the CPU
/// tier from engine-acoustics; a `Cpu` preference never creates a wgpu
/// instance. Fails with `acoustics.backend.unavailable` when no tier works,
/// listing every reason.
pub fn select_backend(
    preference: BackendPreference,
    options: &GpuOptions,
    api: GraphicsApi,
    cpu: Box<dyn PropagationBackend>,
) -> Result<TierSelection, BackendError> {
    let mut reasons: Vec<String> = Vec::new();
    let mut cpu = Some(cpu);
    for (attempt, kind) in tier_order(preference).iter().enumerate() {
        let mut backend: Box<dyn PropagationBackend> = match kind {
            BackendKind::GpuRayQuery => {
                Box::new(GpuBackend::ray_query(options.clone()).with_api(api))
            }
            BackendKind::GpuCompute => Box::new(GpuBackend::compute(options.clone()).with_api(api)),
            BackendKind::Cpu => cpu.take().expect("CPU is tried once, last"),
        };
        match backend.initialize() {
            Ok(()) => {
                let info = backend.info();
                let fallback_reason =
                    (!reasons.is_empty()).then(|| bounded_reason(&reasons.join("; ")));
                return Ok(TierSelection {
                    backend,
                    requested: preference,
                    active: *kind,
                    state: if attempt == 0 {
                        AcousticState::Active
                    } else {
                        AcousticState::Fallback
                    },
                    fallback_reason,
                    adapter: info.adapter,
                    hardware_rt: info.hardware_rt,
                });
            }
            Err(e) => {
                let name = format!("{kind:?}");
                // "<tier>: <reason>", without repeating a leading tier name.
                let message = e
                    .message
                    .strip_prefix(&format!("{name}: "))
                    .or_else(|| e.message.strip_prefix(&format!("{name} ")))
                    .unwrap_or(&e.message);
                reasons.push(format!("{name}: {message}"));
            }
        }
    }
    Err(error::unavailable(bounded_reason(&format!(
        "no propagation tier is available: {}",
        reasons.join("; ")
    ))))
}
