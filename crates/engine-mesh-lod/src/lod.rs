//! LOD chains and screen-space LOD selection.

use crate::bounds::BoundingSphere;
use crate::distance::mesh_error;
use crate::error::{MeshLodError, Result};
use crate::mesh::MeshData;
use crate::simplify::{SimplifyOptions, simplify};

/// How one LOD level is specified relative to the original mesh.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LodTarget {
    /// Fraction of the original triangle count, in `(0, 1]`.
    Ratio(f32),
    /// Maximum geometric error in object units.
    Error(f32),
    /// Absolute triangle count (>= 1).
    Triangles(usize),
}

/// One generated level.
#[derive(Debug, Clone, PartialEq)]
pub struct LodLevel {
    pub mesh: MeshData,
    pub triangle_count: usize,
    /// Approximate Hausdorff error against the original mesh, object units.
    /// Non-decreasing along the chain; 0 for level 0.
    pub error: f32,
}

/// Ordered LODs, finest first, with the original as level 0.
#[derive(Debug, Clone, PartialEq)]
pub struct LodChain {
    pub levels: Vec<LodLevel>,
    /// Bounding sphere of the original mesh (used for selection).
    pub bounds: BoundingSphere,
}

impl LodChain {
    /// Per-level errors, convenient for [`select_lod`].
    pub fn errors(&self) -> Vec<f32> {
        self.levels.iter().map(|l| l.error).collect()
    }
}

/// Builds a chain: level 0 is `mesh` itself, then one level per target.
///
/// Each level is simplified from the previous one but its `error` is measured
/// against the original, and forced to be non-decreasing. `base` supplies the
/// weights and locking flags; its target fields are ignored.
pub fn build_lod_chain(
    mesh: &MeshData,
    targets: &[LodTarget],
    base: &SimplifyOptions,
) -> Result<LodChain> {
    mesh.validate()?;
    let original_tris = mesh.triangle_count();
    let bounds = BoundingSphere::from_points(&mesh.positions).ok_or(MeshLodError::EmptyMesh)?;
    let mut levels = vec![LodLevel {
        mesh: mesh.clone(),
        triangle_count: original_tris,
        error: 0.0,
    }];
    for target in targets {
        let mut opts = *base;
        opts.target_triangles = None;
        opts.max_error = None;
        match *target {
            LodTarget::Ratio(r) => {
                if !(r.is_finite() && r > 0.0 && r <= 1.0) {
                    return Err(MeshLodError::InvalidOption {
                        reason: "LodTarget::Ratio must be in (0, 1]".into(),
                    });
                }
                opts.target_triangles = Some(((original_tris as f64 * f64::from(r)).ceil() as usize).max(1));
            }
            LodTarget::Triangles(n) => opts.target_triangles = Some(n),
            LodTarget::Error(e) => opts.max_error = Some(e),
        }
        let prev = levels.last().expect("level 0 exists");
        let result = simplify(&prev.mesh, &opts)?;
        let error = mesh_error(mesh, &result.mesh)?.max(prev.error);
        levels.push(LodLevel {
            triangle_count: result.final_triangles,
            mesh: result.mesh,
            error,
        });
    }
    Ok(LodChain { levels, bounds })
}

/// Camera and quality settings for screen-space selection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LodSelectionParams {
    /// Vertical field of view in radians, in `(0, pi)`.
    pub vertical_fov: f32,
    /// Viewport height in pixels.
    pub viewport_height: f32,
    /// Largest tolerated projected error, in pixels.
    pub pixel_threshold: f32,
    /// Relative dead band in `[0, 1)` around the threshold to prevent popping.
    pub hysteresis: f32,
}

impl LodSelectionParams {
    fn check(&self) -> bool {
        self.vertical_fov.is_finite()
            && self.vertical_fov > 0.0
            && self.vertical_fov < std::f32::consts::PI
            && self.viewport_height.is_finite()
            && self.viewport_height > 0.0
            && self.pixel_threshold.is_finite()
            && self.pixel_threshold >= 0.0
            && (0.0..1.0).contains(&self.hysteresis)
    }
}

/// Smallest distance used in projections so the result stays finite.
const MIN_DISTANCE: f32 = 1e-4;

/// Pixels covered by `error` object units at `distance` from the camera.
pub fn projected_error_pixels(error: f32, distance: f32, vertical_fov: f32, viewport_height: f32) -> f32 {
    let d = distance.max(MIN_DISTANCE);
    error * viewport_height / (2.0 * d * (vertical_fov * 0.5).tan())
}

/// Projected error in pixels of a level with `error`, for an object whose
/// bounding sphere centre is `distance` from the camera.
///
/// Conservative: uses the distance to the nearest point of the sphere
/// (`distance - radius`, floored), so objects close to the camera pick finer levels.
pub fn level_pixel_error(error: f32, sphere: &BoundingSphere, distance: f32, p: &LodSelectionParams) -> f32 {
    projected_error_pixels(error, distance - sphere.radius, p.vertical_fov, p.viewport_height)
}

/// Picks the coarsest level whose projected error fits the pixel threshold.
///
/// `errors` must be non-decreasing (level 0 first). `previous` enables
/// hysteresis: the level only becomes coarser when it fits `threshold *
/// (1 - h)` and only becomes finer when the current level exceeds
/// `threshold * (1 + h)`. Invalid parameters or empty `errors` return 0.
pub fn select_lod(
    errors: &[f32],
    sphere: &BoundingSphere,
    distance: f32,
    params: &LodSelectionParams,
    previous: Option<usize>,
) -> usize {
    if errors.is_empty() || !params.check() || !distance.is_finite() {
        return 0;
    }
    let px = |i: usize| level_pixel_error(errors[i], sphere, distance, params);
    let h = params.hysteresis;
    let thr = params.pixel_threshold;
    let coarsest_within = |limit: f32| (0..errors.len()).rev().find(|&i| px(i) <= limit).unwrap_or(0);
    match previous {
        Some(p) if p < errors.len() => {
            if px(p) > thr * (1.0 + h) {
                coarsest_within(thr)
            } else {
                let coarser = coarsest_within(thr * (1.0 - h));
                coarser.max(p)
            }
        }
        _ => coarsest_within(thr),
    }
}

/// Fraction of the viewport height covered by the bounding-sphere diameter
/// (Unity `LODGroup` "relative screen height").
pub fn screen_height_fraction(sphere: &BoundingSphere, distance: f32, vertical_fov: f32) -> f32 {
    let d = distance.max(MIN_DISTANCE);
    sphere.radius / (d * (vertical_fov * 0.5).tan())
}

/// Screen-size thresholds in the style of Unity `LODGroup`.
#[derive(Debug, Clone, PartialEq)]
pub struct ScreenSizeLods {
    /// `thresholds[i]` is the smallest screen-height fraction at which level `i`
    /// is still used. Must be strictly decreasing and in `(0, 1]`.
    pub thresholds: Vec<f32>,
    /// Below this fraction the object is culled (`None` from the selector).
    pub cull_below: Option<f32>,
    /// Relative dead band in `[0, 1)`.
    pub hysteresis: f32,
}

impl ScreenSizeLods {
    /// Validates ordering and ranges.
    pub fn validate(&self) -> Result<()> {
        let bad = |reason: &str| {
            Err(MeshLodError::InvalidOption {
                reason: reason.into(),
            })
        };
        if self.thresholds.is_empty() {
            return bad("thresholds must not be empty");
        }
        if self.thresholds.iter().any(|t| !(t.is_finite() && *t > 0.0 && *t <= 1.0)) {
            return bad("thresholds must be in (0, 1]");
        }
        if self.thresholds.windows(2).any(|w| w[1] >= w[0]) {
            return bad("thresholds must be strictly decreasing");
        }
        if !(0.0..1.0).contains(&self.hysteresis) {
            return bad("hysteresis must be in [0, 1)");
        }
        if let Some(c) = self.cull_below
            && !(c.is_finite() && c >= 0.0 && c < *self.thresholds.last().unwrap_or(&1.0))
        {
            return bad("cull_below must be >= 0 and below the last threshold");
        }
        Ok(())
    }

    fn raw_level(&self, fraction: f32) -> usize {
        self.thresholds
            .iter()
            .position(|&t| fraction >= t)
            .unwrap_or(self.thresholds.len() - 1)
    }

    /// Chooses a level for a screen-height `fraction`, or `None` when culled.
    /// `previous` is the last visible level (`None` if culled or unknown).
    /// With `previous`, a level is kept while `fraction` stays within its
    /// band widened by the hysteresis. Invalid configuration returns level 0.
    pub fn select(&self, fraction: f32, previous: Option<usize>) -> Option<usize> {
        if self.validate().is_err() || !fraction.is_finite() {
            return Some(0);
        }
        let h = self.hysteresis;
        if let Some(c) = self.cull_below {
            // A visible object is culled only below c*(1-h); a culled or new one
            // becomes visible at c.
            let limit = if previous.is_some() { c * (1.0 - h) } else { c };
            if fraction < limit {
                return None;
            }
        }
        let raw = self.raw_level(fraction);
        if let Some(p) = previous
            && p < self.thresholds.len()
        {
            let lower = self.thresholds[p] * (1.0 - h);
            let upper = if p == 0 {
                f32::INFINITY
            } else {
                self.thresholds[p - 1] * (1.0 + h)
            };
            if fraction >= lower && fraction < upper {
                return Some(p);
            }
        }
        Some(raw)
    }
}
