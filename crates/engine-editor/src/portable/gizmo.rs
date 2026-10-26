//! Translate/rotate/scale manipulators: handle geometry, hit testing, drag math
//! and the application of a gesture to entity transforms. Everything except
//! [`draw`] is pure so numeric behaviour is unit tested without a window.
use super::viewport::{Ray, ViewMath};
use eframe::egui::{self, Color32, Pos2, Stroke};
use engine_editor_settings::{GizmoMode, GizmoSpace, Snapping};
use engine_project::Transform;
use glam::{Mat4, Quat, Vec3};

/// Gizmo size on screen, in egui points.
pub const GIZMO_PIXELS: f32 = 90.0;
const HIT_PIXELS: f32 = 7.0;
const RING_SEGMENTS: usize = 64;
const MIN_SCALE_FACTOR: f32 = 0.001;
const MAX_SCALE_FACTOR: f32 = 1000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    /// Camera-plane translation (centre disc).
    Free,
    /// Translate or scale along basis axis `n`.
    Axis(usize),
    /// Translate in the plane whose normal is basis axis `n`.
    Plane(usize),
    /// Rotate about basis axis `n`.
    Ring(usize),
    /// Uniform scale (centre disc).
    Uniform,
}

/// Pivot and orthonormal axes of the manipulator, in world space.
#[derive(Clone, Copy, Debug)]
pub struct Basis {
    pub pivot: Vec3,
    pub axes: [Vec3; 3],
}
impl Basis {
    pub fn world(pivot: Vec3) -> Self {
        Self {
            pivot,
            axes: [Vec3::X, Vec3::Y, Vec3::Z],
        }
    }
    pub fn new(space: GizmoSpace, pivot: Vec3, active_world: Option<&Mat4>) -> Self {
        match (space, active_world) {
            (GizmoSpace::Local, Some(world)) => {
                let rotation = rotation_of(world);
                Self {
                    pivot,
                    axes: [rotation * Vec3::X, rotation * Vec3::Y, rotation * Vec3::Z],
                }
            }
            _ => Self::world(pivot),
        }
    }
}

/// Rotation part of a world matrix; identity when the matrix is degenerate.
pub fn rotation_of(matrix: &Mat4) -> Quat {
    if !matrix.is_finite() || matrix.determinant().abs() < 1e-12 {
        return Quat::IDENTITY;
    }
    let (_, rotation, _) = matrix.to_scale_rotation_translation();
    if rotation.is_finite() {
        rotation.normalize()
    } else {
        Quat::IDENTITY
    }
}

/// The transform state of one manipulated entity at gesture start.
#[derive(Clone, Debug)]
pub struct EntityStart {
    pub id: String,
    pub local: Transform,
    pub world: Mat4,
    /// World matrix of the parent (identity for roots).
    pub parent_world: Mat4,
}

/// A world-space change produced by a gesture.
#[derive(Clone, Debug, PartialEq)]
pub enum Delta {
    Translate(Vec3),
    Rotate {
        axis: Vec3,
        radians: f32,
    },
    /// `axis: None` scales uniformly.
    Scale {
        axis: Option<Vec3>,
        factor: f32,
    },
}

fn snap(value: f32, step: f32) -> f32 {
    if step > 0.0 {
        (value / step).round() * step
    } else {
        value
    }
}

/// Parameter of the point on the line `origin + t * axis` nearest to `ray`.
pub fn axis_parameter(origin: Vec3, axis: Vec3, ray: &Ray) -> Option<f32> {
    let w = origin - ray.origin;
    let b = axis.dot(ray.dir);
    let denominator = 1.0 - b * b;
    if denominator.abs() < 1e-4 {
        return None;
    }
    Some((b * ray.dir.dot(w) - axis.dot(w)) / denominator).filter(|t| t.is_finite())
}

/// Intersection of `ray` with the plane through `point` with `normal`.
pub fn plane_hit(point: Vec3, normal: Vec3, ray: &Ray) -> Option<Vec3> {
    let denominator = normal.dot(ray.dir);
    if denominator.abs() < 1e-4 {
        return None;
    }
    let t = normal.dot(point - ray.origin) / denominator;
    (t.is_finite() && t > 0.0).then(|| ray.origin + ray.dir * t)
}

fn wrap_angle(angle: f32) -> f32 {
    let tau = std::f32::consts::TAU;
    let mut a = (angle + std::f32::consts::PI).rem_euclid(tau) - std::f32::consts::PI;
    if a <= -std::f32::consts::PI {
        a += tau;
    }
    a
}

#[derive(Clone, Copy, Debug)]
enum Start {
    Screen(Pos2),
    Parameter(f32),
    Point(Vec3),
    Angle,
}

/// An active drag on one handle.
#[derive(Clone, Debug)]
pub struct Gesture {
    pub handle: Handle,
    pub mode: GizmoMode,
    pub basis: Basis,
    length: f32,
    start: Start,
    last_angle: f32,
    accumulated: f32,
}

fn plane_axes(index: usize) -> (usize, usize) {
    ((index + 1) % 3, (index + 2) % 3)
}

impl Gesture {
    /// Starts a drag. `None` when the ray cannot address the handle (edge-on).
    pub fn begin(
        handle: Handle,
        mode: GizmoMode,
        basis: Basis,
        ray: &Ray,
        press: Pos2,
        view: &ViewMath,
    ) -> Option<Self> {
        let length = GIZMO_PIXELS * view.world_per_pixel(basis.pivot);
        let (start, angle) = match handle {
            Handle::Free | Handle::Uniform => (Start::Screen(press), 0.0),
            Handle::Axis(i) => (
                Start::Parameter(axis_parameter(basis.pivot, basis.axes[i], ray)?),
                0.0,
            ),
            Handle::Plane(i) => (
                Start::Point(plane_hit(basis.pivot, basis.axes[i], ray)?),
                0.0,
            ),
            Handle::Ring(i) => {
                let hit = plane_hit(basis.pivot, basis.axes[i], ray)?;
                let angle = ring_angle(&basis, i, hit);
                (Start::Angle, angle)
            }
        };
        Some(Self {
            handle,
            mode,
            basis,
            length,
            start,
            last_angle: angle,
            accumulated: 0.0,
        })
    }

    /// The (snapped) world change for the pointer's current ray. Rotation
    /// accumulates across frames so winding past +-pi keeps counting.
    pub fn delta(
        &mut self,
        ray: &Ray,
        pointer: Pos2,
        view: &ViewMath,
        snapping: &Snapping,
    ) -> Option<Delta> {
        let basis = self.basis;
        match (self.handle, self.start) {
            (Handle::Free, Start::Screen(origin)) => {
                let motion = glam::Vec2::new(pointer.x - origin.x, pointer.y - origin.y);
                let world = view.screen_to_world(motion, basis.pivot);
                Some(Delta::Translate(self.snap_vector(world, snapping)))
            }
            (Handle::Axis(i), Start::Parameter(start)) => {
                let now = axis_parameter(basis.pivot, basis.axes[i], ray)?;
                if self.mode == GizmoMode::Scale {
                    let mut change = (now - start) / self.length;
                    if snapping.enabled {
                        change = snap(change, snapping.scale_step);
                    }
                    let factor = (1.0 + change).clamp(MIN_SCALE_FACTOR, MAX_SCALE_FACTOR);
                    Some(Delta::Scale {
                        axis: Some(basis.axes[i]),
                        factor,
                    })
                } else {
                    let mut distance = now - start;
                    if snapping.enabled {
                        distance = snap(distance, snapping.translate_step);
                    }
                    Some(Delta::Translate(basis.axes[i] * distance))
                }
            }
            (Handle::Plane(i), Start::Point(start)) => {
                let now = plane_hit(basis.pivot, basis.axes[i], ray)?;
                Some(Delta::Translate(self.snap_vector(now - start, snapping)))
            }
            (Handle::Ring(i), Start::Angle) => {
                let hit = plane_hit(basis.pivot, basis.axes[i], ray)?;
                let angle = ring_angle(&basis, i, hit);
                self.accumulated += wrap_angle(angle - self.last_angle);
                self.last_angle = angle;
                let mut radians = self.accumulated;
                if snapping.enabled {
                    radians = snap(radians, snapping.rotate_degrees.to_radians());
                }
                Some(Delta::Rotate {
                    axis: basis.axes[i],
                    radians,
                })
            }
            (Handle::Uniform, Start::Screen(origin)) => {
                let mut change = ((pointer.x - origin.x) - (pointer.y - origin.y)) / 200.0;
                if snapping.enabled {
                    change = snap(change, snapping.scale_step);
                }
                Some(Delta::Scale {
                    axis: None,
                    factor: (1.0 + change).clamp(MIN_SCALE_FACTOR, MAX_SCALE_FACTOR),
                })
            }
            _ => None,
        }
    }

    /// Snaps a world vector per basis axis when snapping is on.
    fn snap_vector(&self, vector: Vec3, snapping: &Snapping) -> Vec3 {
        if !snapping.enabled {
            return vector;
        }
        let axes = self.basis.axes;
        axes.iter()
            .map(|axis| *axis * snap(vector.dot(*axis), snapping.translate_step))
            .sum()
    }
}

fn ring_angle(basis: &Basis, index: usize, hit: Vec3) -> f32 {
    let (j, k) = plane_axes(index);
    let offset = hit - basis.pivot;
    offset.dot(basis.axes[k]).atan2(offset.dot(basis.axes[j]))
}

/// Applies a gesture result to every start state, returning the new local
/// transforms in the same order.
pub fn apply(
    delta: &Delta,
    pivot: Vec3,
    starts: &[EntityStart],
    three_dimensional: bool,
) -> Vec<Transform> {
    starts
        .iter()
        .map(|start| apply_one(delta, pivot, start, three_dimensional))
        .collect()
}

fn inverse_or_identity(matrix: &Mat4) -> Mat4 {
    if matrix.is_finite() && matrix.determinant().abs() > 1e-8 {
        matrix.inverse()
    } else {
        Mat4::IDENTITY
    }
}

fn apply_one(
    delta: &Delta,
    pivot: Vec3,
    start: &EntityStart,
    three_dimensional: bool,
) -> Transform {
    let mut transform = start.local.clone();
    let unchanged = match delta {
        Delta::Translate(vector) => *vector == Vec3::ZERO,
        Delta::Rotate { radians, .. } => *radians == 0.0,
        Delta::Scale { factor, .. } => *factor == 1.0,
    };
    if unchanged {
        return transform;
    }
    let parent_inverse = inverse_or_identity(&start.parent_world);
    let world_position = start.world.w_axis.truncate();
    // World displacement of the origin, converted to a local displacement so an
    // unmoved origin leaves the authored position bit-identical.
    let displacement = match delta {
        Delta::Translate(vector) => *vector,
        Delta::Rotate { axis, radians } => {
            Quat::from_axis_angle(axis.normalize_or(Vec3::Z), *radians) * (world_position - pivot)
                + pivot
                - world_position
        }
        Delta::Scale {
            axis: Some(axis),
            factor,
        } => {
            let axis = axis.normalize_or(Vec3::X);
            axis * ((factor - 1.0) * (world_position - pivot).dot(axis))
        }
        Delta::Scale { axis: None, factor } => (world_position - pivot) * (factor - 1.0),
    };
    if displacement != Vec3::ZERO {
        let local = parent_inverse.transform_vector3(displacement);
        for (value, change) in transform.position.iter_mut().zip(local.to_array()) {
            *value += change;
        }
    }
    match delta {
        Delta::Translate(_) => {}
        Delta::Rotate { axis, radians } => {
            let world_rotation = Quat::from_axis_angle(axis.normalize_or(Vec3::Z), *radians);
            let parent_rotation = rotation_of(&start.parent_world);
            let local_change = parent_rotation.inverse() * world_rotation * parent_rotation;
            let rotation =
                (local_change * Quat::from_array(transform.rotation).normalize()).normalize();
            transform.rotation = rotation.to_array();
        }
        Delta::Scale { axis, factor } => {
            let components: Vec<usize> = match axis {
                None if three_dimensional => vec![0, 1, 2],
                None => vec![0, 1],
                Some(axis) => {
                    let rotation = rotation_of(&start.world);
                    let local_axes = [rotation * Vec3::X, rotation * Vec3::Y, rotation * Vec3::Z];
                    let best = (0..3)
                        .max_by(|a, b| {
                            axis.dot(local_axes[*a])
                                .abs()
                                .total_cmp(&axis.dot(local_axes[*b]).abs())
                        })
                        .unwrap_or(0);
                    vec![best]
                }
            };
            for component in components {
                transform.scale[component] *= factor;
            }
        }
    }
    transform
}

// ----- geometry, hit testing and drawing -----

struct Part {
    handle: Handle,
    segments: Vec<[Pos2; 2]>,
    /// Filled convex quad (plane handles).
    quad: Option<[Pos2; 4]>,
    /// Filled disc (centre handles).
    disc: Option<(Pos2, f32)>,
    /// Tip marker position for scale handles.
    tip: Option<Pos2>,
    color: Color32,
}

fn axis_color(index: usize) -> Color32 {
    [
        Color32::from_rgb(235, 70, 70),
        Color32::from_rgb(90, 210, 90),
        Color32::from_rgb(80, 130, 245),
    ][index]
}

fn parts(mode: GizmoMode, basis: &Basis, view: &ViewMath) -> Vec<Part> {
    let length = GIZMO_PIXELS * view.world_per_pixel(basis.pivot);
    let pivot = basis.pivot;
    let axes_in_use: &[usize] = if view.three_dimensional {
        &[0, 1, 2]
    } else {
        &[0, 1]
    };
    let mut parts = Vec::new();
    let Some(center) = view.project(pivot) else {
        return parts;
    };
    let line = |a: Vec3, b: Vec3| view.project_segment(a, b).into_iter().collect::<Vec<_>>();
    match mode {
        GizmoMode::Translate | GizmoMode::Scale => {
            for &i in axes_in_use {
                let tip = pivot + basis.axes[i] * length;
                parts.push(Part {
                    handle: Handle::Axis(i),
                    segments: line(pivot, tip),
                    quad: None,
                    disc: None,
                    tip: view.project(tip),
                    color: axis_color(i),
                });
            }
            if mode == GizmoMode::Translate {
                let plane_normals: &[usize] = if view.three_dimensional {
                    &[0, 1, 2]
                } else {
                    &[2]
                };
                for &i in plane_normals {
                    let (j, k) = plane_axes(i);
                    let corner = |a: f32, b: f32| {
                        view.project(pivot + (basis.axes[j] * a + basis.axes[k] * b) * length)
                    };
                    if let (Some(p0), Some(p1), Some(p2), Some(p3)) = (
                        corner(0.25, 0.25),
                        corner(0.55, 0.25),
                        corner(0.55, 0.55),
                        corner(0.25, 0.55),
                    ) {
                        parts.push(Part {
                            handle: Handle::Plane(i),
                            segments: vec![],
                            quad: Some([p0, p1, p2, p3]),
                            disc: None,
                            tip: None,
                            color: axis_color(i),
                        });
                    }
                }
            }
            parts.push(Part {
                handle: if mode == GizmoMode::Translate {
                    Handle::Free
                } else {
                    Handle::Uniform
                },
                segments: vec![],
                quad: None,
                disc: Some((center, 8.0)),
                tip: None,
                color: Color32::from_gray(225),
            });
        }
        GizmoMode::Rotate => {
            let rings: &[usize] = if view.three_dimensional {
                &[0, 1, 2]
            } else {
                &[2]
            };
            for &i in rings {
                let (j, k) = plane_axes(i);
                let radius = length * 0.85;
                let points: Vec<Vec3> = (0..=RING_SEGMENTS)
                    .map(|step| {
                        let angle = step as f32 / RING_SEGMENTS as f32 * std::f32::consts::TAU;
                        pivot + (basis.axes[j] * angle.cos() + basis.axes[k] * angle.sin()) * radius
                    })
                    .collect();
                let segments = points
                    .windows(2)
                    .filter_map(|pair| view.project_segment(pair[0], pair[1]))
                    .collect();
                parts.push(Part {
                    handle: Handle::Ring(i),
                    segments,
                    quad: None,
                    disc: None,
                    tip: None,
                    color: axis_color(i),
                });
            }
        }
    }
    parts
}

fn segment_distance(point: Pos2, [a, b]: [Pos2; 2]) -> f32 {
    let ab = b - a;
    let length_squared = ab.length_sq();
    if length_squared < 1e-6 {
        return point.distance(a);
    }
    let t = ((point - a).dot(ab) / length_squared).clamp(0.0, 1.0);
    point.distance(a + ab * t)
}

fn inside_quad(point: Pos2, quad: &[Pos2; 4]) -> bool {
    let mut sign = 0.0f32;
    for index in 0..4 {
        let a = quad[index];
        let b = quad[(index + 1) % 4];
        let cross = (b.x - a.x) * (point.y - a.y) - (b.y - a.y) * (point.x - a.x);
        if cross != 0.0 {
            if sign != 0.0 && cross.signum() != sign.signum() {
                return false;
            }
            sign = cross;
        }
    }
    true
}

/// The handle under `pointer`, if any. Centre discs win, then the nearest line.
pub fn hit_test(mode: GizmoMode, basis: &Basis, view: &ViewMath, pointer: Pos2) -> Option<Handle> {
    let mut best: Option<(f32, Handle)> = None;
    for part in parts(mode, basis, view) {
        if let Some((center, radius)) = part.disc
            && pointer.distance(center) <= radius + 2.0
        {
            return Some(part.handle);
        }
        let mut distance = f32::INFINITY;
        if let Some(quad) = &part.quad
            && inside_quad(pointer, quad)
        {
            distance = 0.0;
        }
        for segment in &part.segments {
            distance = distance.min(segment_distance(pointer, *segment));
        }
        if distance <= HIT_PIXELS && best.is_none_or(|(d, _)| distance < d) {
            best = Some((distance, part.handle));
        }
    }
    best.map(|(_, handle)| handle)
}

/// Paints the manipulator over the viewport.
pub fn draw(
    painter: &egui::Painter,
    mode: GizmoMode,
    basis: &Basis,
    view: &ViewMath,
    hovered: Option<Handle>,
    active: Option<Handle>,
) {
    for part in parts(mode, basis, view) {
        let highlighted =
            active == Some(part.handle) || (active.is_none() && hovered == Some(part.handle));
        let color = if highlighted {
            Color32::YELLOW
        } else {
            part.color
        };
        let width = if highlighted { 4.0 } else { 2.5 };
        for [a, b] in &part.segments {
            painter.line_segment([*a, *b], Stroke::new(width, color));
        }
        if let Some(quad) = part.quad {
            painter.add(egui::Shape::convex_polygon(
                quad.to_vec(),
                color.gamma_multiply(if highlighted { 0.8 } else { 0.45 }),
                Stroke::new(1.0, color),
            ));
        }
        if let Some((center, radius)) = part.disc {
            painter.circle_filled(center, radius, color.gamma_multiply(0.8));
        }
        if let Some(tip) = part.tip {
            if mode == GizmoMode::Scale {
                painter.rect_filled(
                    egui::Rect::from_center_size(tip, egui::vec2(9.0, 9.0)),
                    0.0,
                    color,
                );
            } else {
                painter.circle_filled(tip, 4.5, color);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::viewport::{ViewMath, ViewNavigation};
    use super::*;

    fn rect() -> egui::Rect {
        egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(640.0, 360.0))
    }
    fn view2d() -> ViewMath {
        ViewMath::new(&ViewNavigation::default(), rect()).unwrap()
    }
    fn snapping(on: bool) -> Snapping {
        Snapping {
            enabled: on,
            translate_step: 0.5,
            rotate_degrees: 15.0,
            scale_step: 0.25,
        }
    }
    fn start(position: [f32; 3], rotation: Quat, parent: Mat4) -> EntityStart {
        let local = Transform {
            version: 1,
            position,
            rotation: rotation.to_array(),
            scale: [1.0; 3],
        };
        let local_matrix =
            Mat4::from_scale_rotation_translation(Vec3::ONE, rotation, Vec3::from_array(position));
        EntityStart {
            id: "e".into(),
            local,
            world: parent * local_matrix,
            parent_world: parent,
        }
    }
    fn drag(view: &ViewMath, gesture: &mut Gesture, to: Pos2, snapping: &Snapping) -> Delta {
        gesture.delta(&view.ray(to), to, view, snapping).unwrap()
    }
    fn begin(view: &ViewMath, handle: Handle, mode: GizmoMode, basis: Basis, at: Pos2) -> Gesture {
        Gesture::begin(handle, mode, basis, &view.ray(at), at, view).unwrap()
    }

    #[test]
    fn projection_and_ray_round_trip_in_2d_and_3d() {
        let view = view2d();
        let point = view.project(Vec3::new(2.0, 1.0, 0.0)).unwrap();
        assert!((point.x - (320.0 + 32.0)).abs() < 1e-3 && (point.y - (180.0 - 16.0)).abs() < 1e-3);
        let ray = view.ray(point);
        let hit = plane_hit(Vec3::ZERO, Vec3::Z, &ray).unwrap();
        assert!((hit - Vec3::new(2.0, 1.0, 0.0)).length() < 1e-3);
        let nav = ViewNavigation {
            three_dimensional: true,
            yaw: 0.4,
            pitch: -0.3,
            ..Default::default()
        };
        let view = ViewMath::new(&nav, rect()).unwrap();
        let target = Vec3::new(1.0, 0.5, -2.0);
        let ray = view.ray(view.project(target).unwrap());
        let t = (target - ray.origin).dot(ray.dir);
        assert!((ray.origin + ray.dir * t - target).length() < 1e-3);
    }

    #[test]
    fn axis_translate_follows_the_pointer_and_snaps() {
        let view = view2d();
        let basis = Basis::world(Vec3::ZERO);
        let tip = view.project(Vec3::new(5.0, 0.0, 0.0)).unwrap();
        let mut gesture = begin(&view, Handle::Axis(0), GizmoMode::Translate, basis, tip);
        // 40 px at 16 px/unit = 2.5 units; y motion is constrained away.
        let Delta::Translate(vector) = drag(
            &view,
            &mut gesture,
            tip + egui::vec2(40.0, 25.0),
            &snapping(false),
        ) else {
            panic!()
        };
        assert!(
            (vector - Vec3::new(2.5, 0.0, 0.0)).length() < 1e-4,
            "{vector}"
        );
        let Delta::Translate(vector) = drag(
            &view,
            &mut gesture,
            tip + egui::vec2(35.0, 0.0),
            &snapping(true),
        ) else {
            panic!()
        };
        assert!((vector.x - 2.0).abs() < 1e-4 && vector.y == 0.0, "{vector}");
        // A positive half step rounds up: 36/16 = 2.25 -> 2.5.
        let Delta::Translate(vector) = drag(
            &view,
            &mut gesture,
            tip + egui::vec2(36.0, 0.0),
            &snapping(true),
        ) else {
            panic!()
        };
        assert!((vector.x - 2.5).abs() < 1e-4);
    }

    #[test]
    fn plane_translate_moves_in_the_plane_and_snaps_each_axis() {
        let edge_on = ViewMath::new(
            &ViewNavigation {
                three_dimensional: true,
                ..Default::default()
            },
            rect(),
        )
        .unwrap();
        let press = edge_on.project(Vec3::new(0.3, 0.0, 0.3)).unwrap();
        assert!(
            Gesture::begin(
                Handle::Plane(1),
                GizmoMode::Translate,
                Basis::world(Vec3::ZERO),
                &edge_on.ray(press),
                press,
                &edge_on
            )
            .is_none()
        );
        let nav = ViewNavigation {
            three_dimensional: true,
            pitch: -0.4,
            ..Default::default()
        };
        let view = ViewMath::new(&nav, rect()).unwrap();
        let basis = Basis::world(Vec3::ZERO);
        let start = view.project(Vec3::new(0.3, 0.0, 0.3)).unwrap();
        let mut gesture = begin(&view, Handle::Plane(1), GizmoMode::Translate, basis, start);
        let goal = view.project(Vec3::new(1.3, 0.0, -0.7)).unwrap();
        let Delta::Translate(v) = drag(&view, &mut gesture, goal, &snapping(false)) else {
            panic!()
        };
        assert!((v - Vec3::new(1.0, 0.0, -1.0)).length() < 1e-3, "{v}");
        let goal = view.project(Vec3::new(1.2, 0.0, -0.3)).unwrap();
        let Delta::Translate(v) = drag(&view, &mut gesture, goal, &snapping(true)) else {
            panic!()
        };
        assert!((v - Vec3::new(1.0, 0.0, -0.5)).length() < 1e-3, "{v}");
    }

    #[test]
    fn rotate_ring_measures_angle_and_snaps_to_degrees() {
        let view = view2d();
        let basis = Basis::world(Vec3::ZERO);
        let start = view.project(Vec3::new(4.0, 0.0, 0.0)).unwrap();
        let mut gesture = begin(&view, Handle::Ring(2), GizmoMode::Rotate, basis, start);
        // 40 degrees counter-clockwise about +Z.
        let to = view
            .project(Vec3::new(
                4.0 * 40f32.to_radians().cos(),
                4.0 * 40f32.to_radians().sin(),
                0.0,
            ))
            .unwrap();
        let Delta::Rotate { axis, radians } = drag(&view, &mut gesture, to, &snapping(false))
        else {
            panic!()
        };
        assert_eq!(axis, Vec3::Z);
        assert!(
            (radians.to_degrees() - 40.0).abs() < 1e-2,
            "{}",
            radians.to_degrees()
        );
        let Delta::Rotate { radians, .. } = drag(&view, &mut gesture, to, &snapping(true)) else {
            panic!()
        };
        assert!(
            (radians.to_degrees() - 45.0).abs() < 1e-2,
            "{}",
            radians.to_degrees()
        );
    }

    #[test]
    fn scale_axis_and_uniform_factors_snap() {
        let view = view2d();
        let basis = Basis::world(Vec3::ZERO);
        let length = GIZMO_PIXELS / 16.0;
        let tip = view.project(Vec3::new(length, 0.0, 0.0)).unwrap();
        let mut gesture = begin(&view, Handle::Axis(0), GizmoMode::Scale, basis, tip);
        let Delta::Scale { axis, factor } = drag(
            &view,
            &mut gesture,
            tip + egui::vec2(45.0, 0.0),
            &snapping(false),
        ) else {
            panic!()
        };
        assert_eq!(axis, Some(Vec3::X));
        assert!((factor - 1.5).abs() < 1e-4, "{factor}");
        let Delta::Scale { factor, .. } = drag(
            &view,
            &mut gesture,
            tip + egui::vec2(40.0, 0.0),
            &snapping(true),
        ) else {
            panic!()
        };
        assert!((factor - 1.5).abs() < 1e-4, "{factor}");
        let centre = Pos2::new(320.0, 180.0);
        let mut uniform = begin(&view, Handle::Uniform, GizmoMode::Scale, basis, centre);
        let Delta::Scale { axis, factor } = drag(
            &view,
            &mut uniform,
            centre + egui::vec2(100.0, 0.0),
            &snapping(true),
        ) else {
            panic!()
        };
        assert_eq!(axis, None);
        assert!((factor - 1.5).abs() < 1e-4, "{factor}");
    }

    #[test]
    fn hit_testing_prefers_centre_then_nearest_handle() {
        let view = view2d();
        let basis = Basis::world(Vec3::ZERO);
        let centre = Pos2::new(320.0, 180.0);
        assert_eq!(
            hit_test(GizmoMode::Translate, &basis, &view, centre),
            Some(Handle::Free)
        );
        assert_eq!(
            hit_test(GizmoMode::Scale, &basis, &view, centre),
            Some(Handle::Uniform)
        );
        assert_eq!(
            hit_test(
                GizmoMode::Translate,
                &basis,
                &view,
                centre + egui::vec2(60.0, 2.0)
            ),
            Some(Handle::Axis(0))
        );
        assert_eq!(
            hit_test(
                GizmoMode::Translate,
                &basis,
                &view,
                centre + egui::vec2(40.0, -40.0)
            ),
            Some(Handle::Plane(2))
        );
        assert_eq!(
            hit_test(
                GizmoMode::Translate,
                &basis,
                &view,
                centre + egui::vec2(-200.0, 100.0)
            ),
            None
        );
        let ring = GIZMO_PIXELS * 0.85;
        assert_eq!(
            hit_test(
                GizmoMode::Rotate,
                &basis,
                &view,
                centre + egui::vec2(ring, 0.0)
            ),
            Some(Handle::Ring(2))
        );
    }

    #[test]
    fn snapped_noop_preserves_authored_transform_bits() {
        let mut entity = start([2.0, 3.0, 0.0], Quat::IDENTITY, Mat4::IDENTITY);
        entity.local.rotation = [0.0, 0.0, 0.6, 0.79999];
        for delta in [
            Delta::Translate(Vec3::ZERO),
            Delta::Rotate {
                axis: Vec3::Z,
                radians: 0.0,
            },
            Delta::Scale {
                axis: None,
                factor: 1.0,
            },
        ] {
            assert_eq!(
                apply(&delta, Vec3::ZERO, &[entity.clone()], false),
                vec![entity.local.clone()]
            );
        }
    }
    #[test]
    fn world_vs_local_translate_on_a_rotated_parent() {
        let parent = Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let child = start([1.0, 0.0, 0.0], Quat::IDENTITY, parent);
        // World +2 X is local -2 Y under a 90 degree parent.
        let world = apply(
            &Delta::Translate(Vec3::new(2.0, 0.0, 0.0)),
            child.world.w_axis.truncate(),
            std::slice::from_ref(&child),
            false,
        );
        assert!(
            (Vec3::from_array(world[0].position) - Vec3::new(1.0, -2.0, 0.0)).length() < 1e-5,
            "{:?}",
            world[0].position
        );
        // Local X of the child points along world +Y.
        let basis = Basis::new(
            GizmoSpace::Local,
            child.world.w_axis.truncate(),
            Some(&child.world),
        );
        assert!((basis.axes[0] - Vec3::Y).length() < 1e-5);
        let local = apply(
            &Delta::Translate(basis.axes[0] * 2.0),
            basis.pivot,
            std::slice::from_ref(&child),
            false,
        );
        assert!(
            (Vec3::from_array(local[0].position) - Vec3::new(3.0, 0.0, 0.0)).length() < 1e-5,
            "{:?}",
            local[0].position
        );
        assert_eq!(
            Basis::new(GizmoSpace::World, Vec3::ZERO, Some(&child.world)).axes[0],
            Vec3::X
        );
    }

    #[test]
    fn rotate_and_scale_apply_to_local_components() {
        let parent = Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let child = start([0.0; 3], Quat::IDENTITY, parent);
        // A world-Z turn commutes with the parent's Z rotation.
        let turned = apply(
            &Delta::Rotate {
                axis: Vec3::Z,
                radians: std::f32::consts::FRAC_PI_2,
            },
            Vec3::ZERO,
            std::slice::from_ref(&child),
            true,
        );
        let expected = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        assert!(Quat::from_array(turned[0].rotation).angle_between(expected) < 1e-5);
        // A world-X scale on a child whose local Y lies along world X scales local Y.
        let scaled = apply(
            &Delta::Scale {
                axis: Some(Vec3::X),
                factor: 2.0,
            },
            Vec3::ZERO,
            std::slice::from_ref(&child),
            true,
        );
        assert_eq!(scaled[0].scale, [1.0, 2.0, 1.0]);
        let uniform = apply(
            &Delta::Scale {
                axis: None,
                factor: 3.0,
            },
            Vec3::ZERO,
            std::slice::from_ref(&child),
            false,
        );
        assert_eq!(uniform[0].scale, [3.0, 3.0, 1.0]);
    }

    #[test]
    fn multi_selection_rotates_and_scales_about_the_pivot() {
        let a = start([1.0, 0.0, 0.0], Quat::IDENTITY, Mat4::IDENTITY);
        let b = EntityStart {
            id: "b".into(),
            ..start([-1.0, 0.0, 0.0], Quat::IDENTITY, Mat4::IDENTITY)
        };
        let turned = apply(
            &Delta::Rotate {
                axis: Vec3::Z,
                radians: std::f32::consts::FRAC_PI_2,
            },
            Vec3::ZERO,
            &[a.clone(), b.clone()],
            true,
        );
        assert!((Vec3::from_array(turned[0].position) - Vec3::Y).length() < 1e-5);
        assert!((Vec3::from_array(turned[1].position) + Vec3::Y).length() < 1e-5);
        let scaled = apply(
            &Delta::Scale {
                axis: None,
                factor: 2.0,
            },
            Vec3::ZERO,
            &[a, b],
            true,
        );
        assert_eq!(scaled[0].position, [2.0, 0.0, 0.0]);
        assert_eq!(scaled[1].position, [-2.0, 0.0, 0.0]);
    }

    #[test]
    fn rotation_accumulates_past_half_a_turn() {
        let view = view2d();
        let basis = Basis::world(Vec3::ZERO);
        let start = view.project(Vec3::new(4.0, 0.0, 0.0)).unwrap();
        let mut gesture = begin(&view, Handle::Ring(2), GizmoMode::Rotate, basis, start);
        let mut last = 0.0;
        for degrees in [60.0f32, 120.0, 180.0, 240.0] {
            let to = view
                .project(Vec3::new(
                    4.0 * degrees.to_radians().cos(),
                    4.0 * degrees.to_radians().sin(),
                    0.0,
                ))
                .unwrap();
            let Delta::Rotate { radians, .. } = drag(&view, &mut gesture, to, &snapping(false))
            else {
                panic!()
            };
            last = radians.to_degrees();
        }
        assert!((last - 240.0).abs() < 0.1, "{last}");
    }
}
