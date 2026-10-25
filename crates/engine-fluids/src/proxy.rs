//! Plain-data rigid-body proxies supplied by the host on every step.
//!
//! [`ProxyShape`] and [`ColliderProxy`] are kept identical to the types of the sibling
//! `engine-xpbd` crate so one host adapter can feed both solvers.

use crate::{Error, MAX_PROXIES, Result, params::MAX_COORDINATE};
use glam::{Quat, Vec3};

/// Collision shape of a proxy, in the proxy's local frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ProxyShape {
    Sphere {
        radius: f32,
    },
    /// Capsule along the local Y axis: segment `-half_height..=half_height` plus `radius`.
    Capsule {
        half_height: f32,
        radius: f32,
    },
    Box {
        half_extents: Vec3,
    },
    /// World-space half-space `dot(normal, x) <= offset` is solid; the pose is ignored.
    Plane {
        normal: Vec3,
        offset: f32,
    },
}

/// One rigid body as seen by the fluid for one step: pose at the start of the step and the
/// velocities used to advance it through the substeps.
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

/// Impulse the fluid applied to one proxy during a step (N s), and its angular impulse about
/// the proxy position (N m s). Apply `linear / dt` as a force to obtain buoyancy and drag.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProxyImpulse {
    pub id: u64,
    pub linear: Vec3,
    pub angular: Vec3,
}

/// A validated proxy with a normalized rotation and cached bound.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PreparedProxy {
    pub id: u64,
    position: Vec3,
    rotation: Quat,
    linear_velocity: Vec3,
    angular_velocity: Vec3,
    shape: ProxyShape,
    pub friction: f32,
    bound: f32,
}

/// Proxy pose at one instant inside the step.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ProxyPose {
    pub position: Vec3,
    rotation: Quat,
    inverse: Quat,
}

/// Contact of a particle centre with a proxy surface.
pub(crate) struct Contact {
    /// Signed distance of the point from the surface (negative inside).
    pub distance: f32,
    /// Outward surface normal.
    pub normal: Vec3,
}

fn positive(value: f32, what: &'static str) -> Result<()> {
    if value.is_finite() && value > 0.0 && value <= MAX_COORDINATE {
        Ok(())
    } else {
        Err(Error::Invalid(what))
    }
}

/// Validate, normalize and sort proxies by id. Duplicate ids are rejected.
pub(crate) fn prepare(proxies: &[ColliderProxy]) -> Result<Vec<PreparedProxy>> {
    if proxies.len() > MAX_PROXIES {
        return Err(Error::Invalid("too many collider proxies (MAX_PROXIES)"));
    }
    let mut prepared = proxies
        .iter()
        .map(PreparedProxy::new)
        .collect::<Result<Vec<_>>>()?;
    prepared.sort_by_key(|p| p.id);
    if prepared.windows(2).any(|w| w[0].id == w[1].id) {
        return Err(Error::Invalid("duplicate collider proxy id"));
    }
    Ok(prepared)
}

impl PreparedProxy {
    fn new(proxy: &ColliderProxy) -> Result<Self> {
        if !proxy.position.is_finite()
            || proxy.position.abs().max_element() > MAX_COORDINATE
            || !proxy.linear_velocity.is_finite()
            || !proxy.angular_velocity.is_finite()
        {
            return Err(Error::Invalid(
                "proxy position and velocities must be finite and bounded",
            ));
        }
        if proxy.linear_velocity.length() > 1000.0 || proxy.angular_velocity.length() > 1000.0 {
            return Err(Error::Invalid(
                "proxy velocities must be at most 1000 m/s and 1000 rad/s",
            ));
        }
        let length = proxy.rotation.length();
        if !proxy.rotation.is_finite() || !(0.5..=2.0).contains(&length) {
            return Err(Error::Invalid(
                "proxy rotation must be a finite unit quaternion",
            ));
        }
        if !(proxy.friction.is_finite() && (0.0..=10.0).contains(&proxy.friction)) {
            return Err(Error::Invalid(
                "proxy friction must be finite and within 0..=10",
            ));
        }
        let (shape, bound) = match proxy.shape {
            ProxyShape::Sphere { radius } => {
                positive(radius, "proxy sphere radius must be finite and positive")?;
                (proxy.shape, radius)
            }
            ProxyShape::Capsule {
                half_height,
                radius,
            } => {
                positive(radius, "proxy capsule radius must be finite and positive")?;
                if !(half_height.is_finite() && (0.0..=MAX_COORDINATE).contains(&half_height)) {
                    return Err(Error::Invalid(
                        "proxy capsule half height must be finite and non-negative",
                    ));
                }
                (proxy.shape, half_height + radius)
            }
            ProxyShape::Box { half_extents } => {
                for e in half_extents.to_array() {
                    positive(e, "proxy box half extents must be finite and positive")?;
                }
                (proxy.shape, half_extents.length())
            }
            ProxyShape::Plane { normal, offset } => {
                let n = normal.length();
                if !normal.is_finite() || n < 1.0e-6 || !offset.is_finite() {
                    return Err(Error::Invalid(
                        "proxy plane needs a finite non-zero normal and finite offset",
                    ));
                }
                (
                    ProxyShape::Plane {
                        normal: normal / n,
                        offset: offset / n,
                    },
                    f32::INFINITY,
                )
            }
        };
        Ok(Self {
            id: proxy.id,
            position: proxy.position,
            rotation: proxy.rotation.normalize(),
            linear_velocity: proxy.linear_velocity,
            angular_velocity: proxy.angular_velocity,
            shape,
            friction: proxy.friction,
            bound,
        })
    }

    /// Pose `t` seconds after the start of the step (constant velocities).
    pub fn pose_at(&self, t: f32) -> ProxyPose {
        let rotation =
            (Quat::from_scaled_axis(self.angular_velocity * t) * self.rotation).normalize();
        ProxyPose {
            position: self.position + self.linear_velocity * t,
            rotation,
            inverse: rotation.inverse(),
        }
    }

    pub fn is_plane(&self) -> bool {
        matches!(self.shape, ProxyShape::Plane { .. })
    }

    /// Velocity of the proxy surface at world point `x`.
    pub fn surface_velocity(&self, pose: &ProxyPose, x: Vec3) -> Vec3 {
        self.linear_velocity + self.angular_velocity.cross(x - pose.position)
    }

    /// Contact of point `x` with the proxy grown by `margin`, or `None` when the point is
    /// farther than `margin` from the surface.
    #[inline]
    pub fn contact(&self, pose: &ProxyPose, x: Vec3, margin: f32) -> Option<Contact> {
        if let ProxyShape::Plane { normal, offset } = self.shape {
            let distance = normal.dot(x) - offset;
            return (distance < margin).then_some(Contact { distance, normal });
        }
        let reach = self.bound + margin;
        if x.distance_squared(pose.position) >= reach * reach {
            return None;
        }
        let local = pose.inverse * (x - pose.position);
        let (distance, local_normal) = match self.shape {
            ProxyShape::Sphere { radius } => sphere_contact(local, radius),
            ProxyShape::Capsule {
                half_height,
                radius,
            } => {
                let axis = Vec3::new(0.0, local.y.clamp(-half_height, half_height), 0.0);
                sphere_contact(local - axis, radius)
            }
            ProxyShape::Box { half_extents } => box_contact(local, half_extents),
            ProxyShape::Plane { .. } => unreachable!("planes return above"),
        };
        (distance < margin).then(|| Contact {
            distance,
            normal: pose.rotation * local_normal,
        })
    }
}

fn sphere_contact(local: Vec3, radius: f32) -> (f32, Vec3) {
    let length = local.length();
    if length <= radius * 1.0e-6 {
        return (-radius, Vec3::Y);
    }
    (length - radius, local / length)
}

fn box_contact(local: Vec3, half: Vec3) -> (f32, Vec3) {
    let d = local.abs() - half;
    let sign = Vec3::select(local.cmplt(Vec3::ZERO), Vec3::NEG_ONE, Vec3::ONE);
    if d.max_element() > 0.0 {
        let outside = d.max(Vec3::ZERO);
        let length = outside.length();
        return (length, outside * sign / length);
    }
    // Inside: leave through the nearest face.
    let axis = if d.x >= d.y && d.x >= d.z {
        Vec3::X
    } else if d.y >= d.z {
        Vec3::Y
    } else {
        Vec3::Z
    };
    (d.max_element(), axis * sign)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy(shape: ProxyShape) -> ColliderProxy {
        ColliderProxy {
            id: 1,
            position: Vec3::new(1.0, 2.0, 3.0),
            rotation: Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
            linear_velocity: Vec3::ZERO,
            angular_velocity: Vec3::ZERO,
            shape,
            friction: 0.0,
        }
    }

    #[test]
    fn signed_distances_and_normals() {
        let capsule = prepare(&[proxy(ProxyShape::Capsule {
            half_height: 1.0,
            radius: 0.5,
        })])
        .unwrap()[0];
        let pose = capsule.pose_at(0.0);
        // Rotated 90 degrees about Z: the capsule axis lies along world X.
        let c = capsule
            .contact(&pose, Vec3::new(2.0, 2.25, 3.0), 0.0)
            .unwrap();
        assert!((c.distance + 0.25).abs() < 1e-5);
        assert!(c.normal.distance(Vec3::Y) < 1e-5);
        let boxed = prepare(&[proxy(ProxyShape::Box {
            half_extents: Vec3::new(1.0, 0.5, 0.5),
        })])
        .unwrap()[0];
        let pose = boxed.pose_at(0.0);
        let c = boxed.contact(&pose, Vec3::new(1.0, 2.0, 3.6), 0.2).unwrap();
        assert!((c.distance - 0.1).abs() < 1e-5);
        assert!(c.normal.distance(Vec3::Z) < 1e-5);
        assert!(
            boxed
                .contact(&pose, Vec3::new(1.0, 2.0, 3.8), 0.2)
                .is_none()
        );
        let plane = prepare(&[proxy(ProxyShape::Plane {
            normal: Vec3::new(0.0, 2.0, 0.0),
            offset: 1.0,
        })])
        .unwrap()[0];
        let c = plane
            .contact(&plane.pose_at(0.0), Vec3::new(5.0, 0.25, 0.0), 0.0)
            .unwrap();
        assert!((c.distance + 0.25).abs() < 1e-6);
    }
}
