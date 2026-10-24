//! Acoustic DSP rendered by the mixer (Stream A of `docs/spatial-audio-design.md`).
//!
//! Everything here is plain data plus allocation-free per-block processing. All
//! buffers are sized on the control thread (`play`, `configure`,
//! `set_environment`, `acoustic_publisher`); the audio thread only reads, writes
//! and swaps them.

// Band, lane and channel loops index several parallel fixed-size arrays at
// once; explicit indices read closer to the DSP equations than zipped iterators.
#![allow(clippy::needless_range_loop)]

pub(crate) mod air;
pub(crate) mod band;
pub(crate) mod convolver;
pub(crate) mod delay;
pub(crate) mod fdn;
pub(crate) mod fft;
pub(crate) mod hrtf;
pub(crate) mod ramp;
pub(crate) mod render;
pub(crate) mod voice;

/// Frames processed per internal mixing block.
pub(crate) const BLOCK: usize = 256;

/// Values below this magnitude are flushed to zero once per block so decaying
/// filter states never linger in the (slow) subnormal range.
pub(crate) const FLUSH: f32 = 1e-15;

#[inline]
pub(crate) fn flush(value: &mut f32) {
    if value.abs() < FLUSH {
        *value = 0.0;
    }
}

/// Replaces non-finite values with zero and clamps to `[low, high]`.
#[inline]
pub(crate) fn sanitize(value: f32, low: f32, high: f32) -> f32 {
    if value.is_finite() {
        value.clamp(low, high)
    } else {
        0.0
    }
}

pub(crate) fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn length(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

/// Unit vector, or `None` for (near) zero input.
pub(crate) fn normalize(a: [f64; 3]) -> Option<[f64; 3]> {
    let l = length(a);
    (l.is_finite() && l > 1e-9).then(|| a.map(|v| v / l))
}
