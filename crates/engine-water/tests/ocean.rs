//! FFT ocean: Parseval, spectrum energy, FFT vs direct synthesis, tile periodicity,
//! determinism, the dispersion relation of the time evolution, loop period and displacement
//! inversion.
use engine_water::fft::fft2;
use engine_water::{
    Complex, GRAVITY, INVERSE_TOLERANCE, OceanSettings, OceanSpectrum, SpectrumKind,
    angular_frequency,
};
use glam::Vec2;

fn settings() -> OceanSettings {
    OceanSettings {
        resolution: 64,
        patch_length: 200.0,
        wind_speed: 10.0,
        seed: 42,
        ..OceanSettings::default()
    }
}

/// Mean square height of a frame equals the sum of |h(k, t)|^2 (Parseval) at every time.
#[test]
fn parseval_holds_at_every_time() {
    let ocean = OceanSpectrum::new(&settings(), GRAVITY).unwrap();
    for t in [0.0, 1.7, 23.0, 400.0] {
        let frame = ocean.frame(t).unwrap();
        let spatial: f64 = frame
            .texels()
            .iter()
            .map(|x| (x.h as f64).powi(2))
            .sum::<f64>()
            / frame.texels().len() as f64;
        let spectral: f64 = ocean
            .amplitudes(t)
            .iter()
            .map(|a| a.norm_sqr() as f64)
            .sum();
        let relative = (spatial - spectral).abs() / spectral;
        eprintln!("t {t}: spatial {spatial:.6e} spectral {spectral:.6e} rel {relative:.2e}");
        assert!(relative < 1.0e-4);
    }
}

/// The discrete spectrum holds the continuum energy (Phillips: a alpha L^2 / 4), and the
/// realized variance averaged over seeds matches the expected discrete variance.
#[test]
fn spectrum_energy_matches_the_continuum_and_the_realizations() {
    for kind in [SpectrumKind::Phillips, SpectrumKind::Jonswap] {
        let mut s = settings();
        s.spectrum = kind;
        (s.resolution, s.patch_length) = match kind {
            SpectrumKind::Phillips => (128, 400.0),
            SpectrumKind::Jonswap => (256, 1000.0),
        };
        let ocean = OceanSpectrum::new(&s, GRAVITY).unwrap();
        let stats = ocean.stats();
        let ratio = stats.variance / stats.continuum_variance;
        eprintln!(
            "{kind:?}: discrete {:.4} continuum {:.4} (ratio {ratio:.3}), Hs {:.2} m, peak {:.1} m / {:.2} s",
            stats.variance,
            stats.continuum_variance,
            stats.significant_height,
            stats.peak_wavelength,
            stats.peak_period
        );
        assert!((ratio - 1.0).abs() < 0.1, "{kind:?} ratio {ratio}");
        let mut realized = 0.0f64;
        let seeds = 24;
        for seed in 0..seeds {
            s.seed = seed;
            let frame = OceanSpectrum::new(&s, GRAVITY).unwrap().frame(3.0).unwrap();
            let (_, _, mean_square, _) = frame.height_stats();
            realized += mean_square as f64;
        }
        let realized = realized / seeds as f64;
        let error = (realized / stats.variance as f64 - 1.0).abs();
        eprintln!("{kind:?}: mean realized variance {realized:.4} (error {error:.3})");
        assert!(error < 0.1);
        let total: f32 = stats.energy_by_wavelength.iter().sum();
        assert!((total - 1.0).abs() < 1.0e-4);
    }
    // Phillips continuum variance for 10 m/s: 0.0081 * (100 / 9.81)^2 / 4.
    let s = settings();
    let expected = 0.0081 * (100.0f32 / GRAVITY).powi(2) / 4.0;
    assert!((s.continuum_variance(GRAVITY) - expected).abs() < 1.0e-6);
}

/// The FFT frame equals the direct Fourier sum at grid points, and the direct sum repeats with
/// the patch period in X and Z (tileable).
#[test]
fn frame_matches_direct_synthesis_and_tiles() {
    let mut s = settings();
    s.resolution = 32;
    let ocean = OceanSpectrum::new(&s, GRAVITY).unwrap();
    let t = 5.5;
    let frame = ocean.frame(t).unwrap();
    let sigma = ocean.discrete_variance().sqrt();
    let cell = s.patch_length / 32.0;
    let mut worst: f32 = 0.0;
    for (c, r) in [(0i64, 0i64), (3, 17), (31, 31), (12, 5)] {
        let direct = ocean.height_direct(Vec2::new(c as f32, r as f32) * cell, t);
        worst = worst.max((direct - frame.texel(c, r).h).abs());
    }
    eprintln!("FFT vs direct: {worst:e} m (sigma {sigma:.3})");
    assert!(worst < 1.0e-4 * sigma.max(1.0));
    let l = s.patch_length;
    let mut periodic: f32 = 0.0;
    for p in [
        Vec2::new(1.3, 7.9),
        Vec2::new(-40.2, 13.1),
        Vec2::new(77.7, -3.3),
    ] {
        let h = ocean.height_direct(p, t);
        for shift in [Vec2::new(l, 0.0), Vec2::new(0.0, l), Vec2::new(-l, 2.0 * l)] {
            periodic = periodic.max((ocean.height_direct(p + shift, t) - h).abs());
        }
        // The sampled frame tiles too (wrapping bilinear lookups).
        let a = frame.at_rest(p);
        let b = frame.at_rest(p + Vec2::new(l, -l));
        assert!((a.offset - b.offset).length() < 1.0e-4 * sigma.max(1.0));
    }
    eprintln!("tile periodicity error {periodic:e} m");
    assert!(periodic < 1.0e-3 * sigma.max(1.0));
    // Opposite edges of the grid continue each other: texel n wraps to texel 0.
    assert_eq!(frame.texel(32, 3), frame.texel(0, 3));
}

#[test]
fn same_seed_is_bit_identical_and_seeds_differ() {
    let a = OceanSpectrum::new(&settings(), GRAVITY).unwrap();
    let b = OceanSpectrum::new(&settings(), GRAVITY).unwrap();
    assert_eq!(a, b);
    assert_eq!(a.frame(12.5).unwrap(), b.frame(12.5).unwrap());
    let mut other = settings();
    other.seed = 43;
    let c = OceanSpectrum::new(&other, GRAVITY).unwrap();
    assert_ne!(a.frame(12.5).unwrap(), c.frame(12.5).unwrap());
}

/// Each Fourier mode of the evolving height field rotates with the dispersion relation:
/// with JONSWAP (downwind waves only) a mode's phase advances by `-w t`, measured from forward
/// FFTs of two frames, for deep and finite depth.
#[test]
fn time_evolution_follows_the_dispersion_relation() {
    for depth in [0.0f32, 4.0] {
        let s = OceanSettings {
            spectrum: SpectrumKind::Jonswap,
            patch_length: 300.0,
            depth,
            ..settings()
        };
        let ocean = OceanSpectrum::new(&s, GRAVITY).unwrap();
        let n = 64;
        let spectrum_of = |t: f32| {
            let frame = ocean.frame(t).unwrap();
            let mut data: Vec<Complex> = frame
                .texels()
                .iter()
                .map(|x| Complex::new(x.h, 0.0))
                .collect();
            fft2(&mut data, n, false).unwrap();
            data
        };
        let (t0, t1) = (1.0, 3.0);
        let (a, b) = (spectrum_of(t0), spectrum_of(t1));
        let mut checked = 0;
        for (c, r) in [(2usize, 0usize), (3, 1), (5, 0), (4, 63), (7, 2)] {
            let i = r * n + c;
            if a[i].norm_sqr() < 1.0e-12 {
                continue;
            }
            let (k, _) = ocean.mode(c, r).unwrap();
            let w = angular_frequency(k.length(), s.dispersion_depth(), GRAVITY);
            let measured = -(b[i] * a[i].conj()).im.atan2((b[i] * a[i].conj()).re) / (t1 - t0);
            eprintln!(
                "depth {depth}: mode ({c},{r}) k {:.4}: w {w:.5} measured {measured:.5}",
                k.length()
            );
            assert!((measured - w).abs() < 1.0e-3 * w, "{measured} vs {w}");
            checked += 1;
        }
        assert!(checked >= 3);
    }
}

#[test]
fn loop_period_repeats_the_surface() {
    let s = OceanSettings {
        loop_period: 20.0,
        ..settings()
    };
    let ocean = OceanSpectrum::new(&s, GRAVITY).unwrap();
    let (a, b) = (ocean.frame(3.0).unwrap(), ocean.frame(23.0).unwrap());
    let worst = a
        .texels()
        .iter()
        .zip(b.texels())
        .map(|(x, y)| (x.h - y.h).abs())
        .fold(0.0, f32::max);
    assert!(worst < 1.0e-3, "{worst}");
}

/// Choppy displacement is inverted for height queries; residuals stay within tolerance where
/// the surface does not fold.
#[test]
fn choppy_height_queries_invert_the_displacement() {
    let ocean = OceanSpectrum::new(&settings(), GRAVITY).unwrap();
    let frame = ocean.frame(7.0).unwrap();
    let (_, _, _, folded) = frame.height_stats();
    let mut worst: f32 = 0.0;
    let mut converged = 0;
    let mut unfolded = 0;
    for i in 0..2500 {
        let p = Vec2::new((i % 50) as f32 * 3.97, (i / 50) as f32 * 4.03);
        let (point, inversion) = frame.sample(p);
        if point.determinant < 0.2 {
            continue;
        }
        unfolded += 1;
        let landed = inversion.rest + Vec2::new(point.offset.x, point.offset.z);
        worst = worst.max(landed.distance(p));
        converged += usize::from(inversion.converged);
    }
    eprintln!(
        "folded texels {:.2}%, unfolded queries {unfolded}, converged {converged}, max error {worst:e} m",
        folded * 100.0
    );
    assert!(unfolded > 2000);
    assert!(converged as f32 >= 0.99 * unfolded as f32);
    assert!(worst < 10.0 * INVERSE_TOLERANCE, "{worst}");
}

#[test]
fn rejects_invalid_settings() {
    let bad = OceanSettings {
        resolution: 100,
        ..settings()
    };
    assert_eq!(
        OceanSpectrum::new(&bad, GRAVITY).unwrap_err().code,
        "water.ocean.invalid"
    );
    for s in [
        OceanSettings {
            choppiness: 3.0,
            ..settings()
        },
        OceanSettings {
            wind_speed: 0.0,
            ..settings()
        },
        OceanSettings {
            depth: 0.01,
            ..settings()
        },
        OceanSettings {
            patch_length: f32::NAN,
            ..settings()
        },
    ] {
        assert!(s.validate().is_err());
    }
}
