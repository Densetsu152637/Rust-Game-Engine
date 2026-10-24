macro_rules! world {
    () => {
        use crate::{BodyKind, CollisionEvent, Error, Material, RayHit, Result};
        use std::collections::{BTreeMap, BTreeSet};
        /// Owns Rapier state and stable application IDs. One collider per body.
        pub struct World {
            inner: PhysicsWorld,
            bodies: BTreeMap<u64, (RigidBodyHandle, ColliderHandle)>,
            events: Vec<CollisionEvent>,
            contacts: BTreeSet<(u64, u64)>,
            capacity: usize,
        }
        impl World {
            pub fn new(gravity: Position, capacity: usize) -> Result<Self> {
                if !crate::finite(&gravity) || capacity == 0 || capacity > 100_000 {
                    return Err(Error("invalid world limits or gravity"));
                }
                let inner = PhysicsWorld {
                    gravity: vector(gravity),
                    ..PhysicsWorld::default()
                };
                Ok(Self {
                    inner,
                    bodies: BTreeMap::new(),
                    events: Vec::new(),
                    contacts: BTreeSet::new(),
                    capacity,
                })
            }
            pub fn insert(
                &mut self,
                id: u64,
                desc: BodyDesc,
                shape: Shape,
                material: Material,
            ) -> Result<()> {
                desc.validate()?;
                material.validate()?;
                let collider = shape.collider()?;
                if self.bodies.contains_key(&id) {
                    return Err(Error("duplicate body ID"));
                }
                if self.bodies.len() >= self.capacity {
                    return Err(Error("body capacity exceeded"));
                }
                let builder = match desc.kind {
                    BodyKind::Fixed => RigidBodyBuilder::fixed(),
                    BodyKind::Dynamic => RigidBodyBuilder::dynamic(),
                    BodyKind::Kinematic => RigidBodyBuilder::kinematic_position_based(),
                };
                let mut body = builder
                    .translation(vector(desc.position))
                    .linvel(vector(desc.velocity))
                    .ccd_enabled(desc.ccd)
                    .build();
                body.set_rotation(rotation(desc.rotation), true);
                body.set_angvel(angular(desc.angular_velocity), true);
                let collider = collider
                    .density(material.density)
                    .friction(material.friction)
                    .restitution(material.restitution)
                    .sensor(material.sensor)
                    .collision_groups(InteractionGroups::new(
                        Group::from_bits_truncate(material.layer),
                        Group::from_bits_truncate(material.mask),
                        InteractionTestMode::And,
                    ))
                    .active_events(ActiveEvents::COLLISION_EVENTS);
                let handles = self.inner.insert(body, collider);
                self.bodies.insert(id, handles);
                Ok(())
            }
            pub fn remove(&mut self, id: u64) -> bool {
                if let Some((body, _)) = self.bodies.remove(&id) {
                    self.inner.remove_body(body);
                    true
                } else {
                    false
                }
            }
            pub fn len(&self) -> usize {
                self.bodies.len()
            }
            pub fn is_empty(&self) -> bool {
                self.bodies.is_empty()
            }
            pub fn pose(&self, id: u64) -> Option<BodyPose> {
                let body = self.inner.bodies.get(self.bodies.get(&id)?.0)?;
                Some(body_pose(body))
            }
            pub fn apply_impulse(&mut self, id: u64, impulse: Position) -> Result<()> {
                if !crate::finite(&impulse) {
                    return Err(Error("invalid impulse"));
                }
                let handle = self.bodies.get(&id).ok_or(Error("unknown body"))?.0;
                let body = &mut self.inner.bodies[handle];
                if !body.is_dynamic() {
                    return Err(Error("impulse requires dynamic body"));
                }
                body.apply_impulse(vector(impulse), true);
                Ok(())
            }
            pub fn set_kinematic_target(
                &mut self,
                id: u64,
                position: Position,
                orientation: Orientation,
            ) -> Result<()> {
                if !crate::finite(&position) || !valid_rotation(orientation) {
                    return Err(Error("invalid target pose"));
                }
                let handle = self.bodies.get(&id).ok_or(Error("unknown body"))?.0;
                let body = &mut self.inner.bodies[handle];
                if !body.is_kinematic() {
                    return Err(Error("target requires kinematic body"));
                }
                body.set_next_kinematic_translation(vector(position));
                body.set_next_kinematic_rotation(rotation(orientation));
                Ok(())
            }
            /// Events are replaced each step, bounding storage even if the host never drains them.
            pub fn step(&mut self, dt: f32) -> Result<()> {
                if !dt.is_finite() || !(0.0..=0.25).contains(&dt) || dt == 0.0 {
                    return Err(Error("step requires 0 < dt <= 0.25"));
                }
                self.inner.integration_parameters.dt = dt;
                self.inner.step();
                self.events.clear();
                let ids: std::collections::HashMap<_, _> =
                    self.bodies.iter().map(|(id, (_, c))| (*c, *id)).collect();
                let mut current = BTreeSet::new();
                let solids = self
                    .inner
                    .narrow_phase
                    .contact_pairs()
                    .filter(|p| p.has_any_active_contact())
                    .map(|p| (p.collider1, p.collider2));
                let sensors = self
                    .inner
                    .narrow_phase
                    .intersection_pairs()
                    .filter(|(_, _, active)| *active)
                    .map(|(a, b, _)| (a, b));
                for (a, b) in solids.chain(sensors) {
                    if let (Some(&a), Some(&b)) = (ids.get(&a), ids.get(&b)) {
                        current.insert((a.min(b), a.max(b)));
                    }
                }
                for &(first, second) in current.difference(&self.contacts) {
                    self.events.push(CollisionEvent {
                        first,
                        second,
                        started: true,
                    });
                }
                for &(first, second) in self.contacts.difference(&current) {
                    self.events.push(CollisionEvent {
                        first,
                        second,
                        started: false,
                    });
                }
                self.contacts = current;
                self.events.sort_by_key(|e| (e.first, e.second, e.started));
                Ok(())
            }
            pub fn drain_events(&mut self) -> impl Iterator<Item = CollisionEvent> + '_ {
                self.events.drain(..)
            }
            /// Normalizes direction so returned distance is in world units.
            /// Exact linear scan remains fresh immediately after insertion/removal.
            pub fn raycast(
                &self,
                origin: Position,
                direction: Position,
                max_distance: f32,
                mask: u32,
            ) -> Result<Option<RayHit>> {
                if !crate::finite(&origin)
                    || !crate::finite(&direction)
                    || !max_distance.is_finite()
                    || max_distance < 0.0
                {
                    return Err(Error("invalid ray"));
                }
                let direction = vector(direction);
                if direction.length_squared() < 1e-12 {
                    return Err(Error("zero ray direction"));
                }
                let ray = Ray::new(vector(origin), direction.normalize());
                let mut closest: Option<RayHit> = None;
                for (&id, &(body, collider)) in &self.bodies {
                    let collider = &self.inner.colliders[collider];
                    if collider.collision_groups().memberships.bits() & mask == 0 {
                        continue;
                    }
                    if let Some(distance) = collider.shape().cast_ray(
                        self.inner.bodies[body].position(),
                        &ray,
                        max_distance,
                        true,
                    ) && closest.is_none_or(|hit| distance < hit.distance)
                    {
                        closest = Some(RayHit { id, distance });
                    }
                }
                Ok(closest)
            }
        }
    };
}
pub(crate) use world;
