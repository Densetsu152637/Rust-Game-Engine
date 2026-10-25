//! Tests 12 and 13: analytic estimates and traced RT60 in the reference shoebox.

use engine_acoustics::reference::*;
use engine_acoustics::*;

fn shoebox_surfaces(absorption: BandGains) -> Vec<(f32, BandGains)> {
    let [x, y, z] = SHOEBOX_DIMS_M;
    [y * z, y * z, x * z, x * z, x * y, x * y]
        .into_iter()
        .map(|a| (a, absorption))
        .collect()
}

#[test]
fn sabine_and_eyring_reproduce_the_reference_numbers() {
    let s = shoebox_surfaces(SHOEBOX_ABSORPTION);
    let sabine = sabine_rt60(480.0, &s, [0.0; 4]).unwrap();
    let eyring = eyring_rt60(480.0, &s, [0.0; 4]).unwrap();
    for b in 0..BANDS {
        let es = (sabine[b] - SHOEBOX_SABINE_RT60_S[b]).abs() / SHOEBOX_SABINE_RT60_S[b];
        let ee = (eyring[b] - SHOEBOX_EYRING_RT60_S[b]).abs() / SHOEBOX_EYRING_RT60_S[b];
        println!(
            "band {b}: sabine {:.4} ({es:.2e}) eyring {:.4} ({ee:.2e})",
            sabine[b], eyring[b]
        );
        assert!(es < 1e-3 && ee < 1e-3);
    }
    // Air adds 4 m V with m = 2 * np_per_m (energy).
    let air = sabine_rt60(480.0, &s, [0.001; 4]).unwrap();
    let expected = 0.161 * 480.0 / (376.0 * 0.05 + 8.0 * 0.001 * 480.0);
    assert!((air[0] - expected).abs() / expected < 1e-5);
    assert!(sabine_rt60(0.0, &s, [0.0; 4]).is_err());
    assert!(eyring_rt60(480.0, &[(1.0, [1.5; 4])], [0.0; 4]).is_err());
}

#[test]
fn traced_rt60_matches_eyring() {
    let world = reference_shoebox(1.0).unwrap();
    let settings = reference_trace_settings(3.0);
    let h = cpu_reference_histogram(&world, &settings, &reference_job(65_536, 7), 8).unwrap();
    let fits = fit_bands(&h, 0.08).unwrap();
    let mfp_expected = 4.0 * 480.0 / 376.0;
    let mfp_err = (h.mean_free_path_m - mfp_expected).abs() / mfp_expected;
    println!(
        "mean free path {} vs {mfp_expected} ({:.2}%)",
        h.mean_free_path_m,
        mfp_err * 100.0
    );
    assert!(mfp_err < 0.02);
    for b in 0..BANDS {
        let eyring = SHOEBOX_EYRING_RT60_S[b];
        let err = (fits[b].rt60_s - eyring).abs() / eyring;
        println!(
            "band {b}: traced {:.3} s ({:?}) eyring {eyring} ({:+.1}%)",
            fits[b].rt60_s,
            fits[b].method,
            (fits[b].rt60_s / eyring - 1.0) * 100.0
        );
        let tolerance = if eyring > 2.5 { 0.15 } else { 0.10 };
        assert!(err <= tolerance, "band {b}");
    }
    // Rays can slip through shared triangle edges (the intersection test is not
    // watertight); the leak must stay negligible.
    println!("escaped {:e}", h.escaped);
    assert!(h.escaped < 1e-3, "closed room leaks {}", h.escaped);
}
