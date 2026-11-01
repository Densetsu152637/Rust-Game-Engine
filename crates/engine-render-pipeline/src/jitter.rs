//! The sub-pixel jitter sequence shared by every temporal consumer.
//!
//! Exactly one owner supplies [`crate::ViewData::jitter`] for a view: the host
//! renderer, from the sequence below, whenever a temporal upscaler or resolve
//! (DLSS Super Resolution or the native temporal resolve) owns the view's
//! Upscale stage. The pipeline only applies the value it is given.

/// Radical inverse of `index` (>= 1) in `base`: the Halton sequence in (0, 1).
pub fn halton(mut index: u32, base: u32) -> f32 {
    let mut fraction = 1.0;
    let mut result = 0.0;
    while index > 0 {
        fraction /= base as f32;
        result += fraction * (index % base) as f32;
        index /= base;
    }
    result
}

/// Jitter phases for a render/output ratio: NVIDIA's DLSS guidance of eight
/// phases times the squared per-axis scale, clamped to 8..=256. The native
/// temporal resolve uses the same count so that every output pixel meets many
/// distinct sample positions before the sequence repeats.
pub fn jitter_phase_count(render: [u32; 2], output: [u32; 2]) -> u32 {
    let scale = output[0] as f32 / render[0].max(1) as f32;
    ((8.0 * scale * scale).ceil() as u32).clamp(8, 256)
}

/// Halton(2,3) sub-pixel jitter in render pixels, each axis in (-0.5, 0.5),
/// for 0-based `frame` within `phases`.
pub fn jitter_offset(frame: u64, phases: u32) -> [f32; 2] {
    let index = (frame % u64::from(phases.max(1))) as u32 + 1;
    [halton(index, 2) - 0.5, halton(index, 3) - 0.5]
}
