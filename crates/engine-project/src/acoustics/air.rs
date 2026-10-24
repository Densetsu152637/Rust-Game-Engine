//! Air absorption: ISO 9613-1 pure-tone attenuation at the four band centres.
use super::components::Air;
use engine_acoustics::BandGains;
use engine_audio::acoustic::BAND_CENTERS_HZ;

/// Decibels per neper (`20 / ln 10`).
const DB_PER_NP: f64 = 8.685_889_638;

/// ISO 9613-1:1993 pure-tone atmospheric absorption in dB/m for frequency `f`
/// (Hz), temperature (C), relative humidity (percent) and pressure (kPa).
pub fn iso9613_db_per_m(
    f: f64,
    temperature_c: f64,
    humidity_percent: f64,
    pressure_kpa: f64,
) -> f64 {
    const REFERENCE_PRESSURE: f64 = 101.325;
    const REFERENCE_TEMPERATURE: f64 = 293.15;
    const TRIPLE_POINT: f64 = 273.16;
    let t = temperature_c + 273.15;
    let pr = pressure_kpa / REFERENCE_PRESSURE;
    let c = -6.8346 * (TRIPLE_POINT / t).powf(1.261) + 4.6151;
    let h = humidity_percent * 10f64.powf(c) / pr;
    let t_rel = t / REFERENCE_TEMPERATURE;
    let oxygen = pr * (24.0 + 4.04e4 * h * (0.02 + h) / (0.391 + h));
    let nitrogen =
        pr * t_rel.powf(-0.5) * (9.0 + 280.0 * h * (-4.170 * (t_rel.powf(-1.0 / 3.0) - 1.0)).exp());
    let f2 = f * f;
    8.686
        * f2
        * (1.84e-11 / pr * t_rel.sqrt()
            + t_rel.powf(-2.5)
                * (0.01275 * (-2239.1 / t).exp() / (oxygen + f2 / oxygen)
                    + 0.1068 * (-3352.0 / t).exp() / (nitrogen + f2 / nitrogen)))
}

/// Per-band amplitude attenuation in Np/m for the mixer and tracer.
pub fn air_np_per_m(air: &Air) -> BandGains {
    if !air.enabled {
        return [0.0; 4];
    }
    let mut result = [0.0; 4];
    for (band, value) in result.iter_mut().enumerate() {
        let db = air.absorption_db_per_m.map_or_else(
            || {
                iso9613_db_per_m(
                    f64::from(BAND_CENTERS_HZ[band]),
                    f64::from(air.temperature_c),
                    f64::from(air.humidity_percent),
                    101.325,
                )
            },
            |explicit| f64::from(explicit[band]),
        );
        *value = (db / DB_PER_NP) as f32;
    }
    result
}
