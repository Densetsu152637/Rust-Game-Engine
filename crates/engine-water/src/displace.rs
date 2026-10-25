//! Inverting a horizontal surface displacement.
//!
//! Gerstner and choppy FFT surfaces move a rest point `x0` to `x0 + D(x0)` horizontally, so the
//! height above a world position `p` is the height of the rest point `x0` that solves
//! `x0 + D(x0) = p`. [`invert_horizontal`] solves it with a bounded fixed-point iteration
//! `x0 <- x0 - J(x0)^-1 r(x0)`, `r = x0 + D(x0) - p`, `J = I + dD/dx0` (the Newton map). When a
//! Newton step does not reduce the residual, or `J` is near singular, it takes the plain
//! fixed-point step `x0 <- p - D(x0)` instead, which converges whenever `D` is a contraction
//! (Lipschitz constant below 1; Gerstner sets guarantee at most
//! [`crate::MAX_STEEPNESS_SUM`]).
//!
//! It stops when `|r| <= INVERSE_TOLERANCE` (1e-4 m) or after `INVERSE_MAX_ITERATIONS` (16)
//! iterations and returns the iterate with the smallest residual. For a contraction with
//! constant `L`, the rest-position error is at most `|r| / (1 - L)`.
use glam::{Mat2, Vec2};

/// Convergence tolerance on the horizontal residual `|x0 + D(x0) - p|`, in metres.
pub const INVERSE_TOLERANCE: f32 = 1.0e-4;
/// Most iterations of [`invert_horizontal`].
pub const INVERSE_MAX_ITERATIONS: u32 = 16;

/// Horizontal displacement `D(x0)` and its Jacobian `dD/dx0` at a rest position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HorizontalSample {
    pub offset: Vec2,
    /// Columns: derivative with respect to rest x, rest z.
    pub jacobian: Mat2,
}

/// Result of [`invert_horizontal`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Inversion {
    /// Rest position `x0` whose displaced position is closest to the target.
    pub rest: Vec2,
    /// `|x0 + D(x0) - p|` at `rest`.
    pub residual: f32,
    /// Iterations taken.
    pub iterations: u32,
    /// Whether the residual reached [`INVERSE_TOLERANCE`].
    pub converged: bool,
}

/// Solves `x0 + D(x0) = target` for the rest position `x0` (see the module docs).
pub fn invert_horizontal(target: Vec2, eval: impl Fn(Vec2) -> HorizontalSample) -> Inversion {
    let mut x = target;
    let mut sample = eval(x);
    let mut r = x + sample.offset - target;
    let mut best = (x, r.length());
    let mut iterations = 0;
    while iterations < INVERSE_MAX_ITERATIONS && best.1 > INVERSE_TOLERANCE {
        iterations += 1;
        let j = Mat2::IDENTITY + sample.jacobian;
        let det = j.determinant();
        let newton = det.abs() > 1.0e-3;
        let mut next = if newton { x - j.inverse() * r } else { x - r };
        let mut next_sample = eval(next);
        let mut next_r = next + next_sample.offset - target;
        if newton && (!next_r.is_finite() || next_r.length() >= r.length()) {
            next = x - r;
            next_sample = eval(next);
            next_r = next + next_sample.offset - target;
        }
        if !next_r.is_finite() {
            break;
        }
        x = next;
        sample = next_sample;
        r = next_r;
        if r.length() < best.1 {
            best = (x, r.length());
        }
    }
    Inversion {
        rest: best.0,
        residual: best.1,
        iterations,
        converged: best.1 <= INVERSE_TOLERANCE,
    }
}
