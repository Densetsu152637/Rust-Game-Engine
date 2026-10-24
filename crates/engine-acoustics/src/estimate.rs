use crate::{BandGains, EnergyHistogram, Result, unimplemented};
use engine_audio::acoustic::{ReverbParams, StereoImpulseResponse};

// Deviation from section 7.2: these return `Result` so the unimplemented state is
// reported instead of a fabricated value.

/// Sabine RT60 per band. `surfaces` is `(area, absorption)`.
pub fn sabine_rt60(
    _volume_m3: f32,
    _surfaces: &[(f32, BandGains)],
    _air_np_per_m: BandGains,
) -> Result<BandGains> {
    Err(unimplemented("sabine_rt60"))
}
/// Eyring RT60 per band.
pub fn eyring_rt60(
    _volume_m3: f32,
    _surfaces: &[(f32, BandGains)],
    _air_np_per_m: BandGains,
) -> Result<BandGains> {
    Err(unimplemented("eyring_rt60"))
}
/// Schroeder fit; T30, then T20, then EDT.
pub fn fit_reverb(_h: &EnergyHistogram, _late_start_s: f32) -> Result<ReverbParams> {
    Err(unimplemented("fit_reverb"))
}
pub fn synthesize_ir(
    _h: &EnergyHistogram,
    _sample_rate: u32,
    _seed: u64,
) -> Result<StereoImpulseResponse> {
    Err(unimplemented("synthesize_ir"))
}
