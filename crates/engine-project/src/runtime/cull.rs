//! Culling before copying: the meshes of a 3D frame the active camera cannot see are rejected
//! from a cached bounding box before any of their component data is read or copied.
//!
//! The box of a mesh is computed once per mesh identity (the `Arc`), in local space; an
//! entity's world box is that box under the entity's world matrix (the eight corners are not
//! transformed, the box is: Arvo's method). The rejection is conservative:
//!
//! * the frustum is that of the camera widened to [`MAX_ASPECT`] (the extraction does not
//!   know the output size) and pushed out by [`SLACK`] metres;
//! * a mesh outside the frustum is kept when it can cast a shadow into it: for each light with
//!   shadows, the ray from the mesh away from the light (as long as the light's range, or
//!   unbounded for a directional light) is tested against the frustum widened by the mesh's
//!   radius;
//! * a mesh with a custom material shader is never rejected, because the shader may move its
//!   vertices outside the box;
//! * nothing is rejected under a camera with ray tracing, in an XR session, or after
//!   [`Runtime::set_frustum_culling`] turned the culling off (a host that renders the frame
//!   from other views than the active camera's, such as an editor viewport, does that).
use super::*;
use crate::lights::{LightInstance3D, SceneLight};
use crate::rendering3d::{Camera3D, Projection3D};
use glam::{Mat4, Vec3, Vec4};
use std::collections::HashMap;

/// The widest output aspect (width over height) the frustum is built for.
pub(super) const MAX_ASPECT: f32 = 4.0;
/// Metres the frustum planes are pushed outward.
const SLACK: f32 = 0.05;

/// Axis-aligned box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}
impl Aabb {
    /// The box of the vertex positions; `None` for no vertex or a non-finite one.
    pub fn of(mesh: &Mesh) -> Option<Self> {
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
        Some(Self { min, max })
    }
    pub fn union(self, other: Self) -> Self {
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }
    /// The box of this box under `matrix`.
    pub fn transformed(&self, matrix: &Mat4) -> Self {
        let center = (self.min + self.max) * 0.5;
        let extent = (self.max - self.min) * 0.5;
        let center = matrix.transform_point3(center);
        let axes = [matrix.x_axis, matrix.y_axis, matrix.z_axis];
        let mut half = Vec3::ZERO;
        for (axis, e) in axes.iter().zip(extent.to_array()) {
            half += axis.truncate().abs() * e;
        }
        Self {
            min: center - half,
            max: center + half,
        }
    }
    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }
    pub fn radius(&self) -> f32 {
        (self.max - self.min).length() * 0.5
    }
}

/// The planes of the view frustum, normals pointing inward.
pub(super) struct Frustum {
    planes: [Vec4; 6],
}
impl Frustum {
    /// The frustum of `camera` at world matrix `world`, for an output up to [`MAX_ASPECT`]
    /// wide; `None` when the matrices are unusable (nothing is then rejected).
    pub fn of(camera: &Camera3D, world: &Mat4) -> Option<Self> {
        if !world.is_finite() || world.determinant().abs() < 1e-8 {
            return None;
        }
        let projection = match camera.projection {
            Projection3D::Perspective {
                fov_y_radians,
                near,
                far,
            } => glam::camera::rh::proj::directx::perspective(fov_y_radians, MAX_ASPECT, near, far),
            Projection3D::Orthographic { height, near, far } => {
                glam::camera::rh::proj::directx::orthographic(
                    -height * MAX_ASPECT * 0.5,
                    height * MAX_ASPECT * 0.5,
                    -height * 0.5,
                    height * 0.5,
                    near,
                    far,
                )
            }
        };
        let clip = projection * world.inverse();
        if !clip.is_finite() {
            return None;
        }
        let (r0, r1, r2, r3) = (clip.row(0), clip.row(1), clip.row(2), clip.row(3));
        // Clip volume of a 0..1 depth range: -w <= x, y <= w and 0 <= z <= w.
        let raw = [r3 + r0, r3 - r0, r3 + r1, r3 - r1, r2, r3 - r2];
        let mut planes = [Vec4::ZERO; 6];
        for (plane, raw) in planes.iter_mut().zip(raw) {
            let length = raw.truncate().length();
            if length < 1e-12 {
                return None;
            }
            *plane = raw / length + Vec4::new(0.0, 0.0, 0.0, SLACK);
        }
        Some(Self { planes })
    }
    /// Whether the box touches the frustum (conservative).
    pub fn intersects(&self, aabb: &Aabb) -> bool {
        self.planes.iter().all(|plane| {
            let normal = plane.truncate();
            let far_corner = Vec3::select(normal.cmpge(Vec3::ZERO), aabb.max, aabb.min);
            normal.dot(far_corner) + plane.w >= 0.0
        })
    }
    /// Whether the ray `origin + t * direction`, `0 <= t <= length`, touches the frustum
    /// widened by `radius` (a sphere around `origin` swept along the ray).
    pub fn ray_intersects(&self, origin: Vec3, direction: Vec3, length: f32, radius: f32) -> bool {
        let (mut enter, mut exit) = (0.0_f32, length);
        for plane in &self.planes {
            let normal = plane.truncate();
            let distance = normal.dot(origin) + plane.w + radius;
            let speed = normal.dot(direction);
            if speed.abs() < 1e-9 {
                if distance < 0.0 {
                    return false;
                }
                continue;
            }
            let t = -distance / speed;
            if speed > 0.0 {
                enter = enter.max(t);
            } else {
                exit = exit.min(t);
            }
            if enter > exit {
                return false;
            }
        }
        true
    }
}

/// A light that casts shadows, as the culling sees it.
struct Caster {
    /// Where a local light is; `None` for a directional light.
    position: Option<Vec3>,
    /// The way light travels at a point (for a local light, away from `position`).
    direction: Vec3,
    range: f32,
}

/// What one frame's culling needs.
pub(super) struct Culler {
    frustum: Frustum,
    casters: Vec<Caster>,
}
impl Culler {
    /// `None` when the frame is not culled (see the module documentation).
    pub fn new(
        camera: &Camera3D,
        world: &Mat4,
        lights: &[LightInstance3D],
        rejected: bool,
    ) -> Option<Self> {
        if rejected {
            return None;
        }
        let frustum = Frustum::of(camera, world)?;
        let mut casters = Vec::new();
        for light in lights {
            let matrix = Mat4::from_cols_array_2d(&light.world);
            let position = matrix.w_axis.truncate();
            // A light shines along its local -Z.
            let toward = matrix.z_axis.truncate().normalize_or_zero();
            let (shadows, range, local) = match &light.light {
                SceneLight::Directional(l) => (l.shadows.enabled, f32::INFINITY, false),
                SceneLight::Point(l) => (l.shadows.enabled, l.range, true),
                SceneLight::Spot(l) => (l.shadows.enabled, l.range, true),
                SceneLight::Area(l) => (l.shadows.enabled, l.range, true),
            };
            if !shadows {
                continue;
            }
            if !(position.is_finite() && toward.is_finite()) {
                return None;
            }
            casters.push(Caster {
                position: local.then_some(position),
                direction: -toward,
                range,
            });
        }
        Some(Self { frustum, casters })
    }
    /// What the culling does with a mesh whose world box is `aabb`.
    pub fn decide(&self, aabb: &Aabb, casts_shadows: bool) -> Verdict {
        if self.frustum.intersects(aabb) {
            return Verdict::Visible;
        }
        if casts_shadows {
            let (center, radius) = (aabb.center(), aabb.radius());
            for caster in &self.casters {
                let (direction, length) = match caster.position {
                    // Light reaches `range` from a local light; a shadow starts at the caster.
                    Some(position) => {
                        let offset = center - position;
                        let distance = offset.length();
                        if distance > caster.range + radius {
                            continue;
                        }
                        let direction = if distance > 1e-6 {
                            offset / distance
                        } else {
                            // The light is inside the box: it casts every way.
                            return Verdict::CastsIntoView;
                        };
                        (direction, caster.range)
                    }
                    None => (caster.direction, f32::INFINITY),
                };
                if self
                    .frustum
                    .ray_intersects(center, direction, length, radius)
                {
                    return Verdict::CastsIntoView;
                }
            }
        }
        Verdict::Outside
    }
}

/// The outcome of the culling test of one mesh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    Visible,
    /// Outside the frustum, kept for the shadow it can cast into it.
    CastsIntoView,
    Outside,
}

/// What the last [`Runtime::extract_scene_frame`] did with the mesh renderers, stage by stage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExtractionStats {
    /// Entities with a `MeshRenderer`.
    pub meshes: usize,
    /// Rejected because the 3D pass does not draw them: no `Transform`, the intact mesh of a
    /// fractured entity whose fragments draw, or a generated chunk drawn by the lit 2D pass.
    pub not_3d: usize,
    /// Rejected because they are outside the camera's frustum and cast no shadow into it.
    pub outside_frustum: usize,
    /// Outside the frustum but kept for a shadow they can cast into it.
    pub kept_for_shadow: usize,
    /// Meshes the frame carries (`SceneRenderFrame::meshes`).
    pub drawn: usize,
    /// The frame was culled against the camera (otherwise nothing was rejected by view).
    pub culled: bool,
}

/// Local boxes by mesh identity. The cache holds the `Arc`, so the address is never reused
/// for another mesh while the entry lives and the mesh cannot change in place.
#[derive(Default)]
pub(super) struct MeshBounds(HashMap<usize, (Arc<Mesh>, Option<Aabb>)>);
impl MeshBounds {
    pub fn of(&mut self, mesh: &Arc<Mesh>) -> Option<Aabb> {
        self.0
            .entry(Arc::as_ptr(mesh) as usize)
            .or_insert_with(|| (mesh.clone(), Aabb::of(mesh)))
            .1
    }
    /// Forget the meshes nothing else holds any more.
    pub fn sweep(&mut self) {
        self.0.retain(|_, (mesh, _)| Arc::strong_count(mesh) > 1);
    }
}

impl Runtime {
    /// Turn the culling of `extract_scene_frame` on (the default) or off. With it on, the frame
    /// carries only the meshes the active `Camera3D` can see (and those casting a shadow into
    /// its view); a host that draws the frame from a camera other than the active one (an
    /// editor viewport, XR eyes) turns it off.
    pub fn set_frustum_culling(&self, enabled: bool) {
        self.frustum_culling.set(enabled);
    }
    /// What the last `extract_scene_frame` did with every mesh renderer.
    pub fn extraction_stats(&self) -> ExtractionStats {
        self.stats.get()
    }
}
