//! CPU reference of the atmosphere model.
//!
//! The same single-scattering integral as `wgsl/atmosphere.wgsl`, in `f64`,
//! with the same height formulation, step distribution and step counts. It is
//! the oracle of the unit tests and the GPU readback comparisons; nothing in
//! the renderer calls it per frame.
//!
//! Geometry. The planet is a sphere of radius `R` below the camera, world up is
//! `+Y`. A point on a ray `P(t) = P0 + t d` with `P0 = (0, R + a, 0)` has
//! `|P|^2 - R^2 = u(t) = a (2R + a) + 2 (R + a) d.y t + t^2`, and its altitude
//! is `h = u / (|P| + R)`: no `|P| - R` subtraction, so `f32` stays accurate at
//! planetary radii. Only the camera altitude `a` matters, so horizontal
//! position (and the large-world origin) never enters.

use super::AtmosphereSettings;
use std::f64::consts::PI;

/// View-ray integration steps for the sky pass.
pub const SKY_STEPS: u32 = 32;
/// View-ray integration steps for aerial perspective.
pub const AERIAL_STEPS: u32 = 16;
/// Steps of the sun-ray optical depth.
pub const SUN_STEPS: u32 = 10;
/// Optical depth assigned to a ray blocked by the planet.
pub const BLOCKED_DEPTH: f64 = 1.0e6;
/// Largest radiance written (`Rgba16Float` holds 65504).
pub const MAX_RADIANCE: f64 = 60_000.0;

pub type Rgb = [f64; 3];

/// The medium of one atmosphere in `f64`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Medium {
    pub planet_radius: f64,
    pub thickness: f64,
    pub rayleigh: Rgb,
    pub rayleigh_height: f64,
    pub mie_scattering: f64,
    pub mie_absorption: f64,
    pub mie_height: f64,
    pub mie_g: f64,
    pub ozone: Rgb,
    pub ozone_center: f64,
    pub ozone_width: f64,
    pub ground_albedo: Rgb,
}

fn widen(v: [f32; 3]) -> Rgb {
    v.map(f64::from)
}

impl Medium {
    pub fn new(settings: &AtmosphereSettings) -> Self {
        Self {
            planet_radius: f64::from(settings.planet_radius),
            thickness: settings.thickness(),
            rayleigh: widen(settings.rayleigh_scattering),
            rayleigh_height: f64::from(settings.rayleigh_scale_height),
            mie_scattering: f64::from(settings.mie_scattering),
            mie_absorption: f64::from(settings.mie_absorption),
            mie_height: f64::from(settings.mie_scale_height),
            mie_g: f64::from(settings.mie_anisotropy),
            ozone: widen(settings.ozone_absorption),
            ozone_center: f64::from(settings.ozone_center_height),
            ozone_width: f64::from(settings.ozone_width),
            ground_albedo: widen(settings.ground_albedo),
        }
    }

    pub fn rayleigh_density(&self, h: f64) -> f64 {
        (-h.max(0.0) / self.rayleigh_height).exp()
    }

    pub fn mie_density(&self, h: f64) -> f64 {
        (-h.max(0.0) / self.mie_height).exp()
    }

    pub fn ozone_density(&self, h: f64) -> f64 {
        (1.0 - (h.max(0.0) - self.ozone_center).abs() / self.ozone_width).max(0.0)
    }

    /// Extinction coefficient (1/m) at altitude `h`.
    pub fn extinction(&self, h: f64) -> Rgb {
        let (r, m, o) = (
            self.rayleigh_density(h),
            self.mie_density(h),
            self.ozone_density(h),
        );
        std::array::from_fn(|i| {
            self.rayleigh[i] * r
                + (self.mie_scattering + self.mie_absorption) * m
                + self.ozone[i] * o
        })
    }

    /// `|P| - R` from `u = |P|^2 - R^2` without cancellation.
    pub fn height(&self, u: f64) -> f64 {
        let r = self.planet_radius;
        u / ((r * r + u).max(0.0).sqrt() + r)
    }
}

pub fn rayleigh_phase(mu: f64) -> f64 {
    3.0 / (16.0 * PI) * (1.0 + mu * mu)
}

/// Cornette-Shanks phase function; integrates to one over the sphere.
pub fn mie_phase(mu: f64, g: f64) -> f64 {
    let g2 = g * g;
    let denom = (1.0 + g2 - 2.0 * g * mu).max(1e-4).powf(1.5);
    3.0 / (8.0 * PI) * (1.0 - g2) * (1.0 + mu * mu) / ((2.0 + g2) * denom)
}

pub fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn normalize(v: [f64; 3]) -> [f64; 3] {
    let length = dot(v, v).sqrt();
    v.map(|c| c / length)
}

/// `(start, end, ground)` along a ray of vertical cosine `mu` from altitude
/// `a`: the atmosphere interval (empty when `end <= start`) and the distance
/// to the planet, if the ray hits it.
pub fn interval(m: &Medium, altitude: f64, mu: f64) -> (f64, f64, Option<f64>) {
    let (r, thick) = (m.planet_radius, m.thickness);
    let b = (r + altitude) * mu;
    let c_top = (thick - altitude) * (2.0 * r + thick + altitude);
    let disc = b * b + c_top;
    if disc < 0.0 {
        return (0.0, 0.0, None);
    }
    let root = disc.sqrt();
    let u0 = altitude * (2.0 * r + altitude);
    let disc_ground = b * b - u0;
    let ground = (b < 0.0 && disc_ground > 0.0).then(|| -b - disc_ground.sqrt());
    ((-b - root).max(0.0), -b + root, ground)
}

/// Optical depth from altitude `h` along the sun direction to the top of the
/// atmosphere. `ps` is `dot(P, sun)`. Blocked by the planet: [`BLOCKED_DEPTH`].
pub fn sun_depth(m: &Medium, h: f64, ps: f64) -> Rgb {
    let (r, thick) = (m.planet_radius, m.thickness);
    let hc = h.clamp(0.0, thick);
    let u = hc * (2.0 * r + hc);
    if ps < 0.0 && ps * ps > u {
        return [BLOCKED_DEPTH; 3];
    }
    let c = (thick - hc) * (2.0 * r + thick + hc);
    let top = if ps > 0.0 {
        c / (ps + (ps * ps + c).sqrt())
    } else {
        -ps + (ps * ps + c).sqrt()
    };
    let mut depth = [0.0; 3];
    let mut previous = 0.0;
    for i in 1..=SUN_STEPS {
        let f = f64::from(i) / f64::from(SUN_STEPS);
        let edge = top * f * f;
        let dt = edge - previous;
        let mid = previous + 0.5 * dt;
        previous = edge;
        let sample = m.extinction(m.height(u + 2.0 * ps * mid + mid * mid));
        for k in 0..3 {
            depth[k] += sample[k] * dt;
        }
    }
    depth
}

/// Single-scattered radiance and transmittance along `dir` between `t0` and `t1`.
pub fn march(
    m: &Medium,
    altitude: f64,
    dir: [f64; 3],
    sun: [f64; 3],
    irradiance: Rgb,
    (t0, t1): (f64, f64),
    steps: u32,
) -> (Rgb, Rgb) {
    let r0 = m.planet_radius + altitude;
    let u0 = altitude * (2.0 * m.planet_radius + altitude);
    let ds = dot(dir, sun);
    let (phase_r, phase_m) = (rayleigh_phase(ds), mie_phase(ds, m.mie_g));
    let mut inscatter = [0.0; 3];
    let mut transmittance = [1.0; 3];
    let mut previous = t0;
    for i in 1..=steps {
        let f = f64::from(i) / f64::from(steps);
        let edge = t0 + (t1 - t0) * f * f;
        let dt = edge - previous;
        let t = previous + 0.5 * dt;
        previous = edge;
        let h = m.height(u0 + 2.0 * r0 * dir[1] * t + t * t).max(0.0);
        let ps = r0 * sun[1] + t * ds;
        let depth = sun_depth(m, h, ps);
        let (rd, md) = (m.rayleigh_density(h), m.mie_density(h));
        let extinction = m.extinction(h);
        for k in 0..3 {
            let scatter = (m.rayleigh[k] * rd * phase_r + m.mie_scattering * md * phase_m)
                * irradiance[k]
                * (-depth[k]).exp();
            let step_t = (-extinction[k] * dt).exp();
            let integral = if extinction[k] > 1e-12 {
                scatter * (1.0 - step_t) / extinction[k]
            } else {
                scatter * dt
            };
            inscatter[k] += transmittance[k] * integral;
            transmittance[k] *= step_t;
        }
    }
    (inscatter, transmittance)
}

/// Sky radiance along `dir` (unit, world space) with the sun at unit `sun`
/// (toward the light) and `irradiance` in shading units, for a camera at
/// `altitude` metres. `sun_radius` is the sun's angular radius (radians).
pub fn sky_radiance(
    m: &Medium,
    altitude: f64,
    dir: [f64; 3],
    sun: [f64; 3],
    irradiance: Rgb,
    sun_radius: f64,
) -> Rgb {
    let (start, end, ground) = interval(m, altitude, dir[1]);
    let mut radiance = [0.0; 3];
    let mut transmittance = [1.0; 3];
    if end > start {
        let stop = ground.map_or(end, |g| g.min(end));
        let (inscatter, t) = march(m, altitude, dir, sun, irradiance, (start, stop), SKY_STEPS);
        radiance = inscatter;
        transmittance = t;
        if let Some(distance) = ground {
            let ps = (m.planet_radius + altitude) * sun[1] + distance * dot(dir, sun);
            let cosine = (ps / m.planet_radius).max(0.0);
            let lit = sun_depth(m, 0.0, ps);
            for k in 0..3 {
                radiance[k] += transmittance[k] * m.ground_albedo[k] / PI
                    * irradiance[k]
                    * cosine
                    * (-lit[k]).exp();
            }
        }
    }
    if ground.is_none() && sun_radius > 0.0 {
        let cross = [
            dir[1] * sun[2] - dir[2] * sun[1],
            dir[2] * sun[0] - dir[0] * sun[2],
            dir[0] * sun[1] - dir[1] * sun[0],
        ];
        let angle = dot(cross, cross).sqrt().atan2(dot(dir, sun));
        let coverage = 1.0 - smoothstep(0.8 * sun_radius, sun_radius, angle);
        if coverage > 0.0 {
            let solid = 4.0 * PI * (0.5 * sun_radius).sin().powi(2);
            for k in 0..3 {
                radiance[k] += coverage * transmittance[k] * irradiance[k] / solid;
            }
        }
    }
    radiance.map(|v| v.clamp(0.0, MAX_RADIANCE))
}

/// In-scattered radiance and transmittance toward geometry `distance` metres
/// along `dir` (already multiplied by the aerial-perspective scale).
pub fn aerial(
    m: &Medium,
    altitude: f64,
    dir: [f64; 3],
    sun: [f64; 3],
    irradiance: Rgb,
    distance: f64,
) -> (Rgb, Rgb) {
    let (start, end, ground) = interval(m, altitude, dir[1]);
    if end <= start {
        return ([0.0; 3], [1.0; 3]);
    }
    let stop = ground.map_or(end, |g| g.min(end)).min(distance);
    if stop <= start {
        return ([0.0; 3], [1.0; 3]);
    }
    march(
        m,
        altitude,
        dir,
        sun,
        irradiance,
        (start, stop),
        AERIAL_STEPS,
    )
}

fn smoothstep(edge0: f64, edge1: f64, x: f64) -> f64 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Transmittance of the straight path `distance` metres long along `dir`,
/// integrated with `steps` equal segments (a finer, independent reference of
/// the optical depth than the renderer's quadratic steps).
pub fn path_transmittance(
    m: &Medium,
    altitude: f64,
    dir: [f64; 3],
    distance: f64,
    steps: u32,
) -> Rgb {
    let r0 = m.planet_radius + altitude;
    let u0 = altitude * (2.0 * m.planet_radius + altitude);
    let dt = distance / f64::from(steps);
    let mut depth = [0.0; 3];
    for i in 0..steps {
        let t = (f64::from(i) + 0.5) * dt;
        let h = m.height(u0 + 2.0 * r0 * dir[1] * t + t * t);
        let e = m.extinction(h);
        for k in 0..3 {
            depth[k] += e[k] * dt;
        }
    }
    depth.map(|d| (-d).exp())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn earth() -> Medium {
        Medium::new(&AtmosphereSettings::default())
    }

    fn sun_at(elevation_degrees: f64) -> [f64; 3] {
        let e = elevation_degrees.to_radians();
        [e.cos(), e.sin(), 0.0]
    }

    fn luma(c: Rgb) -> f64 {
        0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
    }

    #[test]
    fn height_formulation_matches_the_direct_radius_difference() {
        let m = earth();
        for altitude in [0.0, 1.7, 1500.0, 80_000.0] {
            for mu in [-0.0003, 0.0, 0.3, 1.0] {
                for t in [1.0, 750.0, 40_000.0] {
                    let r0 = m.planet_radius + altitude;
                    let horizontal = (1.0f64 - mu * mu).max(0.0).sqrt() * t;
                    let vertical = r0 + mu * t;
                    let direct =
                        (horizontal * horizontal + vertical * vertical).sqrt() - m.planet_radius;
                    let u =
                        altitude * (2.0 * m.planet_radius + altitude) + 2.0 * r0 * mu * t + t * t;
                    assert!(
                        (m.height(u) - direct).abs() < 1e-3,
                        "a={altitude} mu={mu} t={t}: {} vs {direct}",
                        m.height(u)
                    );
                }
            }
        }
    }

    #[test]
    fn phase_functions_integrate_to_one_over_the_sphere() {
        for g in [-0.6, 0.0, 0.76, 0.95] {
            let steps = 200_000;
            let mut mie = 0.0;
            let mut rayleigh = 0.0;
            for i in 0..steps {
                let mu = -1.0 + 2.0 * (f64::from(i) + 0.5) / f64::from(steps);
                mie += mie_phase(mu, g) * 2.0 * PI * (2.0 / f64::from(steps));
                rayleigh += rayleigh_phase(mu) * 2.0 * PI * (2.0 / f64::from(steps));
            }
            assert!((mie - 1.0).abs() < 2e-3, "g={g}: {mie}");
            assert!((rayleigh - 1.0).abs() < 1e-4);
        }
        assert!(
            mie_phase(1.0, 0.8) > 10.0 * mie_phase(0.0, 0.8),
            "forward peak"
        );
    }

    #[test]
    fn vertical_optical_depth_matches_the_exponential_integral() {
        // Rayleigh only: tau = beta * H * (1 - exp(-top / H)) straight up.
        let settings = AtmosphereSettings {
            mie_scattering: 0.0,
            mie_absorption: 0.0,
            ozone_absorption: [0.0; 3],
            ..Default::default()
        };
        let m = Medium::new(&settings);
        let analytic = |beta: f64| {
            let h = m.rayleigh_height;
            (-beta * h * (1.0 - (-m.thickness / h).exp())).exp()
        };
        let t = path_transmittance(&m, 0.0, [0.0, 1.0, 0.0], m.thickness, 20_000);
        for (k, (&transmitted, &beta)) in t.iter().zip(&m.rayleigh).enumerate() {
            let expected = analytic(beta);
            assert!(
                (transmitted - expected).abs() < 2e-4,
                "channel {k}: {transmitted} vs {expected}"
            );
        }
        // The renderer's own sun-ray integral agrees at the zenith sun.
        let renderer = sun_depth(&m, 0.0, m.planet_radius);
        for (depth, &beta) in renderer.iter().zip(&m.rayleigh) {
            assert!(((-depth).exp() - analytic(beta)).abs() < 5e-3);
        }
    }

    #[test]
    fn transmittance_is_monotonic_in_path_length_and_bluer_light_is_removed_first() {
        let m = earth();
        let dir = normalize([1.0, 0.05, 0.0]);
        let mut previous = [1.0; 3];
        for distance in [10.0, 100.0, 1_000.0, 10_000.0, 100_000.0, 400_000.0] {
            let t = path_transmittance(&m, 100.0, dir, distance, 4000);
            for k in 0..3 {
                assert!(t[k] <= previous[k] + 1e-12, "{distance} m channel {k}");
                assert!((0.0..=1.0).contains(&t[k]));
            }
            previous = t;
        }
        let long = path_transmittance(&m, 100.0, dir, 200_000.0, 4000);
        assert!(long[0] > long[1] && long[1] > long[2], "{long:?}");
        // No medium, no extinction.
        let clear = Medium::new(&AtmosphereSettings {
            rayleigh_scattering: [0.0; 3],
            mie_scattering: 0.0,
            mie_absorption: 0.0,
            ozone_absorption: [0.0; 3],
            ..Default::default()
        });
        assert_eq!(path_transmittance(&clear, 0.0, dir, 1e6, 100), [1.0; 3]);
    }

    #[test]
    fn interval_finds_atmosphere_and_planet() {
        let m = earth();
        let (start, end, ground) = interval(&m, 0.0, 1.0);
        assert_eq!(start, 0.0);
        assert!((end - m.thickness).abs() < 1.0);
        assert!(ground.is_none());
        // Straight down from 1 km hits the ground after 1 km.
        let (_, _, ground) = interval(&m, 1000.0, -1.0);
        assert!((ground.unwrap() - 1000.0).abs() < 1e-3);
        // A grazing ray above the horizon never hits the planet; below it does.
        assert!(interval(&m, 1000.0, 0.0).2.is_none());
        assert!(interval(&m, 1000.0, -0.2).2.is_some());
        // From space: a ray aimed at the planet enters the atmosphere first.
        let (start, end, ground) = interval(&m, 400_000.0, -1.0);
        assert!((start - 300_000.0).abs() < 1.0, "{start}");
        assert!(end > start && (ground.unwrap() - 400_000.0).abs() < 1e-2);
        // A ray that misses the shell entirely has an empty interval.
        let (start, end, _) = interval(&m, 400_000.0, 0.0);
        assert!(end <= start);
    }

    #[test]
    fn planet_blocks_the_sun_below_the_horizon() {
        let m = earth();
        let up = m.planet_radius;
        assert!(sun_depth(&m, 0.0, -0.1 * up)[0] >= BLOCKED_DEPTH);
        assert!(sun_depth(&m, 0.0, 0.1 * up)[0] < 10.0);
        // High above the surface the sun stays visible past the local horizon.
        let h = 90_000.0;
        let ps = -0.05 * (up + h);
        assert!(sun_depth(&m, h, ps)[0] < BLOCKED_DEPTH);
    }

    #[test]
    fn noon_zenith_is_bluer_than_the_horizon() {
        let m = earth();
        let e = [1.0; 3];
        let sun = sun_at(80.0);
        let zenith = sky_radiance(&m, 2.0, [0.0, 1.0, 0.0], sun, e, 0.0);
        let horizon = sky_radiance(&m, 2.0, normalize([0.0, 0.02, 1.0]), sun, e, 0.0);
        let blue = |c: Rgb| c[2] / c[0];
        assert!(
            blue(zenith) > 1.2 * blue(horizon),
            "{zenith:?} vs {horizon:?}"
        );
        assert!(luma(horizon) > luma(zenith) * 0.9, "the horizon is hazier");
    }

    #[test]
    fn low_sun_reddens_the_horizon_toward_the_sun() {
        let m = earth();
        let e = [1.0; 3];
        let sun = sun_at(2.0);
        let toward = sky_radiance(&m, 2.0, normalize([1.0, 0.03, 0.0]), sun, e, 0.0);
        let away = sky_radiance(&m, 2.0, normalize([-1.0, 0.03, 0.0]), sun, e, 0.0);
        let noon = sky_radiance(&m, 2.0, normalize([1.0, 0.03, 0.0]), sun_at(70.0), e, 0.0);
        let red = |c: Rgb| c[0] / c[2];
        assert!(red(toward) > 2.0 * red(noon), "{toward:?} vs {noon:?}");
        assert!(red(toward) > red(away), "{toward:?} vs {away:?}");
    }

    #[test]
    fn mie_forward_peak_brightens_the_sky_around_the_sun() {
        let m = earth();
        let sun = sun_at(40.0);
        let e = [1.0; 3];
        let near = normalize([sun[0], sun[1] + 0.06, 0.0]);
        let far = normalize([sun[0], sun[1] + 0.9, 0.0]);
        assert!(
            luma(sky_radiance(&m, 2.0, near, sun, e, 0.0))
                > 1.5 * luma(sky_radiance(&m, 2.0, far, sun, e, 0.0))
        );
    }

    #[test]
    fn sun_disk_is_far_brighter_than_the_sky_and_carries_the_irradiance() {
        let m = earth();
        let sun = sun_at(45.0);
        let radius = 0.00465;
        let e = [3.0; 3];
        let disk = sky_radiance(&m, 2.0, sun, sun, e, radius);
        let beside = sky_radiance(
            &m,
            2.0,
            normalize([sun[0], sun[1] + 0.05, 0.0]),
            sun,
            e,
            radius,
        );
        assert!(luma(disk) > 100.0 * luma(beside));
        // Radiance times solid angle returns the attenuated irradiance.
        let solid = 4.0 * PI * (0.5f64 * radius).sin().powi(2);
        let t = path_transmittance(&m, 2.0, sun, m.thickness * 2.0, 20_000);
        assert!(disk[0] * solid <= 3.0 + 1e-6 && disk[0] * solid > 3.0 * t[0] * 0.5);
        // Without a disk radius there is no disk.
        let none = sky_radiance(&m, 2.0, sun, sun, e, 0.0);
        assert!(luma(none) < luma(disk) * 0.01);
    }

    #[test]
    fn ground_albedo_and_density_scale_the_result() {
        let m = earth();
        let sun = sun_at(50.0);
        let e = [1.0; 3];
        let down = normalize([1.0, -0.4, 0.0]);
        let bright = Medium::new(&AtmosphereSettings {
            ground_albedo: [0.9; 3],
            ..Default::default()
        });
        let dark = Medium::new(&AtmosphereSettings {
            ground_albedo: [0.0; 3],
            ..Default::default()
        });
        assert!(
            luma(sky_radiance(&bright, 2.0, down, sun, e, 0.0))
                > 1.5 * luma(sky_radiance(&dark, 2.0, down, sun, e, 0.0))
        );
        let thick = Medium::new(&AtmosphereSettings {
            rayleigh_scattering: [5.802e-6 * 3.0, 13.558e-6 * 3.0, 33.1e-6 * 3.0],
            ..Default::default()
        });
        let up = normalize([0.0, 0.5, 1.0]);
        let (a, b) = (
            sky_radiance(&m, 2.0, up, sun, e, 0.0),
            sky_radiance(&thick, 2.0, up, sun, e, 0.0),
        );
        assert!(
            luma(b) > 1.4 * luma(a) && b[2] > a[2] && b[0] > 2.0 * a[0],
            "denser Rayleigh scatters more: {a:?} vs {b:?}"
        );
        // No sun, no sky.
        assert_eq!(sky_radiance(&m, 2.0, up, sun, [0.0; 3], 0.0), [0.0; 3]);
    }

    #[test]
    fn aerial_perspective_grows_with_distance_and_stays_bounded() {
        let m = earth();
        let sun = sun_at(40.0);
        let e = [1.0; 3];
        let dir = normalize([0.0, 0.0, 1.0]);
        let mut previous_scatter = 0.0;
        let mut previous_t = [1.0; 3];
        for distance in [100.0, 1_000.0, 10_000.0, 60_000.0, 300_000.0] {
            let (scatter, t) = aerial(&m, 10.0, dir, sun, e, distance);
            assert!(luma(scatter) >= previous_scatter, "{distance}");
            for k in 0..3 {
                assert!(t[k] <= previous_t[k] + 1e-12 && (0.0..=1.0).contains(&t[k]));
            }
            previous_scatter = luma(scatter);
            previous_t = t;
        }
        // Zero distance leaves the scene untouched.
        assert_eq!(aerial(&m, 10.0, dir, sun, e, 0.0), ([0.0; 3], [1.0; 3]));
        // A very long path converges toward the sky colour at that direction.
        let (scatter, _) = aerial(&m, 10.0, dir, sun, e, 1.0e7);
        let sky = sky_radiance(&m, 10.0, dir, sun, e, 0.0);
        for k in 0..3 {
            assert!(
                (scatter[k] - sky[k]).abs() < sky[k] * 0.06,
                "{scatter:?} vs {sky:?}"
            );
        }
    }
}
