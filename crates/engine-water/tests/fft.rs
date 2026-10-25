//! FFT correctness: round trips, agreement with the direct DFT, input validation.
use engine_water::Complex;
use engine_water::fft::{dft, fft, fft2};

fn noise(n: usize, seed: u32) -> Vec<Complex> {
    let mut state = seed.wrapping_mul(2_654_435_761).max(1);
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state as f32 / u32::MAX as f32 * 2.0 - 1.0
    };
    (0..n).map(|_| Complex::new(next(), next())).collect()
}

fn max_error(a: &[Complex], b: &[Complex]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (*x - *y).norm_sqr().sqrt())
        .fold(0.0, f32::max)
}

#[test]
fn round_trip_recovers_the_input_within_1e5() {
    for n in [1usize, 2, 8, 64, 512, 4096] {
        let input = noise(n, n as u32);
        let mut data = input.clone();
        fft(&mut data, false).unwrap();
        fft(&mut data, true).unwrap();
        let back: Vec<Complex> = data.iter().map(|c| c.scale(1.0 / n as f32)).collect();
        let error = max_error(&back, &input);
        eprintln!("n {n}: round-trip error {error:e}");
        assert!(error < 1.0e-5, "n {n}: {error}");
    }
}

#[test]
fn forward_and_inverse_match_the_direct_dft() {
    for n in [4usize, 16, 128] {
        let input = noise(n, 7);
        for inverse in [false, true] {
            let mut fast = input.clone();
            fft(&mut fast, inverse).unwrap();
            let slow = dft(&input, inverse);
            let error = max_error(&fast, &slow);
            assert!(error < 2.0e-4 * (n as f32).sqrt(), "n {n}: {error}");
        }
    }
}

#[test]
fn two_dimensional_round_trip_and_single_mode() {
    let n = 64;
    let input = noise(n * n, 3);
    let mut data = input.clone();
    fft2(&mut data, n, false).unwrap();
    fft2(&mut data, n, true).unwrap();
    let scale = 1.0 / (n * n) as f32;
    let back: Vec<Complex> = data.iter().map(|c| c.scale(scale)).collect();
    assert!(max_error(&back, &input) < 1.0e-5);
    // A single mode (kx = 3, kz = 5) synthesizes e^(2 pi i (3 c + 5 r) / n).
    let mut spectrum = vec![Complex::ZERO; n * n];
    spectrum[5 * n + 3] = Complex::new(1.0, 0.0);
    fft2(&mut spectrum, n, true).unwrap();
    for (r, c) in [(0usize, 0usize), (7, 11), (63, 2)] {
        let angle = std::f32::consts::TAU * (3 * c + 5 * r) as f32 / n as f32;
        let expected = Complex::from_angle(angle);
        assert!((spectrum[r * n + c] - expected).norm_sqr().sqrt() < 1.0e-5);
    }
}

#[test]
fn rejects_non_power_of_two_sizes() {
    let mut odd = vec![Complex::ZERO; 12];
    assert_eq!(fft(&mut odd, false).unwrap_err().code, "water.fft.size");
    let mut empty: Vec<Complex> = Vec::new();
    assert!(fft(&mut empty, false).is_err());
    let mut grid = vec![Complex::ZERO; 15];
    assert!(fft2(&mut grid, 4, false).is_err());
}
