//! Plain-data rigid-body collider proxies and the per-proxy reaction impulse report.
use glam::{Quat, Vec3};

use crate::{
    Error, MAX_COORDINATE, MAX_EXTENT, MAX_FRICTION, MAX_PROXIES, MAX_SPEED, Result,
    finite_bounded, in_range,
};

/// Collision shape of a [`ColliderProxy`].
///
/// This type is intentionally identical to `engine_fluids::ProxyShape` so one host adapter
/// can fill both crates from the same rigid bodies.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ProxyShape {
    /// Ball of `radius` meters around the proxy position.
    Sphere { radius: f32 },
    /// Capsule whose segment runs along the local Y axis from `-half_height` to
    /// `+half_height`, inflated by `radius`.
    Capsule { half_height: f32, radius: f32 },
    /// Oriented box with `half_extents` along the local axes.
    Box { half_extents: Vec3 },
    /// World-space half-space: points with `normal . x < offset` are solid. The proxy pose
    /// and velocities are ignored.
    Plane { normal: Vec3, offset: f32 },
}

/// A rigid collider supplied by the host for one step.
///
/// The pose is the collider pose for the whole step. Velocities describe the surface
/// motion used for friction (`linear_velocity + angular_velocity x (p - position)`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColliderProxy {
    pub id: u64,
    pub position: Vec3,
    pub rotation: Quat,
    pub linear_velocity: Vec3,
    pub angular_velocity: Vec3,
    pub shape: ProxyShape,
    pub friction: f32,
}

impl ColliderProxy {
    /// Checks finite, bounded values, a unit rotation (within 1e-3) and positive extents.
    pub fn validate(&self) -> Result<()> {
        let pose_ok = finite_bounded(self.position, MAX_COORDINATE)
            && self.rotation.is_finite()
            && (self.rotation.length() - 1.0).abs() <= 1.0e-3
            && finite_bounded(self.linear_velocity, MAX_SPEED)
            && finite_bounded(self.angular_velocity, MAX_SPEED)
            && in_range(self.friction, 0.0, MAX_FRICTION);
        if !pose_ok {
            return Err(Error("invalid collider proxy pose, velocity or friction"));
        }
        let shape_ok = match self.shape {
            ProxyShape::Sphere { radius } => in_range(radius, f32::MIN_POSITIVE, MAX_EXTENT),
            ProxyShape::Capsule {
                half_height,
                radius,
            } => {
                in_range(half_height, 0.0, MAX_EXTENT)
                    && in_range(radius, f32::MIN_POSITIVE, MAX_EXTENT)
            }
            ProxyShape::Box { half_extents } => {
                finite_bounded(half_extents, MAX_EXTENT) && half_extents.min_element() > 0.0
            }
            ProxyShape::Plane { normal, offset } => {
                finite_bounded(normal, MAX_EXTENT)
                    && normal.length() > 1.0e-6
                    && in_range(offset, -MAX_COORDINATE, MAX_COORDINATE)
            }
        };
        if shape_ok {
            Ok(())
        } else {
            Err(Error("invalid collider proxy shape"))
        }
    }
}

/// Contact impulse accumulated against one proxy over a whole step.
///
/// `impulse` (N s) is the reaction to apply to the proxy's rigid body: the negated sum of
/// the momentum the contacts added to the particles. `point` is the contact point
/// weighted by impulse magnitude, and `angular_impulse` (N m s) is the exact torque
/// impulse of all contacts about [`ColliderProxy::position`], so a host can apply either
/// `impulse` at `point` or `impulse` at the body origin plus `angular_impulse`.
/// `contacts` counts particle contacts summed over substeps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProxyImpulse {
    pub id: u64,
    pub impulse: Vec3,
    pub point: Vec3,
    pub angular_impulse: Vec3,
    pub contacts: u32,
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Sphere(f32),
    Capsule(f32, f32),
    Box(Vec3),
    Plane(Vec3, f32),
}

/// A validated proxy with a normalized rotation, ready for distance queries.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Prepared {
    pub id: u64,
    pub position: Vec3,
    rotation: Quat,
    inverse_rotation: Quat,
    linear_velocity: Vec3,
    angular_velocity: Vec3,
    kind: Kind,
    pub friction: f32,
    /// Bounding-sphere radius around `position`; `None` for planes.
    bound: Option<f32>,
}

impl Prepared {
    fn new(proxy: &ColliderProxy) -> Self {
        let rotation = proxy.rotation.normalize();
        let (kind, bound) = match proxy.shape {
            ProxyShape::Sphere { radius } => (Kind::Sphere(radius), Some(radius)),
            ProxyShape::Capsule {
                half_height,
                radius,
            } => (
                Kind::Capsule(half_height, radius),
                Some(half_height + radius),
            ),
            ProxyShape::Box { half_extents } => {
                (Kind::Box(half_extents), Some(half_extents.length()))
            }
            ProxyShape::Plane { normal, offset } => {
                let length = normal.length();
                (Kind::Plane(normal / length, offset / length), None)
            }
        };
        Self {
            id: proxy.id,
            position: proxy.position,
            rotation,
            inverse_rotation: rotation.inverse(),
            linear_velocity: proxy.linear_velocity,
            angular_velocity: proxy.angular_velocity,
            kind,
            friction: proxy.friction,
            bound,
        }
    }

    /// Signed distance from `p` to the proxy surface (negative inside) and the outward
    /// surface normal at the closest feature.
    pub fn query(&self, p: Vec3) -> (f32, Vec3) {
        match self.kind {
            Kind::Plane(normal, offset) => (normal.dot(p) - offset, normal),
            Kind::Sphere(radius) => {
                let (length, direction) = length_and_direction(p - self.position, self.rotation);
                (length - radius, direction)
            }
            Kind::Capsule(half_height, radius) => {
                let local = self.inverse_rotation * (p - self.position);
                let axis = Vec3::new(0.0, local.y.clamp(-half_height, half_height), 0.0);
                let (length, direction) = length_and_direction(local - axis, Quat::IDENTITY);
                (length - radius, self.rotation * direction)
            }
            Kind::Box(half_extents) => {
                let local = self.inverse_rotation * (p - self.position);
                let excess = local.abs() - half_extents;
                if excess.max_element() > 0.0 {
                    let outside = local - local.clamp(-half_extents, half_extents);
                    let length = outside.length();
                    (length, self.rotation * (outside / length))
                } else {
                    let axis = if excess.x >= excess.y && excess.x >= excess.z {
                        0
                    } else if excess.y >= excess.z {
                        1
                    } else {
                        2
                    };
                    let mut normal = Vec3::ZERO;
                    normal[axis] = if local[axis] >= 0.0 { 1.0 } else { -1.0 };
                    (excess[axis], self.rotation * normal)
                }
            }
        }
    }

    /// Velocity of the proxy surface material at world point `p`.
    pub fn surface_velocity(&self, p: Vec3) -> Vec3 {
        match self.kind {
            Kind::Plane(..) => Vec3::ZERO,
            _ => self.linear_velocity + self.angular_velocity.cross(p - self.position),
        }
    }

    /// Conservative overlap test between the proxy and an axis-aligned box grown by `margin`.
    pub fn may_touch(&self, min: Vec3, max: Vec3, margin: f32) -> bool {
        match (self.kind, self.bound) {
            (Kind::Plane(normal, offset), _) => {
                // Most negative signed distance of any AABB corner.
                let corner = Vec3::select(normal.cmpge(Vec3::ZERO), min, max);
                normal.dot(corner) - offset < margin
            }
            (_, Some(radius)) => {
                let closest = self.position.clamp(min, max);
                closest.distance_squared(self.position) <= (radius + margin).powi(2)
            }
            (_, None) => true,
        }
    }
}

fn length_and_direction(v: Vec3, fallback_frame: Quat) -> (f32, Vec3) {
    let length = v.length();
    if length > 1.0e-12 {
        (length, v / length)
    } else {
        (0.0, fallback_frame * Vec3::Y)
    }
}

/// Validates `proxies`, rejects duplicate ids and writes them sorted by id into `out`.
pub(crate) fn prepare(proxies: &[ColliderProxy], out: &mut Vec<Prepared>) -> Result<()> {
    if proxies.len() > MAX_PROXIES {
        return Err(Error("too many collider proxies"));
    }
    out.clear();
    for proxy in proxies {
        proxy.validate()?;
        out.push(Prepared::new(proxy));
    }
    out.sort_by_key(|proxy| proxy.id);
    if out.windows(2).any(|pair| pair[0].id == pair[1].id) {
        out.clear();
        return Err(Error("duplicate collider proxy id"));
    }
    Ok(())
}

/// Running per-proxy sums for one step.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ImpulseAccum {
    impulse: Vec3,
    angular: Vec3,
    weighted_point: Vec3,
    weight: f32,
    point_sum: Vec3,
    contacts: u32,
}

impl ImpulseAccum {
    /// Adds an impulse applied to the proxy at `point`; `origin` is the torque reference.
    pub fn add(&mut self, impulse: Vec3, point: Vec3, origin: Vec3) {
        let magnitude = impulse.length();
        self.impulse += impulse;
        self.angular += (point - origin).cross(impulse);
        self.weighted_point += point * magnitude;
        self.weight += magnitude;
        self.point_sum += point;
        self.contacts = self.contacts.saturating_add(1);
    }

    pub fn report(&self, id: u64) -> Option<ProxyImpulse> {
        if self.contacts == 0 {
            return None;
        }
        let point = if self.weight > 0.0 {
            self.weighted_point / self.weight
        } else {
            self.point_sum / self.contacts as f32
        };
        Some(ProxyImpulse {
            id,
            impulse: self.impulse,
            point,
            angular_impulse: self.angular,
            contacts: self.contacts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy(shape: ProxyShape, rotation: Quat) -> Prepared {
        Prepared::new(&ColliderProxy {
            id: 1,
            position: Vec3::new(1.0, 2.0, 3.0),
            rotation,
            linear_velocity: Vec3::ZERO,
            angular_velocity: Vec3::ZERO,
            shape,
            friction: 0.0,
        })
    }

    #[test]
    fn box_distance_outside_inside_and_rotated() {
        let boxed = proxy(
            ProxyShape::Box {
                half_extents: Vec3::new(1.0, 0.5, 0.25),
            },
            Quat::IDENTITY,
        );
        let (d, n) = boxed.query(Vec3::new(1.0, 3.0, 3.0));
        assert!((d - 0.5).abs() < 1e-6 && n.abs_diff_eq(Vec3::Y, 1e-6));
        let (d, n) = boxed.query(Vec3::new(1.0, 2.0, 3.2));
        assert!((d + 0.05).abs() < 1e-6 && n.abs_diff_eq(Vec3::Z, 1e-6));
        let rotated = proxy(
            ProxyShape::Box {
                half_extents: Vec3::new(1.0, 0.5, 0.25),
            },
            Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
        );
        // Local +X now points along world +Y.
        let (d, n) = rotated.query(Vec3::new(1.0, 4.0, 3.0));
        assert!((d - 1.0).abs() < 1e-5 && n.abs_diff_eq(Vec3::Y, 1e-5));
    }

    #[test]
    fn capsule_and_plane_distances() {
        let capsule = proxy(
            ProxyShape::Capsule {
                half_height: 1.0,
                radius: 0.5,
            },
            Quat::IDENTITY,
        );
        let (d, n) = capsule.query(Vec3::new(3.0, 2.5, 3.0));
        assert!((d - 1.5).abs() < 1e-6 && n.abs_diff_eq(Vec3::X, 1e-6));
        let (d, _) = capsule.query(Vec3::new(1.0, 4.0, 3.0));
        assert!((d - 0.5).abs() < 1e-6);
        let plane = proxy(
            ProxyShape::Plane {
                normal: Vec3::new(0.0, 2.0, 0.0),
                offset: 2.0,
            },
            Quat::IDENTITY,
        );
        let (d, n) = plane.query(Vec3::new(5.0, 0.25, -7.0));
        assert!((d + 0.75).abs() < 1e-6 && n == Vec3::Y);
        assert!(plane.may_touch(Vec3::splat(-1.0), Vec3::splat(1.1), 0.0));
        assert!(!plane.may_touch(Vec3::splat(1.1), Vec3::splat(2.0), 0.0));
    }
}
