//! Analytic reverberation estimates (section 5.4.5).
//!
//! Sabine: `T = 0.161 V / (sum S_i a_i + 4 m V)`; Eyring:
//! `T = 0.161 V / (-S ln(1 - a_mean) + 4 m V)` with `a_mean = sum S_i a_i / S`
//! (Kuttruff, *Room Acoustics*, 5th ed., 2009, chapter 5). Here `m` is the *energy*
//! attenuation coefficient of air, which is `2 * air_np_per_m` because the inputs
//! are amplitude attenuations in nepers per metre (the unit the mixer applies as
//! `exp(-m d)` to amplitude).
//!
//! These keep the `Result` return of the Stream 0 skeleton (a documented deviation
//! from the plain return types in section 7.2): invalid volumes, areas or
//! coefficients are rejected with `acoustics.request.invalid` instead of producing a
//! meaningless number. A room without absorption returns infinite times.

use crate::{AcousticsError, BANDS, BandGains, Result};

/// The Sabine constant (about `24 ln 10 / 343`), in s/m, as used in the literature.
pub const SABINE_CONSTANT: f32 = 0.161;

fn invalid(message: &str) -> AcousticsError {
    AcousticsError::new("acoustics.request.invalid", message)
}

fn check(volume_m3: f32, surfaces: &[(f32, BandGains)], air: BandGains) -> Result<f64> {
    if !(volume_m3.is_finite() && volume_m3 > 0.0) {
        return Err(invalid("volume must be finite and positive"));
    }
    let mut area = 0.0f64;
    for (s, a) in surfaces {
        if !(s.is_finite() && *s >= 0.0) {
            return Err(invalid("surface areas must be finite and >= 0"));
        }
        if !a.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) {
            return Err(invalid("absorption must be finite and in [0, 1]"));
        }
        area += f64::from(*s);
    }
    if !air.iter().all(|m| m.is_finite() && *m >= 0.0) {
        return Err(invalid("air absorption must be finite and >= 0"));
    }
    Ok(area)
}

fn absorbed(surfaces: &[(f32, BandGains)], band: usize) -> f64 {
    surfaces
        .iter()
        .map(|(s, a)| f64::from(*s) * f64::from(a[band]))
        .sum()
}

/// Sabine RT60 per band. `surfaces` is `(area m^2, absorption)`.
pub fn sabine_rt60(
    volume_m3: f32,
    surfaces: &[(f32, BandGains)],
    air_np_per_m: BandGains,
) -> Result<BandGains> {
    check(volume_m3, surfaces, air_np_per_m)?;
    let v = f64::from(volume_m3);
    let mut out = [0.0; BANDS];
    for (b, t) in out.iter_mut().enumerate() {
        let a = absorbed(surfaces, b) + 8.0 * f64::from(air_np_per_m[b]) * v;
        *t = (f64::from(SABINE_CONSTANT) * v / a) as f32;
    }
    Ok(out)
}

/// Eyring RT60 per band.
pub fn eyring_rt60(
    volume_m3: f32,
    surfaces: &[(f32, BandGains)],
    air_np_per_m: BandGains,
) -> Result<BandGains> {
    let area = check(volume_m3, surfaces, air_np_per_m)?;
    if area <= 0.0 {
        return Err(invalid("Eyring needs a positive total surface area"));
    }
    let v = f64::from(volume_m3);
    let mut out = [0.0; BANDS];
    for (b, t) in out.iter_mut().enumerate() {
        let mean = absorbed(surfaces, b) / area;
        let a = -area * (1.0 - mean).ln() + 8.0 * f64::from(air_np_per_m[b]) * v;
        *t = (f64::from(SABINE_CONSTANT) * v / a) as f32;
    }
    Ok(out)
}
