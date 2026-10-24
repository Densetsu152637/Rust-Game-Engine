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
/// Synced bodies by entity. The engine-physics body ID is `Entity::to_bits()`.
#[derive(Default)]
pub(crate) struct Bodies {
    pub(crate) two: BTreeMap<Entity, Record<PhysicsBody2D>>,
    pub(crate) three: BTreeMap<Entity, Record<PhysicsBody3D>>,
}
fn planar(rotation: Quat) -> Result<f32> {
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
impl Bodies {
    pub(crate) fn is_empty(&self) -> bool {
        self.two.is_empty() && self.three.is_empty()
    }
    /// Removes every body so the next sync rebuilds them from authored data.
    pub(crate) fn clear(&mut self, worlds: &mut Worlds) {
        for e in std::mem::take(&mut self.two).into_keys() {
            worlds.two.remove(e.to_bits());
        }
        for e in std::mem::take(&mut self.three).into_keys() {
            worlds.three.remove(e.to_bits());
        }
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
        }
        for entity in world.iter_entities() {
            let e = entity.id();
            if let Some(config) = entity.get::<PhysicsBody2D>() {
                self.sync2(worlds, settings, world, e, config)?;
            }
            if let Some(config) = entity.get::<PhysicsBody3D>() {
                self.sync3(worlds, settings, world, e, config)?;
            }
        }
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
        if rebuild(
            self.two.get(&e),
            config,
            config.motion == Motion::Fixed,
            pose,
        ) {
            worlds.two.remove(id);
            let desc = dim2::BodyDesc {
                kind: config.motion.into(),
                position: [position.x, position.y],
                rotation: angle,
                velocity: config.velocity,
                angular_velocity: config.angular_velocity,
                ccd: config.ccd,
            };
            let shape = shape2(config, scale)?;
            let material = settings.material(&config.material)?;
            worlds
                .two
                .insert(id, desc, shape, material)
                .map_err(mapped)?;
            let config = config.clone();
            self.two.insert(e, Record { config, pose });
        } else if config.motion == Motion::Kinematic {
            worlds
                .two
                .set_kinematic_target(id, [position.x, position.y], angle)
                .map_err(mapped)?;
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
        if rebuild(
            self.three.get(&e),
            config,
            config.motion == Motion::Fixed,
            pose,
        ) {
            worlds.three.remove(id);
            let desc = dim3::BodyDesc {
                kind: config.motion.into(),
                position: position.to_array(),
                rotation: rotation.to_array(),
                velocity: config.velocity,
                angular_velocity: config.angular_velocity,
                ccd: config.ccd,
            };
            let shape = shape3(config, scale)?;
            let material = settings.material(&config.material)?;
            worlds
                .three
                .insert(id, desc, shape, material)
                .map_err(mapped)?;
            let config = config.clone();
            self.three.insert(e, Record { config, pose });
        } else if config.motion == Motion::Kinematic {
            worlds
                .three
                .set_kinematic_target(id, position.to_array(), rotation.to_array())
                .map_err(mapped)?;
        }
        Ok(())
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
fn depth(world: &World, mut entity: Entity) -> usize {
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
