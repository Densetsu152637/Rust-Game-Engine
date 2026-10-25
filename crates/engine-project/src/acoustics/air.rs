//! Air absorption: ISO 9613-1 pure-tone attenuation at the four band centres.
use super::components::Air;
use engine_acoustics::BandGains;

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

/// Per-band amplitude attenuation in Np/m for the mixer and tracer (energy decays
/// as `exp(-2 m d)`), using the mixer's own ISO 9613-1 at the exact midbands for
/// the temperature/humidity form. Out-of-range values (which component
/// validation rejects) give no attenuation instead of a made-up number.
pub fn air_np_per_m(air: &Air) -> BandGains {
    if !air.enabled {
        return [0.0; 4];
    }
    match air.absorption_db_per_m {
        Some(explicit) => engine_audio::acoustic::db_per_m_to_np_per_m(explicit),
        None => engine_audio::acoustic::iso9613_air_np_per_m(
            air.temperature_c,
            air.humidity_percent,
            101.325,
        )
        .unwrap_or([0.0; 4]),
    }
}
