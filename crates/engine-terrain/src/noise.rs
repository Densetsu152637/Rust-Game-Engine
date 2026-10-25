//! Seeded 2D gradient noise for the sculpt `noise` brush (platform-independent integer hashing).

fn hash(x: i32, z: i32, seed: u32) -> u32 {
    let mut h = (x as u32)
        .wrapping_mul(0x8da6_b343)
        .wrapping_add((z as u32).wrapping_mul(0xd816_3841))
        .wrapping_add(seed.wrapping_mul(0xcb1a_b31f));
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a_2d39);
    h ^ (h >> 15)
}

fn gradient(h: u32, dx: f32, dz: f32) -> f32 {
    const G: [(f32, f32); 8] = [
        (1.0, 0.0),
        (-1.0, 0.0),
        (0.0, 1.0),
        (0.0, -1.0),
        (
            std::f32::consts::FRAC_1_SQRT_2,
            std::f32::consts::FRAC_1_SQRT_2,
        ),
        (
            -std::f32::consts::FRAC_1_SQRT_2,
            std::f32::consts::FRAC_1_SQRT_2,
        ),
        (
            std::f32::consts::FRAC_1_SQRT_2,
            -std::f32::consts::FRAC_1_SQRT_2,
        ),
        (
            -std::f32::consts::FRAC_1_SQRT_2,
            -std::f32::consts::FRAC_1_SQRT_2,
        ),
    ];
    let (gx, gz) = G[(h & 7) as usize];
    gx * dx + gz * dz
}

fn fade(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// Gradient (Perlin) noise at `(x, z)`, roughly within -1..=1; 0 at integer lattice points.
pub fn gradient_noise(x: f32, z: f32, seed: u32) -> f32 {
    let (x0, z0) = (x.floor(), z.floor());
    let (fx, fz) = (x - x0, z - z0);
    let (ix, iz) = (x0 as i32, z0 as i32);
    let n00 = gradient(hash(ix, iz, seed), fx, fz);
    let n10 = gradient(hash(ix + 1, iz, seed), fx - 1.0, fz);
    let n01 = gradient(hash(ix, iz + 1, seed), fx, fz - 1.0);
    let n11 = gradient(hash(ix + 1, iz + 1, seed), fx - 1.0, fz - 1.0);
    let (u, v) = (fade(fx), fade(fz));
    let a = n00 + (n10 - n00) * u;
    let b = n01 + (n11 - n01) * u;
    (a + (b - a) * v) * std::f32::consts::SQRT_2
}

/// Four octaves of [`gradient_noise`] (lacunarity 2, gain 0.5), normalized to about -1..=1.
pub fn fbm(x: f32, z: f32, seed: u32) -> f32 {
    let (mut sum, mut amplitude, mut frequency, mut total) = (0.0, 1.0, 1.0, 0.0);
    for octave in 0..4u32 {
        sum += amplitude * gradient_noise(x * frequency, z * frequency, seed.wrapping_add(octave));
        total += amplitude;
        amplitude *= 0.5;
        frequency *= 2.0;
    }
    sum / total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_is_deterministic_bounded_and_seeded() {
        let mut max = 0.0f32;
        for i in 0..2000 {
            let (x, z) = (i as f32 * 0.137, i as f32 * 0.071 - 30.0);
            let a = fbm(x, z, 7);
            assert_eq!(a, fbm(x, z, 7));
            max = max.max(a.abs());
        }
        assert!(max <= 1.0 && max > 0.2, "{max}");
        assert_ne!(fbm(0.5, 0.5, 1), fbm(0.5, 0.5, 2));
        assert_eq!(gradient_noise(3.0, -4.0, 9), 0.0);
    }
}
