//! Mirrors `engine.PhysicsBody2D/3D` into engine-physics and writes simulated dynamic
//! poses back to `Transform`s.
use super::super::{invalid, mapped, world_pose, write_world_pose};
use super::{Resolved, Worlds, types::*};
use crate::Result;
use bevy_ecs::prelude::*;
use engine_physics::{dim2, dim3};
use glam::{Quat, Vec3};
use std::collections::BTreeMap;

/// World `(scale, rotation, position)` at the last sync.
pub(crate) type Pose = (Vec3, Quat, Vec3);
/// Last synced authored configuration and world pose of one body.
pub(crate) struct Record<T> {
    pub(crate) config: T,
    pub(crate) pose: Pose,
}
/// Live state of a dynamic 2D body, kept across a rebuild so editing its configuration does
/// not teleport or stop it. `authored` is the authored velocity pair the body last had.
#[derive(Clone, Copy)]
struct Live2 {
    position: [f32; 2],
    rotation: f32,
    velocity: [f32; 2],
    angular: f32,
    authored: ([f32; 2], f32),
}
/// Live state of a dynamic 3D body, see [`Live2`].
#[derive(Clone, Copy)]
struct Live3 {
    position: [f32; 3],
    rotation: [f32; 4],
    velocity: [f32; 3],
    angular: [f32; 3],
    authored: ([f32; 3], [f32; 3]),
}
/// The motion of a kinematic body over the fixed steps of one frame: from the pose it had when
/// the frame started to the pose its `Transform` asks for.
struct Drive2 {
    start: ([f32; 2], f32),
    target: ([f32; 2], f32),
}
struct Drive3 {
    start: (Vec3, Quat),
    target: (Vec3, Quat),
}
/// Synced bodies by entity. The engine-physics body ID is `Entity::to_bits()`.
#[derive(Default)]
pub(crate) struct Bodies {
    pub(crate) two: BTreeMap<Entity, Record<PhysicsBody2D>>,
    pub(crate) three: BTreeMap<Entity, Record<PhysicsBody3D>>,
    drives2: BTreeMap<Entity, Drive2>,
    drives3: BTreeMap<Entity, Drive3>,
    /// States captured by `clear`, consumed by the sync that rebuilds the bodies.
    carry2: BTreeMap<Entity, Live2>,
    carry3: BTreeMap<Entity, Live3>,
}
pub(super) fn planar(rotation: Quat) -> Result<f32> {
    if rotation.x.abs() > 0.0001 || rotation.y.abs() > 0.0001 {
        return Err(invalid("2D bodies require rotation about world Z only"));
    }
    Ok(2. * rotation.z.atan2(rotation.w))
}
fn uniform(scale: Vec3, n: usize) -> Result<()> {
    if (scale.x - scale.y).abs() > 0.0001 || n == 3 && (scale.x - scale.z).abs() > 0.0001 {
        return Err(invalid(
            "Ball, capsule, cylinder and compound colliders require uniform world scale",
        ));
    }
    Ok(())
}
fn part_shape_error() -> crate::Error {
    invalid("Compound parts must be box, ball, capsule, cylinder (3D) or convex_hull")
}
fn shape2(c: &PhysicsBody2D, scale: Vec3) -> Result<dim2::Shape> {
    let scaled = |p: &[f32; 2]| [p[0] * scale.x, p[1] * scale.y];
    Ok(match c.shape {
        Shape::Box => dim2::Shape::Box {
            half_extents: scaled(&c.half_extents),
        },
        Shape::Ball => {
            uniform(scale, 2)?;
            dim2::Shape::Ball {
                radius: c.radius * scale.x,
            }
        }
        Shape::Capsule => {
            uniform(scale, 2)?;
            dim2::Shape::Capsule {
                radius: c.radius * scale.x,
                half_height: c.half_height * scale.y,
            }
        }
        Shape::Cylinder => return Err(invalid("Cylinder colliders are 3D only")),
        Shape::ConvexHull => dim2::Shape::ConvexHull {
            points: c.points.iter().map(scaled).collect(),
        },
        Shape::TriMesh => dim2::Shape::TriMesh {
            vertices: c.vertices.iter().map(scaled).collect(),
            indices: c.indices.clone(),
        },
        Shape::HeightField => dim2::Shape::HeightField {
            heights: c.heights.clone(),
            scale: scaled(&c.heightfield_scale),
        },
        Shape::Compound => {
            uniform(scale, 2)?;
            let s = scale.x;
            let parts = c.parts.iter().map(|p| {
                let shape = match p.shape {
                    Shape::Box => dim2::Shape::Box {
                        half_extents: p.half_extents.map(|v| v * s),
                    },
                    Shape::Ball => dim2::Shape::Ball {
                        radius: p.radius * s,
                    },
                    Shape::Capsule => dim2::Shape::Capsule {
                        radius: p.radius * s,
                        half_height: p.half_height * s,
                    },
                    Shape::ConvexHull => dim2::Shape::ConvexHull {
                        points: p.points.iter().map(|q| q.map(|v| v * s)).collect(),
                    },
                    _ => return Err(part_shape_error()),
                };
                Ok(dim2::CompoundPart {
                    position: p.position.map(|v| v * s),
                    rotation: p.rotation,
                    shape,
                })
            });
            dim2::Shape::Compound {
                parts: parts.collect::<Result<_>>()?,
            }
        }
    })
}
/// Expands the authored hole cell indices into one flag per cell.
fn heightfield_holes(c: &PhysicsBody3D) -> Result<Vec<bool>> {
    if c.heightfield_holes.is_empty() {
        return Ok(Vec::new());
    }
    // Validate the sample grid before sizing anything from it: `rows` and `columns` are
    // authored, only `heights` is bounded.
    let (rows, columns) = (c.heightfield_rows as usize, c.heightfield_columns as usize);
    if rows < 2
        || columns < 2
        || rows.checked_mul(columns) != Some(c.heights.len())
        || c.heights.len() > engine_physics::MAX_HEIGHTFIELD_SAMPLES
    {
        return Err(invalid(
            "heightfield needs at least 2x2 samples matching rows x columns (at most 1048576 samples)",
        ));
    }
    let cells = (rows - 1) * (columns - 1);
    let mut holes = vec![false; cells];
    for &cell in &c.heightfield_holes {
        *holes.get_mut(cell as usize).ok_or_else(|| {
            invalid("heightfield_holes entries must index a cell of the heightfield")
        })? = true;
    }
    Ok(holes)
}
fn shape3(c: &PhysicsBody3D, scale: Vec3) -> Result<dim3::Shape> {
    let scaled = |p: &[f32; 3]| (Vec3::from(*p) * scale).to_array();
    Ok(match c.shape {
        Shape::Box => dim3::Shape::Box {
            half_extents: scaled(&c.half_extents),
        },
        Shape::Ball => {
            uniform(scale, 3)?;
            dim3::Shape::Ball {
                radius: c.radius * scale.x,
            }
        }
        Shape::Capsule => {
            uniform(scale, 3)?;
            dim3::Shape::Capsule {
                radius: c.radius * scale.x,
                half_height: c.half_height * scale.y,
            }
        }
        Shape::Cylinder => {
            uniform(scale, 3)?;
            dim3::Shape::Cylinder {
                radius: c.radius * scale.x,
                half_height: c.half_height * scale.y,
            }
        }
        Shape::ConvexHull => dim3::Shape::ConvexHull {
            points: c.points.iter().map(scaled).collect(),
        },
        Shape::TriMesh => dim3::Shape::TriMesh {
            vertices: c.vertices.iter().map(scaled).collect(),
            indices: c.indices.clone(),
        },
        Shape::HeightField => dim3::Shape::HeightField {
            rows: c.heightfield_rows as usize,
            columns: c.heightfield_columns as usize,
            heights: c.heights.clone(),
            scale: scaled(&c.heightfield_scale),
            holes: heightfield_holes(c)?,
        },
        Shape::Compound => {
            uniform(scale, 3)?;
            let s = scale.x;
            let parts = c.parts.iter().map(|p| {
                let shape = match p.shape {
                    Shape::Box => dim3::Shape::Box {
                        half_extents: p.half_extents.map(|v| v * s),
                    },
                    Shape::Ball => dim3::Shape::Ball {
                        radius: p.radius * s,
                    },
                    Shape::Capsule => dim3::Shape::Capsule {
                        radius: p.radius * s,
                        half_height: p.half_height * s,
                    },
                    Shape::Cylinder => dim3::Shape::Cylinder {
                        radius: p.radius * s,
                        half_height: p.half_height * s,
                    },
                    Shape::ConvexHull => dim3::Shape::ConvexHull {
                        points: p.points.iter().map(|q| q.map(|v| v * s)).collect(),
                    },
                    _ => return Err(part_shape_error()),
                };
                Ok(dim3::CompoundPart {
                    position: p.position.map(|v| v * s),
                    rotation: p.rotation,
                    shape,
                })
            });
            dim3::Shape::Compound {
                parts: parts.collect::<Result<_>>()?,
            }
        }
    })
}
/// Rebuild when authored data or scale changes, or when a fixed body moved.
fn rebuild<T: PartialEq>(old: Option<&Record<T>>, config: &T, fixed: bool, pose: Pose) -> bool {
    old.is_none_or(|old| {
        old.config != *config
            || !old.pose.0.abs_diff_eq(pose.0, 0.0001)
            || fixed && old.pose != pose
    })
}
/// Angle `to` reached from `from` along the shortest way round.
fn lerp_angle(from: f32, to: f32, t: f32) -> f32 {
    let tau = std::f32::consts::TAU;
    let delta = (to - from + std::f32::consts::PI).rem_euclid(tau) - std::f32::consts::PI;
    from + delta * t
}
fn live2(worlds: &Worlds, id: u64, config: &PhysicsBody2D) -> Option<Live2> {
    let pose = worlds.two.pose(id)?;
    let velocity = worlds.two.velocity(id)?;
    Some(Live2 {
        position: pose.position,
        rotation: pose.rotation,
        velocity: velocity.linear,
        angular: velocity.angular,
        authored: (config.velocity, config.angular_velocity),
    })
}
fn live3(worlds: &Worlds, id: u64, config: &PhysicsBody3D) -> Option<Live3> {
    let pose = worlds.three.pose(id)?;
    let velocity = worlds.three.velocity(id)?;
    Some(Live3 {
        position: pose.position,
        rotation: pose.rotation,
        velocity: velocity.linear,
        angular: velocity.angular,
        authored: (config.velocity, config.angular_velocity),
    })
}
/// Notes that `entity` has no body because the world is full (once per episode).
fn starve(worlds: &mut Worlds, entity: Entity) {
    if worlds.starved.insert(entity) {
        worlds.starved_new += 1;
    }
}
impl Bodies {
    pub(crate) fn is_empty(&self) -> bool {
        self.two.is_empty() && self.three.is_empty()
    }
    /// Removes every body so the next sync rebuilds them from authored data. Dynamic bodies
    /// come back at the pose and velocity they have now.
    pub(crate) fn clear(&mut self, worlds: &mut Worlds) {
        for (e, record) in std::mem::take(&mut self.two) {
            if record.config.motion == Motion::Dynamic
                && let Some(live) = live2(worlds, e.to_bits(), &record.config)
            {
                self.carry2.insert(e, live);
            }
            worlds.two.remove(e.to_bits());
        }
        for (e, record) in std::mem::take(&mut self.three) {
            if record.config.motion == Motion::Dynamic
                && let Some(live) = live3(worlds, e.to_bits(), &record.config)
            {
                self.carry3.insert(e, live);
            }
            worlds.three.remove(e.to_bits());
        }
        self.drives2.clear();
        self.drives3.clear();
    }
    pub(crate) fn sync(
        &mut self,
        worlds: &mut Worlds,
        settings: &Resolved,
        world: &World,
    ) -> Result<()> {
        let removed: Vec<_> = self
            .two
            .keys()
            .filter(|e| world.get::<PhysicsBody2D>(**e).is_none())
            .copied()
            .collect();
        for e in removed {
            worlds.two.remove(e.to_bits());
            self.two.remove(&e);
            self.drives2.remove(&e);
        }
        let removed: Vec<_> = self
            .three
            .keys()
            .filter(|e| world.get::<PhysicsBody3D>(**e).is_none())
            .copied()
            .collect();
        for e in removed {
            worlds.three.remove(e.to_bits());
            self.three.remove(&e);
            self.drives3.remove(&e);
        }
        // Starved entities that lost their component, or whose ragdoll went away, are no
        // longer waiting for room.
        worlds.starved.retain(|e| {
            world.get::<PhysicsBody2D>(*e).is_some()
                || world.get::<PhysicsBody3D>(*e).is_some()
                || world.get::<super::Ragdoll>(*e).is_some()
                || world.get::<super::PhysicsJoint2D>(*e).is_some()
                || world.get::<super::PhysicsJoint3D>(*e).is_some()
        });
        for entity in world.iter_entities() {
            let e = entity.id();
            if let Some(config) = entity.get::<PhysicsBody2D>() {
                self.sync2(worlds, settings, world, e, config)?;
            }
            if let Some(config) = entity.get::<PhysicsBody3D>() {
                self.sync3(worlds, settings, world, e, config)?;
            }
        }
        self.carry2.clear();
        self.carry3.clear();
        Ok(())
    }
    fn sync2(
        &mut self,
        worlds: &mut Worlds,
        settings: &Resolved,
        world: &World,
        e: Entity,
        config: &PhysicsBody2D,
    ) -> Result<()> {
        let id = e.to_bits();
        let pose = world_pose(world, e)?;
        let (scale, rotation, position) = pose;
        let angle = planar(rotation)?;
        let old = self.two.get(&e);
        if rebuild(old, config, config.motion == Motion::Fixed, pose) {
            // A dynamic body that already exists keeps its simulated pose and velocity: the
            // `Transform` may hold an interpolated pose, and the authored velocity is only
            // the starting velocity (unless that field itself was edited).
            let live = self
                .carry2
                .remove(&e)
                .or_else(|| {
                    old.filter(|o| o.config.motion == Motion::Dynamic)
                        .and_then(|o| live2(worlds, id, &o.config))
                })
                .filter(|_| config.motion == Motion::Dynamic);
            worlds.two.remove(id);
            self.two.remove(&e);
            self.drives2.remove(&e);
            if worlds.two.len() >= worlds.two.capacity() {
                starve(worlds, e);
                return Ok(());
            }
            let carried = live.is_some();
            let (at, rotation_2d, velocity, angular) = match live {
                Some(l) => (
                    l.position,
                    l.rotation,
                    if l.authored.0 == config.velocity {
                        l.velocity
                    } else {
                        config.velocity
                    },
                    if l.authored.1 == config.angular_velocity {
                        l.angular
                    } else {
                        config.angular_velocity
                    },
                ),
                None => (
                    [position.x, position.y],
                    angle,
                    config.velocity,
                    config.angular_velocity,
                ),
            };
            let desc = dim2::BodyDesc {
                kind: config.motion.into(),
                position: at,
                rotation: rotation_2d,
                velocity,
                angular_velocity: angular,
                ccd: config.ccd,
            };
            let shape = shape2(config, scale)?;
            let material = settings.material(&config.material)?;
            worlds
                .two
                .insert(id, desc, shape, material)
                .map_err(mapped)?;
            worlds.starved.remove(&e);
            let pose = if carried {
                (
                    scale,
                    Quat::from_rotation_z(rotation_2d),
                    Vec3::new(at[0], at[1], position.z),
                )
            } else {
                pose
            };
            let config = config.clone();
            self.two.insert(e, Record { config, pose });
        } else if config.motion == Motion::Kinematic
            && let Some(now) = worlds.two.pose(id)
        {
            self.drives2.insert(
                e,
                Drive2 {
                    start: (now.position, now.rotation),
                    target: ([position.x, position.y], angle),
                },
            );
        }
        Ok(())
    }
    fn sync3(
        &mut self,
        worlds: &mut Worlds,
        settings: &Resolved,
        world: &World,
        e: Entity,
        config: &PhysicsBody3D,
    ) -> Result<()> {
        let id = e.to_bits();
        let pose = world_pose(world, e)?;
        let (scale, rotation, position) = pose;
        let old = self.three.get(&e);
        if rebuild(old, config, config.motion == Motion::Fixed, pose) {
            // See `sync2`: an existing dynamic body keeps its simulated state.
            let live = self
                .carry3
                .remove(&e)
                .or_else(|| {
                    old.filter(|o| o.config.motion == Motion::Dynamic)
                        .and_then(|o| live3(worlds, id, &o.config))
                })
                .filter(|_| config.motion == Motion::Dynamic);
            worlds.three.remove(id);
            self.three.remove(&e);
            self.drives3.remove(&e);
            if worlds.three.len() >= worlds.three.capacity() {
                starve(worlds, e);
                return Ok(());
            }
            let carried = live.is_some();
            let (at, turn, velocity, angular) = match live {
                Some(l) => (
                    l.position,
                    l.rotation,
                    if l.authored.0 == config.velocity {
                        l.velocity
                    } else {
                        config.velocity
                    },
                    if l.authored.1 == config.angular_velocity {
                        l.angular
                    } else {
                        config.angular_velocity
                    },
                ),
                None => (
                    position.to_array(),
                    rotation.to_array(),
                    config.velocity,
                    config.angular_velocity,
                ),
            };
            let desc = dim3::BodyDesc {
                kind: config.motion.into(),
                position: at,
                rotation: turn,
                velocity,
                angular_velocity: angular,
                ccd: config.ccd,
            };
            let shape = shape3(config, scale)?;
            let material = settings.material(&config.material)?;
            worlds
                .three
                .insert(id, desc, shape, material)
                .map_err(mapped)?;
            worlds.starved.remove(&e);
            let pose = if carried {
                (scale, Quat::from_array(turn), Vec3::from(at))
            } else {
                pose
            };
            let config = config.clone();
            self.three.insert(e, Record { config, pose });
        } else if config.motion == Motion::Kinematic
            && let Some(now) = worlds.three.pose(id)
        {
            self.drives3.insert(
                e,
                Drive3 {
                    start: (Vec3::from(now.position), Quat::from_array(now.rotation)),
                    target: (position, rotation),
                },
            );
        }
        Ok(())
    }
    /// Moves every kinematic body toward its frame target: after step `index` of `steps` it
    /// has covered `index / steps` of the way, so a body driven once per frame moves at the
    /// same speed in every step of the frame instead of jumping in the first one.
    pub(crate) fn drive_kinematics(&self, worlds: &mut Worlds, index: usize, steps: usize) {
        let t = index as f32 / steps.max(1) as f32;
        let last = index >= steps;
        for (e, drive) in &self.drives2 {
            let (position, angle) = if last {
                drive.target
            } else {
                (
                    [
                        drive.start.0[0] + (drive.target.0[0] - drive.start.0[0]) * t,
                        drive.start.0[1] + (drive.target.0[1] - drive.start.0[1]) * t,
                    ],
                    lerp_angle(drive.start.1, drive.target.1, t),
                )
            };
            let _ = worlds
                .two
                .set_kinematic_target(e.to_bits(), position, angle);
        }
        for (e, drive) in &self.drives3 {
            let (position, rotation) = if last {
                drive.target
            } else {
                (
                    drive.start.0.lerp(drive.target.0, t),
                    drive.start.1.slerp(drive.target.1, t),
                )
            };
            let _ = worlds.three.set_kinematic_target(
                e.to_bits(),
                position.to_array(),
                rotation.to_array(),
            );
        }
    }
    /// Writes dynamic poses back, parents before children.
    pub(crate) fn write_back(&self, worlds: &Worlds, world: &mut World) -> Result<()> {
        let mut writes = vec![];
        for (e, r) in &self.two {
            if r.config.motion == Motion::Dynamic {
                let p = worlds
                    .two
                    .pose(e.to_bits())
                    .ok_or_else(|| invalid("Missing body"))?;
                writes.push((
                    *e,
                    r.pose.0,
                    Quat::from_rotation_z(p.rotation),
                    Vec3::new(p.position[0], p.position[1], r.pose.2.z),
                ));
            }
        }
        for (e, r) in &self.three {
            if r.config.motion == Motion::Dynamic {
                let p = worlds
                    .three
                    .pose(e.to_bits())
                    .ok_or_else(|| invalid("Missing body"))?;
                writes.push((
                    *e,
                    r.pose.0,
                    Quat::from_array(p.rotation),
                    Vec3::from(p.position),
                ));
            }
        }
        // Parents must be applied before children convert their simulated world pose.
        writes.sort_by_key(|(e, _, _, _)| depth(world, *e));
        for (e, s, r, p) in writes {
            write_world_pose(world, e, s, r, p)?;
        }
        Ok(())
    }
}
pub(super) fn depth(world: &World, mut entity: Entity) -> usize {
    let mut count = 0;
    while let Some(parent) = world.get::<crate::Parent>(entity) {
        count += 1;
        if count > 256 {
            break;
        }
        let Some(p) = crate::services::find_entity(world, &parent.0) else {
            break;
        };
        entity = p;
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(rows: u32, columns: u32, heights: usize, holes: Vec<u32>) -> PhysicsBody3D {
        PhysicsBody3D {
            shape: Shape::HeightField,
            heightfield_rows: rows,
            heightfield_columns: columns,
            heights: vec![0.0; heights],
            heightfield_holes: holes,
            ..PhysicsBody3D::default()
        }
    }

    #[test]
    fn heightfield_holes_validate_the_grid_before_allocating() {
        // (rows - 1) * (columns - 1) would be ~1.1e12 flags: it must be an error, not an
        // allocation.
        let huge = field(1_048_576, 1_048_576, 4, vec![0]);
        assert!(heightfield_holes(&huge).is_err());
        assert!(shape3(&huge, Vec3::ONE).is_err());
        let ok = field(3, 3, 9, vec![0, 3]);
        let holes = heightfield_holes(&ok).unwrap();
        assert_eq!(holes.len(), 4);
        assert!(holes[0] && !holes[1] && !holes[2] && holes[3]);
        assert!(heightfield_holes(&field(3, 3, 9, vec![4])).is_err());
    }
}
