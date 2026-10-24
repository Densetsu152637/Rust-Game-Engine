use super::*;
use engine_physics::{World2D, World3D, dim2, dim3};
type Pose = (Vec3, Quat, Vec3);
struct Record<T> {
    config: T,
    pose: Pose,
}
pub(super) struct Physics {
    two: World2D,
    three: World3D,
    bodies2: BTreeMap<Entity, Record<PhysicsBody2D>>,
    bodies3: BTreeMap<Entity, Record<PhysicsBody3D>>,
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
            "Ball and capsule colliders require uniform world scale",
        ));
    }
    Ok(())
}
impl Physics {
    pub fn new(config: SimulationConfig) -> Result<Self> {
        Ok(Self {
            two: World2D::new(config.gravity_2d, config.body_capacity).map_err(mapped)?,
            three: World3D::new(config.gravity_3d, config.body_capacity).map_err(mapped)?,
            bodies2: BTreeMap::new(),
            bodies3: BTreeMap::new(),
        })
    }
    pub fn sync(&mut self, world: &World) -> Result<()> {
        let removed: Vec<_> = self
            .bodies2
            .keys()
            .filter(|e| world.get::<PhysicsBody2D>(**e).is_none())
            .copied()
            .collect();
        for e in removed {
            self.two.remove(e.to_bits());
            self.bodies2.remove(&e);
        }
        let removed: Vec<_> = self
            .bodies3
            .keys()
            .filter(|e| world.get::<PhysicsBody3D>(**e).is_none())
            .copied()
            .collect();
        for e in removed {
            self.three.remove(e.to_bits());
            self.bodies3.remove(&e);
        }
        for entity in world.iter_entities() {
            let e = entity.id();
            let id = e.to_bits();
            if let Some(config) = entity.get::<PhysicsBody2D>() {
                let pose = world_pose(world, e)?;
                let (scale, rotation, position) = pose;
                let angle = planar(rotation)?;
                let rebuild = self.bodies2.get(&e).is_none_or(|old| {
                    old.config != *config
                        || !old.pose.0.abs_diff_eq(scale, 0.0001)
                        || config.motion == Motion::Fixed && old.pose != pose
                });
                if rebuild {
                    self.two.remove(id);
                    let shape = match config.shape {
                        Shape::Box => dim2::Shape::Box {
                            half_extents: [
                                config.half_extents[0] * scale.x,
                                config.half_extents[1] * scale.y,
                            ],
                        },
                        Shape::Ball => {
                            uniform(scale, 2)?;
                            dim2::Shape::Ball {
                                radius: config.radius * scale.x,
                            }
                        }
                        Shape::Capsule => {
                            uniform(scale, 2)?;
                            dim2::Shape::Capsule {
                                radius: config.radius * scale.x,
                                half_height: config.half_height * scale.y,
                            }
                        }
                    };
                    self.two
                        .insert(
                            id,
                            dim2::BodyDesc {
                                kind: config.motion.into(),
                                position: [position.x, position.y],
                                rotation: angle,
                                velocity: config.velocity,
                                angular_velocity: config.angular_velocity,
                                ccd: config.ccd,
                            },
                            shape,
                            (&config.material).into(),
                        )
                        .map_err(mapped)?;
                    self.bodies2.insert(
                        e,
                        Record {
                            config: config.clone(),
                            pose,
                        },
                    );
                } else if config.motion == Motion::Kinematic {
                    self.two
                        .set_kinematic_target(id, [position.x, position.y], angle)
                        .map_err(mapped)?;
                }
            }
            if let Some(config) = entity.get::<PhysicsBody3D>() {
                let pose = world_pose(world, e)?;
                let (scale, rotation, position) = pose;
                let rebuild = self.bodies3.get(&e).is_none_or(|old| {
                    old.config != *config
                        || !old.pose.0.abs_diff_eq(scale, 0.0001)
                        || config.motion == Motion::Fixed && old.pose != pose
                });
                if rebuild {
                    self.three.remove(id);
                    let shape = match config.shape {
                        Shape::Box => dim3::Shape::Box {
                            half_extents: (Vec3::from(config.half_extents) * scale).to_array(),
                        },
                        Shape::Ball => {
                            uniform(scale, 3)?;
                            dim3::Shape::Ball {
                                radius: config.radius * scale.x,
                            }
                        }
                        Shape::Capsule => {
                            uniform(scale, 3)?;
                            dim3::Shape::Capsule {
                                radius: config.radius * scale.x,
                                half_height: config.half_height * scale.y,
                            }
                        }
                    };
                    self.three
                        .insert(
                            id,
                            dim3::BodyDesc {
                                kind: config.motion.into(),
                                position: position.to_array(),
                                rotation: rotation.to_array(),
                                velocity: config.velocity,
                                angular_velocity: config.angular_velocity,
                                ccd: config.ccd,
                            },
                            shape,
                            (&config.material).into(),
                        )
                        .map_err(mapped)?;
                    self.bodies3.insert(
                        e,
                        Record {
                            config: config.clone(),
                            pose,
                        },
                    );
                } else if config.motion == Motion::Kinematic {
                    self.three
                        .set_kinematic_target(id, position.to_array(), rotation.to_array())
                        .map_err(mapped)?;
                }
            }
        }
        Ok(())
    }
    pub fn step(&mut self, world: &mut World, dt: f32) -> Result<()> {
        if self.bodies2.is_empty() && self.bodies3.is_empty() {
            return Ok(());
        }
        // Bounded subdivision keeps Rapier within its documented dt range.
        if !dt.is_finite() || dt <= 0. || dt > 1. {
            return Err(invalid("Physics frame delta must be in (0,1] seconds"));
        }
        let substeps = (dt / 0.016667).ceil() as usize;
        for _ in 0..substeps {
            self.two.step(dt / substeps as f32).map_err(mapped)?;
            self.three.step(dt / substeps as f32).map_err(mapped)?;
            for (dimension, events) in [
                (2, self.two.drain_events().collect::<Vec<_>>()),
                (3, self.three.drain_events().collect::<Vec<_>>()),
            ] {
                for event in events {
                    let first = Entity::from_bits(event.first);
                    let second = Entity::from_bits(event.second);
                    for (target, other) in [(first, second), (second, first)] {
                        if world.get_entity(target).is_ok() {
                            let name = if event.started {
                                "physics.contact.start"
                            } else {
                                "physics.contact.stop"
                            };
                            let other_id = world
                                .get::<crate::AuthoredId>(other)
                                .map(|id| id.id.clone());
                            emit(
                                world,
                                target,
                                world.get_entity(other).is_ok().then_some(other),
                                name,
                                serde_json::json!({"dimension":dimension,"other":other_id}),
                            )?;
                        }
                    }
                }
            }
        }
        let mut writes = vec![];
        for (e, r) in &self.bodies2 {
            if r.config.motion == Motion::Dynamic {
                let p = self
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
        for (e, r) in &self.bodies3 {
            if r.config.motion == Motion::Dynamic {
                let p = self
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
