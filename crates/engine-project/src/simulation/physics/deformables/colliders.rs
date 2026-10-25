//! Rigid bodies as collider proxies for the deformable solvers, and the reaction impulses
//! applied back to dynamic bodies.
//!
//! Exact shapes: box, ball, capsule. Approximations (documented in `docs/physics-cloth.md`):
//! cylinder, convex hull and compound bodies become the local bounding box of their points or
//! parts; trimesh and heightfield bodies are not coupled.
use super::super::{Bodies, Context, Motion, PhysicsBody3D, Shape, Worlds};
use crate::Result;
use crate::simulation::mapped;
use glam::{Quat, Vec3};

/// Solver-neutral proxy shape.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Form {
    Sphere(f32),
    Capsule(f32, f32),
    Box(Vec3),
}

/// One rigid body as seen by a deformable for one substep.
#[derive(Clone, Copy, Debug)]
pub(super) struct Collider {
    pub id: u64,
    pub body_position: Vec3,
    pub position: Vec3,
    pub rotation: Quat,
    pub linear: Vec3,
    pub angular: Vec3,
    pub form: Form,
    pub friction: f32,
    pub layer: u32,
    pub dynamic: bool,
    /// Radius of a sphere around `position` that contains the shape.
    pub bound: f32,
}

fn part_radius(
    shape: Shape,
    half_extents: Vec3,
    radius: f32,
    half_height: f32,
    points: &[[f32; 3]],
) -> f32 {
    match shape {
        Shape::Box => half_extents.length(),
        Shape::Ball => radius,
        Shape::Capsule => half_height + radius,
        Shape::Cylinder => radius.hypot(half_height),
        Shape::ConvexHull => points
            .iter()
            .map(|p| Vec3::from(*p).length())
            .fold(0., f32::max),
        Shape::TriMesh | Shape::HeightField | Shape::Compound => 0.,
    }
}

/// The proxy form and its centre offset in body-local space, or `None` for shapes that are
/// not coupled.
fn form(config: &PhysicsBody3D, scale: Vec3) -> Option<(Form, Vec3)> {
    let he = Vec3::from(config.half_extents) * scale;
    Some(match config.shape {
        Shape::Box => (Form::Box(he), Vec3::ZERO),
        Shape::Ball => (Form::Sphere(config.radius * scale.x), Vec3::ZERO),
        Shape::Capsule => (
            Form::Capsule(config.half_height * scale.y, config.radius * scale.x),
            Vec3::ZERO,
        ),
        Shape::Cylinder => (
            Form::Box(Vec3::new(
                config.radius * scale.x,
                config.half_height * scale.y,
                config.radius * scale.x,
            )),
            Vec3::ZERO,
        ),
        Shape::ConvexHull => {
            let (min, max) = config.points.iter().fold(
                (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
                |(lo, hi), p| {
                    let p = Vec3::from(*p) * scale;
                    (lo.min(p), hi.max(p))
                },
            );
            if config.points.is_empty() {
                return None;
            }
            (Form::Box((max - min) / 2.), (max + min) / 2.)
        }
        Shape::Compound => {
            let mut lo = Vec3::splat(f32::MAX);
            let mut hi = Vec3::splat(f32::MIN);
            for part in &config.parts {
                let reach = part_radius(
                    part.shape,
                    Vec3::from(part.half_extents),
                    part.radius,
                    part.half_height,
                    &part.points,
                ) * scale.max_element();
                let center = Vec3::from(part.position) * scale;
                lo = lo.min(center - reach);
                hi = hi.max(center + reach);
            }
            if config.parts.is_empty() {
                return None;
            }
            (Form::Box((hi - lo) / 2.), (hi + lo) / 2.)
        }
        Shape::TriMesh | Shape::HeightField => return None,
    })
}

/// Every coupled 3D body at its current pose, sorted by id (ascending, unique).
pub(super) fn collect(cx: &Context<'_>) -> Vec<Collider> {
    gather(cx.worlds, cx.bodies, cx.settings)
}
fn gather(worlds: &Worlds, bodies: &Bodies, settings: &super::super::Resolved) -> Vec<Collider> {
    let mut out = Vec::with_capacity(bodies.three.len());
    for (entity, record) in &bodies.three {
        let Ok(material) = settings.material(&record.config.material) else {
            continue;
        };
        if material.sensor {
            continue;
        }
        let id = entity.to_bits();
        let Some(pose) = worlds.three.pose(id) else {
            continue;
        };
        let Some((form, offset)) = form(&record.config, record.pose.0) else {
            continue;
        };
        let velocity = worlds.three.velocity(id);
        let rotation = Quat::from_array(pose.rotation);
        let body_position = Vec3::from(pose.position);
        let bound = offset.length()
            + match form {
                Form::Sphere(r) => r,
                Form::Capsule(h, r) => h + r,
                Form::Box(h) => h.length(),
            };
        out.push(Collider {
            id,
            body_position,
            position: body_position + rotation * offset,
            rotation,
            linear: velocity.map_or(Vec3::from(pose.velocity), |v| Vec3::from(v.linear)),
            angular: velocity.map_or(Vec3::ZERO, |v| Vec3::from(v.angular)),
            form,
            friction: material.friction,
            layer: material.layer,
            dynamic: record.config.motion == Motion::Dynamic,
            bound,
        });
    }
    out.sort_by_key(|c| c.id);
    out
}

/// Colliders allowed by `mask` whose bound comes within `margin` of the box `min..max`.
pub(super) fn near(
    colliders: &[Collider],
    mask: u32,
    min: Vec3,
    max: Vec3,
    margin: f32,
) -> Vec<Collider> {
    colliders
        .iter()
        .filter(|c| c.layer & mask != 0)
        .filter(|c| {
            let reach = c.bound + margin + c.linear.length() * 0.1;
            c.position.clamp(min, max).distance_squared(c.position) <= reach * reach
        })
        .copied()
        .collect()
}

/// Keeps the `limit` colliders nearest to the box `min..max` (ties by id), still sorted by id.
/// Returns how many were left out.
pub(super) fn limit(near: &mut Vec<Collider>, min: Vec3, max: Vec3, limit: usize) -> usize {
    if near.len() <= limit {
        return 0;
    }
    let omitted = near.len() - limit;
    let gap = |c: &Collider| c.position.clamp(min, max).distance_squared(c.position);
    near.sort_by(|a, b| gap(a).total_cmp(&gap(b)).then(a.id.cmp(&b.id)));
    near.truncate(limit);
    near.sort_by_key(|c| c.id);
    omitted
}

impl Collider {
    pub(super) fn xpbd(&self) -> engine_xpbd::ColliderProxy {
        engine_xpbd::ColliderProxy {
            id: self.id,
            position: self.position,
            rotation: self.rotation,
            linear_velocity: self.linear,
            angular_velocity: self.angular,
            shape: match self.form {
                Form::Sphere(radius) => engine_xpbd::ProxyShape::Sphere { radius },
                Form::Capsule(half_height, radius) => engine_xpbd::ProxyShape::Capsule {
                    half_height,
                    radius,
                },
                Form::Box(half_extents) => engine_xpbd::ProxyShape::Box { half_extents },
            },
            friction: self.friction,
        }
    }
    pub(super) fn fluid(&self) -> engine_fluids::ColliderProxy {
        engine_fluids::ColliderProxy {
            id: self.id,
            position: self.position,
            rotation: self.rotation,
            linear_velocity: self.linear,
            angular_velocity: self.angular,
            shape: match self.form {
                Form::Sphere(radius) => engine_fluids::ProxyShape::Sphere { radius },
                Form::Capsule(half_height, radius) => engine_fluids::ProxyShape::Capsule {
                    half_height,
                    radius,
                },
                Form::Box(half_extents) => engine_fluids::ProxyShape::Box { half_extents },
            },
            friction: self.friction,
        }
    }
}

/// Applies a reaction on proxy `id`: `linear` impulse (N s) and `angular` impulse about the
/// proxy position, both multiplied by `scale`. Non-dynamic bodies are unaffected.
pub(super) fn apply(
    worlds: &mut Worlds,
    colliders: &[Collider],
    id: u64,
    linear: Vec3,
    angular: Vec3,
    scale: f32,
) -> Result<()> {
    let Ok(index) = colliders.binary_search_by_key(&id, |c| c.id) else {
        return Ok(());
    };
    let collider = &colliders[index];
    if !collider.dynamic || scale <= 0. {
        return Ok(());
    }
    let about_body = angular + (collider.position - collider.body_position).cross(linear);
    let (linear, about_body) = (linear * scale, about_body * scale);
    if !linear.is_finite() || !about_body.is_finite() {
        return Ok(());
    }
    if linear != Vec3::ZERO {
        worlds
            .three
            .apply_impulse(id, linear.to_array())
            .map_err(mapped)?;
    }
    if about_body != Vec3::ZERO {
        worlds
            .three
            .apply_angular_impulse(id, about_body.to_array())
            .map_err(mapped)?;
    }
    Ok(())
}
