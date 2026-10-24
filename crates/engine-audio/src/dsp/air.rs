//! Atmospheric absorption from ISO 9613-1:1993, equations (1) to (5).

use crate::acoustic::BANDS;
use crate::{AudioError, Result};

/// Decibels per neper (20 / ln 10).
pub(crate) const DB_PER_NEPER: f64 = 8.685_889_638_065_037;

/// Exact base-ten midband frequencies (ISO 266) of the nominal band centres
/// 125 / 500 / 2000 / 8000 Hz, as ISO 9613 prescribes for octave bands.
pub(crate) const EXACT_MIDBANDS_HZ: [f64; BANDS] = [
    125.892_541_179_416_7,
    501.187_233_627_272_3,
    1_995.262_314_968_88,
    7_943.282_347_242_815,
];

/// Pure-tone attenuation coefficient in dB/m.
pub(crate) fn alpha_db_per_m(
    freq: f64,
    temperature_c: f64,
    humidity: f64,
    pressure_kpa: f64,
) -> f64 {
    let t = temperature_c + 273.15;
    let t0 = 293.15;
    let t01 = 273.16;
    let pr = pressure_kpa / 101.325;
    let c = -6.8346 * (t01 / t).powf(1.261) + 4.6151;
    let h = humidity * 10f64.powf(c) / pr;
    let fr_o = pr * (24.0 + 4.04e4 * h * (0.02 + h) / (0.391 + h));
    let fr_n = pr
        * (t / t0).powf(-0.5)
        * (9.0 + 280.0 * h * (-4.170 * ((t / t0).powf(-1.0 / 3.0) - 1.0)).exp());
    let f2 = freq * freq;
    8.686
        * f2
        * (1.84e-11 / pr * (t / t0).sqrt()
            + (t / t0).powf(-2.5)
                * (0.01275 * (-2239.1 / t).exp() / (fr_o + f2 / fr_o)
                    + 0.1068 * (-3352.0 / t).exp() / (fr_n + f2 / fr_n)))
}

/// Air absorption per band as amplitude attenuation in nepers per metre (the
/// unit of `MixerEnvironment::air_np_per_m`), from ISO 9613-1 at the exact
/// midband frequencies. Valid for -20..=50 degrees C, 0..=100 % relative
/// humidity and 50..=120 kPa.
pub fn iso9613_air_np_per_m(
    temperature_c: f32,
    relative_humidity_percent: f32,
    pressure_kpa: f32,
) -> Result<[f32; BANDS]> {
    let ok = |v: f32, lo: f32, hi: f32| v.is_finite() && (lo..=hi).contains(&v);
    if !(ok(temperature_c, -20.0, 50.0)
        && ok(relative_humidity_percent, 0.0, 100.0)
        && ok(pressure_kpa, 50.0, 120.0))
    {
        return Err(AudioError::new(
            "audio.environment.invalid",
            "Air needs temperature -20-50 C, humidity 0-100 % and pressure 50-120 kPa",
        ));
    }
    Ok(EXACT_MIDBANDS_HZ.map(|f| {
        (alpha_db_per_m(
            f,
            f64::from(temperature_c),
            f64::from(relative_humidity_percent),
            f64::from(pressure_kpa),
        ) / DB_PER_NEPER) as f32
    }))
}

/// Converts explicit decibels per metre to nepers per metre.
pub fn db_per_m_to_np_per_m(db_per_m: [f32; BANDS]) -> [f32; BANDS] {
    db_per_m.map(|v| (f64::from(v) / DB_PER_NEPER) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ISO 9613-2:1996 Table 2 (dB/km) at 125, 500, 2000 and 8000 Hz.
    const TABLE: [(f32, f32, [f64; 4]); 6] = [
        (10.0, 70.0, [0.4, 1.9, 9.7, 117.0]),
        (20.0, 70.0, [0.3, 2.8, 9.0, 76.6]),
        (30.0, 70.0, [0.3, 3.1, 12.7, 59.3]),
        (15.0, 20.0, [0.6, 2.7, 28.2, 202.0]),
        (15.0, 50.0, [0.5, 2.2, 10.8, 129.0]),
        (15.0, 80.0, [0.3, 2.4, 8.3, 82.8]),
    ];

    #[test]
    fn matches_the_published_table() {
        for (t, h, row) in TABLE {
            let np = iso9613_air_np_per_m(t, h, 101.325).unwrap();
            for b in 0..4 {
                let db_km = f64::from(np[b]) * DB_PER_NEPER * 1000.0;
                // The table rounds to 0.1 dB/km or three significant figures.
                let tol = (row[b] * 0.01).max(0.06);
                assert!(
                    (db_km - row[b]).abs() <= tol,
                    "{t} C {h} %: band {b} {db_km} vs {}",
                    row[b]
                );
            }
        }
    }

    #[test]
    fn rejects_out_of_range_air() {
        assert!(iso9613_air_np_per_m(80.0, 50.0, 101.325).is_err());
        assert!(iso9613_air_np_per_m(20.0, f32::NAN, 101.325).is_err());
        let np = db_per_m_to_np_per_m([DB_PER_NEPER as f32; 4]);
        assert!(np.iter().all(|v| (v - 1.0).abs() < 1e-6));
    }
}
