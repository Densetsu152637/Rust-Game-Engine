//! Per-view CPU mesh LOD selection and the conservative chain bounds the
//! camera visibility path projects. Selection happens before any camera
//! prepass; shadows keep the level-zero geometry (`MeshLods` documents this).

use engine_project::content::Mesh;
use engine_project::rendering3d::MeshDrawOptions3D;
use engine_render_pipeline::ViewId;
use glam::{Mat4, Vec3};
use std::collections::HashMap;

/// A local axis-aligned box as `(min, max)`.
pub(crate) type LocalBounds = (Vec3, Vec3);

/// Local AABB of every finite vertex, or `None` for an empty or
/// nonfinite mesh (which the visibility path then treats as always visible).
pub(crate) fn mesh_bounds(mesh: &Mesh) -> Option<LocalBounds> {
    let mut points = mesh.vertices.iter().map(|v| Vec3::from_array(v.position));
    let first = points.next()?;
    let (mut min, mut max) = (first, first);
    for point in std::iter::once(first).chain(points) {
        if !point.is_finite() {
            return None;
        }
        min = min.min(point);
        max = max.max(point);
    }
    Some((min, max))
}

/// The union of every level's bounds, so a bound enclosing the chain stays
/// valid whichever level is selected. Any level without bounds makes the
/// whole chain unbounded.
pub(crate) fn chain_bounds(levels: &[Option<LocalBounds>]) -> Option<LocalBounds> {
    let mut union: Option<LocalBounds> = None;
    for level in levels {
        let (min, max) = (*level)?;
        union = Some(match union {
            None => (min, max),
            Some((lo, hi)) => (lo.min(min), hi.max(max)),
        });
    }
    union
}

/// Fraction of the viewport height covered by the world bounding sphere's
/// diameter (Unity `LODGroup` relative screen height). Perspective and
/// orthographic projections are told apart by `clip_from_view`.
pub(crate) fn screen_fraction(
    clip_from_view: &Mat4,
    camera: Vec3,
    center: Vec3,
    radius: f32,
) -> f32 {
    let scale = clip_from_view.y_axis.y;
    let orthographic = clip_from_view.w_axis.w != 0.0;
    if orthographic {
        radius * scale
    } else {
        radius * scale / (center.distance(camera)).max(1e-4)
    }
}

/// World bounding sphere of a local AABB under `world_from_local`; the radius
/// grows by the largest axis scale.
pub(crate) fn world_sphere(bounds: (Vec3, Vec3), world_from_local: &Mat4) -> (Vec3, f32) {
    let center = world_from_local.transform_point3((bounds.0 + bounds.1) * 0.5);
    let scale = world_from_local
        .x_axis
        .truncate()
        .length()
        .max(world_from_local.y_axis.truncate().length())
        .max(world_from_local.z_axis.truncate().length());
    (center, (bounds.1 - bounds.0).length() * 0.5 * scale)
}

/// The level a draw uses: the authored policy's choice with hysteresis against
/// `previous`, clamped to the available levels.
pub(crate) fn select_level(
    options: &MeshDrawOptions3D,
    fraction: f32,
    previous: Option<usize>,
) -> usize {
    options
        .policy
        .select(fraction, previous)
        .unwrap_or(0)
        .min(options.levels.len())
}

/// Hysteresis memory: the last selected level per view and draw ID.
#[derive(Default)]
pub(crate) struct LodSelections {
    views: HashMap<ViewId, HashMap<String, usize>>,
}

impl LodSelections {
    /// Take a view's memory for this frame; `reset` (a camera cut) forgets it.
    pub(crate) fn take(&mut self, view: ViewId, reset: bool) -> HashMap<String, usize> {
        let previous = self.views.remove(&view).unwrap_or_default();
        if reset { HashMap::new() } else { previous }
    }

    /// Replace the view's memory with this frame's selections. IDs absent
    /// from the frame are forgotten.
    pub(crate) fn store(&mut self, view: ViewId, selected: HashMap<String, usize>) {
        self.views.insert(view, selected);
    }

    #[cfg(test)]
    pub(crate) fn level(&self, view: ViewId, id: &str) -> Option<usize> {
        self.views.get(&view)?.get(id).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_mesh_lod::ScreenSizeLods;
    use engine_project::content::Vertex;
    use std::sync::Arc;

    fn mesh(points: &[[f32; 3]]) -> Mesh {
        Mesh {
            uv2: vec![],
            normals: vec![],
            vertices: points
                .iter()
                .map(|&position| Vertex {
                    position,
                    uv: [0.0; 2],
                    color: [1.0; 4],
                })
                .collect(),
            indices: vec![],
        }
    }

    fn options(levels: usize) -> MeshDrawOptions3D {
        MeshDrawOptions3D {
            levels: (0..levels).map(|_| Arc::new(mesh(&[[0.0; 3]]))).collect(),
            policy: ScreenSizeLods {
                thresholds: vec![0.5, 0.2],
                cull_below: None,
                hysteresis: 0.1,
            },
            occluder: false,
        }
    }

    #[test]
    fn bounds_cover_every_level_and_nonfinite_input_is_unbounded() {
        let fine = mesh_bounds(&mesh(&[[-1.0, 0.0, 0.0], [1.0, 1.0, 1.0]]));
        let coarse = mesh_bounds(&mesh(&[[-2.0, -1.0, 0.0], [0.5, 0.5, 3.0]]));
        let (min, max) = chain_bounds(&[fine, coarse]).unwrap();
        assert_eq!(min, Vec3::new(-2.0, -1.0, 0.0));
        assert_eq!(max, Vec3::new(1.0, 1.0, 3.0));
        assert!(chain_bounds(&[fine, None]).is_none());
        assert!(mesh_bounds(&mesh(&[[f32::NAN, 0.0, 0.0]])).is_none());
        assert!(mesh_bounds(&mesh(&[])).is_none());
    }

    #[test]
    fn screen_fraction_matches_perspective_and_orthographic_geometry() {
        let fov = std::f32::consts::FRAC_PI_2;
        let perspective = glam::camera::rh::proj::directx::perspective(fov, 1.0, 0.1, 100.0);
        // tan(45 deg) = 1: a unit radius at distance 4 spans a quarter of the height.
        let fraction = screen_fraction(&perspective, Vec3::ZERO, Vec3::new(0.0, 0.0, -4.0), 1.0);
        assert!((fraction - 0.25).abs() < 1e-6);
        let orthographic =
            glam::camera::rh::proj::directx::orthographic(-4.0, 4.0, -4.0, 4.0, 0.1, 100.0);
        let fraction = screen_fraction(&orthographic, Vec3::ZERO, Vec3::new(0.0, 0.0, -50.0), 2.0);
        assert!((fraction - 0.5).abs() < 1e-6, "diameter 4 of height 8");
    }

    #[test]
    fn world_sphere_uses_the_largest_axis_scale() {
        let model = Mat4::from_scale(Vec3::new(1.0, 3.0, 2.0)) * Mat4::from_translation(Vec3::X);
        let (center, radius) = world_sphere((Vec3::splat(-1.0), Vec3::splat(1.0)), &model);
        assert!(center.abs_diff_eq(Vec3::new(1.0, 0.0, 0.0), 1e-6));
        assert!((radius - 3.0_f32.sqrt() * 3.0).abs() < 1e-5);
    }

    #[test]
    fn selection_keeps_a_level_inside_its_hysteresis_band_and_clamps() {
        let options = options(1);
        // Thresholds [0.5, 0.2], h = 0.1: level 0 is kept down to 0.45.
        assert_eq!(select_level(&options, 0.55, None), 0);
        assert_eq!(select_level(&options, 0.48, Some(0)), 0);
        assert_eq!(select_level(&options, 0.44, Some(0)), 1);
        // Coarser levels return only above 0.5 * 1.1.
        assert_eq!(select_level(&options, 0.52, Some(1)), 1);
        assert_eq!(select_level(&options, 0.56, Some(1)), 0);
        // A policy with more thresholds than loaded levels never overruns.
        assert_eq!(select_level(&options, 0.01, None), 1);
    }

    #[test]
    fn memory_resets_on_cuts_and_forgets_absent_draws() {
        let mut memory = LodSelections::default();
        let view = ViewId::MAIN;
        let previous = memory.take(view, false);
        assert!(previous.is_empty());
        memory.store(
            view,
            HashMap::from([("a".to_owned(), 1), ("b".to_owned(), 2)]),
        );
        assert_eq!(memory.level(view, "a"), Some(1));
        let carried = memory.take(view, false);
        assert_eq!(carried.get("b"), Some(&2));
        memory.store(view, HashMap::from([("a".to_owned(), 0)]));
        assert_eq!(memory.level(view, "b"), None);
        assert!(memory.take(view, true).is_empty());
        let other = ViewId(77);
        memory.store(other, HashMap::from([("a".to_owned(), 2)]));
        assert_eq!(memory.level(other, "a"), Some(2));
    }
}
