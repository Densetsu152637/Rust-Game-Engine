//! Schroeder backward integration and reverberation-time fits (section 5.4.5;
//! ISO 3382-1:2009 evaluation ranges: T30 from -5 to -35 dB, T20 from -5 to
//! -25 dB, EDT from 0 to -10 dB, each extrapolated to 60 dB by least squares).

use crate::{AcousticsError, BANDS, EnergyHistogram, Result};
use engine_audio::acoustic::ReverbParams;

/// Which evaluation range produced a band's RT60.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecayFit {
    T30,
    T20,
    Edt,
    /// The decay never fell 10 dB inside the histogram (or there was no energy).
    None,
}

/// Per-band detail behind [`fit_reverb`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandFit {
    pub rt60_s: f32,
    pub method: DecayFit,
    /// Total deposited energy relative to the direct sound at 1 m.
    pub energy: f32,
    /// Energy at or after `late_start_s`, same reference.
    pub late_energy: f32,
}

/// Checks the shape and values of a histogram from any backend. A non-finite or
/// negative value is a backend failure (`acoustics.backend.failed`).
pub fn validate_histogram(h: &EnergyHistogram) -> Result<()> {
    let fail = |m: &str| {
        Err(AcousticsError::new(
            "acoustics.backend.failed",
            m.to_owned(),
        ))
    };
    if !(h.bin_s.is_finite() && h.bin_s > 0.0) || h.bins.is_empty() {
        return fail("histogram needs a positive bin width and at least one bin");
    }
    if h.bins
        .iter()
        .any(|b| b.iter().any(|v| !v.is_finite() || *v < 0.0))
    {
        return fail("histogram has a negative or non-finite bin");
    }
    if !h.escaped.is_finite() || !h.mean_free_path_m.is_finite() {
        return fail("histogram summary values must be finite");
    }
    Ok(())
}

/// Fits every band; see [`fit_reverb`].
pub fn fit_bands(h: &EnergyHistogram, late_start_s: f32) -> Result<[BandFit; BANDS]> {
    validate_histogram(h)?;
    if !(late_start_s.is_finite() && late_start_s >= 0.0) {
        return Err(AcousticsError::new(
            "acoustics.request.invalid",
            "late_start_s must be finite and >= 0",
        ));
    }
    let late_bin = (late_start_s / h.bin_s).ceil() as usize;
    let mut out = [BandFit {
        rt60_s: 0.0,
        method: DecayFit::None,
        energy: 0.0,
        late_energy: 0.0,
    }; BANDS];
    let mut schroeder = vec![0.0f64; h.bins.len()];
    for (b, fit) in out.iter_mut().enumerate() {
        let mut acc = 0.0f64;
        for i in (0..h.bins.len()).rev() {
            acc += f64::from(h.bins[i][b]);
            schroeder[i] = acc;
        }
        let total = acc;
        fit.energy = total as f32;
        fit.late_energy = schroeder.get(late_bin).copied().unwrap_or(0.0) as f32;
        if total <= 0.0 {
            continue;
        }
        let level: Vec<f64> = schroeder
            .iter()
            .map(|&s| {
                if s > 0.0 {
                    10.0 * (s / total).log10()
                } else {
                    f64::NEG_INFINITY
                }
            })
            .collect();
        for (method, hi, lo) in [
            (DecayFit::T30, -5.0, -35.0),
            (DecayFit::T20, -5.0, -25.0),
            (DecayFit::Edt, 0.0, -10.0),
        ] {
            if let Some(rt) = regress(&level, h.bin_s, hi, lo) {
                fit.rt60_s = rt as f32;
                fit.method = method;
                break;
            }
        }
    }
    Ok(out)
}

/// Least-squares decay over the points with `lo <= L <= hi`, requiring the curve to
/// fall below `lo` inside the histogram. Returns the time for 60 dB.
fn regress(level: &[f64], bin_s: f32, hi: f64, lo: f64) -> Option<f64> {
    if !level.iter().any(|&l| l < lo) {
        return None;
    }
    let (mut n, mut sx, mut sy, mut sxx, mut sxy) = (0.0f64, 0.0, 0.0, 0.0, 0.0);
    for (i, &l) in level.iter().enumerate() {
        if l > hi {
            continue;
        }
        if l < lo {
            break;
        }
        let x = i as f64 * f64::from(bin_s);
        n += 1.0;
        sx += x;
        sy += l;
        sxx += x * x;
        sxy += x * l;
    }
    if n < 3.0 {
        return None;
    }
    let slope = (n * sxy - sx * sy) / (n * sxx - sx * sx);
    (slope < 0.0 && slope.is_finite()).then(|| -60.0 / slope)
}

/// Schroeder fit of a late-field histogram into mixer reverb parameters.
///
/// - `rt60_s`: T30, else T20, else EDT per band (0 for a band without decay).
/// - `level`: the late energy (at or after `late_start_s`) relative to the direct
///   sound at 1 m, as an energy. That is the mixer's convention for
///   `ReverbParams::level` (stream A divides it by the FDN's own energy), so an
///   amplitude here would make the late field quiet by the square root.
/// - `predelay_s`: start time of the first non-empty bin.
/// - `mean_free_path_m`: copied from the histogram.
/// - `enabled`: at least one band produced an RT60.
///
/// Keeps the `Result` return of the Stream 0 skeleton: a malformed histogram is an
/// error, an empty one gives disabled parameters.
pub fn fit_reverb(h: &EnergyHistogram, late_start_s: f32) -> Result<ReverbParams> {
    let bands = fit_bands(h, late_start_s)?;
    let predelay = h
        .bins
        .iter()
        .position(|b| b.iter().any(|v| *v > 0.0))
        .map_or(0.0, |i| i as f32 * h.bin_s);
    Ok(ReverbParams {
        enabled: bands.iter().any(|b| b.method != DecayFit::None),
        rt60_s: bands.map(|b| b.rt60_s),
        level: bands.map(|b| b.late_energy.max(0.0)),
        predelay_s: predelay,
        mean_free_path_m: h.mean_free_path_m,
    })
}
