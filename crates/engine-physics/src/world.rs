//! Dimension-generic world core, compiled once per dimension.
//!
//! `dim2.rs` and `dim3.rs` each include this file with `#[path = "world.rs"] mod world;`.
//! Dimension-specific names (`Position`, `BodyDesc`, `Shape`, `vector`, the Rapier prelude)
//! resolve through `use super::*` against the including dimension module.
//!
//! # Extension seam
//!
//! A feature lives in `src/features/<feature>.rs` (dimension-generic) or in
//! `src/features/<feature>/{dim2,dim3,shared}.rs` (dimension-specific descriptors plus a
//! shared part), and is registered with ONE `#[path = ...] mod <feature>;` item per
//! dimension file. Inside it:
//!
//! - add `impl World { ... }` blocks for the feature's public methods;
//! - keep per-world state in a side table type implementing [`Extension`] + `Default`,
//!   reached with [`World::extension`] / [`World::extension_mut`] (created on first use);
//! - use [`Core`] for Rapier sets, pipelines, integration parameters and the stable
//!   application-ID <-> handle maps;
//! - implement [`Extension::pre_step`] / [`Extension::post_step`] for per-step work and
//!   [`Extension::body_removed`] to drop side-table rows when a body disappears.
//!
//! Hooks run in ascending `(Extension::order, type name)` order, which is stable for a build.
use super::*;
use crate::{BodyKind, CollisionEvent, CombineRule, Error, Material, RayHit, Result};
use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};

/// Rapier handles of one application body. Each body owns exactly one collider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BodyEntry {
    pub(crate) body: RigidBodyHandle,
    pub(crate) collider: ColliderHandle,
}

/// Rapier state plus the stable application-ID maps; extensions receive `&mut Core`.
///
/// Body and collider `user_data` hold the owning application ID, so handle -> ID lookups
/// are O(1). Insert and remove application bodies only through [`World::insert`] and
/// [`World::remove`] so the maps and extension hooks stay consistent.
pub(crate) struct Core {
    /// Rapier sets, pipelines, gravity and integration parameters.
    pub(crate) rapier: PhysicsWorld,
    bodies: BTreeMap<u64, BodyEntry>,
    capacity: usize,
}
impl Core {
    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }
    pub(crate) fn entry(&self, id: u64) -> Result<BodyEntry> {
        self.bodies.get(&id).copied().ok_or(Error("unknown body"))
    }
    pub(crate) fn body_id(&self, handle: RigidBodyHandle) -> Option<u64> {
        let id = self.rapier.bodies.get(handle)?.user_data as u64;
        (self.bodies.get(&id)?.body == handle).then_some(id)
    }
    /// Application ID of the body owning `handle`, if it is an application body collider.
    pub(crate) fn collider_body_id(&self, handle: ColliderHandle) -> Option<u64> {
        self.body_id(self.rapier.colliders.get(handle)?.parent()?)
    }
    pub(crate) fn body(&self, id: u64) -> Result<&RigidBody> {
        let handle = self.entry(id)?.body;
        self.rapier.bodies.get(handle).ok_or(Error("unknown body"))
    }
    pub(crate) fn body_mut(&mut self, id: u64) -> Result<&mut RigidBody> {
        let handle = self.entry(id)?.body;
        self.rapier
            .bodies
            .get_mut(handle)
            .ok_or(Error("unknown body"))
    }
}

/// Default extension ordering band. Lower values run first.
pub(crate) const DEFAULT_ORDER: u16 = 100;

/// Per-world feature state ("side table") with step hooks. See the module docs.
pub(crate) trait Extension: Any + Send + Sync {
    /// Hook ordering band; ties break by type name.
    fn order(&self) -> u16 {
        DEFAULT_ORDER
    }
    /// Runs before Rapier integrates, e.g. to apply controller or vehicle forces.
    fn pre_step(&mut self, _core: &mut Core, _dt: f32) -> Result<()> {
        Ok(())
    }
    /// Runs after Rapier integrated and core contact events were collected.
    fn post_step(&mut self, _core: &mut Core, _dt: f32) -> Result<()> {
        Ok(())
    }
    /// Runs after `id` and its Rapier colliders and joints were removed.
    fn body_removed(&mut self, _core: &mut Core, _id: u64) {}
}
struct Slot {
    order: u16,
    name: &'static str,
    value: Box<dyn Extension>,
}

/// Owns Rapier state and stable application IDs. One collider per body.
pub struct World {
    pub(crate) core: Core,
    extensions: Vec<Slot>,
    events: Vec<CollisionEvent>,
    contacts: BTreeSet<(u64, u64)>,
}
/// Contact filter for the exclusion groups of [`World::set_contact_exclusion`]. The group is
/// stored in the upper 64 bits of the collider `user_data`; the lower 64 bits are the body ID.
struct ExclusionHooks;
impl PhysicsHooks for ExclusionHooks {
    fn filter_contact_pair(&self, context: &PairFilterContext) -> Option<SolverFlags> {
        let group = |handle| {
            context
                .colliders
                .get(handle)
                .map_or(0, |collider| (collider.user_data >> 64) as u64)
        };
        let (a, b) = (group(context.collider1), group(context.collider2));
        if a != 0 && a == b {
            None
        } else {
            Some(SolverFlags::default())
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyVelocity {
    pub linear: Position,
    pub angular: AngularVelocity,
}
fn combine(rule: CombineRule) -> CoefficientCombineRule {
    match rule {
        CombineRule::Average => CoefficientCombineRule::Average,
        CombineRule::Min => CoefficientCombineRule::Min,
        CombineRule::Max => CoefficientCombineRule::Max,
        CombineRule::Multiply => CoefficientCombineRule::Multiply,
    }
}
impl World {
    pub fn new(gravity: Position, capacity: usize) -> Result<Self> {
        if !crate::finite(&gravity) || capacity == 0 || capacity > 100_000 {
            return Err(Error("invalid world limits or gravity"));
        }
        let rapier = PhysicsWorld {
            gravity: vector(gravity),
            ..PhysicsWorld::default()
        };
        Ok(Self {
            core: Core {
                rapier,
                bodies: BTreeMap::new(),
                capacity,
            },
            extensions: Vec::new(),
            events: Vec::new(),
            contacts: BTreeSet::new(),
        })
    }
    /// Side table `T`, if a feature already created it.
    pub(crate) fn extension<T: Extension>(&self) -> Option<&T> {
        self.extensions
            .iter()
            .find_map(|slot| (slot.value.as_ref() as &dyn Any).downcast_ref())
    }
    /// Side table `T` (created on first use) together with the core it may mutate.
    pub(crate) fn extension_mut<T: Extension + Default>(&mut self) -> (&mut Core, &mut T) {
        let index = match self
            .extensions
            .iter()
            .position(|slot| (slot.value.as_ref() as &dyn Any).is::<T>())
        {
            Some(index) => index,
            None => {
                let value = T::default();
                let key = (value.order(), std::any::type_name::<T>());
                let index = self
                    .extensions
                    .partition_point(|slot| (slot.order, slot.name) < key);
                self.extensions.insert(
                    index,
                    Slot {
                        order: key.0,
                        name: key.1,
                        value: Box::new(value),
                    },
                );
                index
            }
        };
        let value = (self.extensions[index].value.as_mut() as &mut dyn Any)
            .downcast_mut()
            .expect("extension slot type was checked or inserted above");
        (&mut self.core, value)
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
        if desc.kind == BodyKind::Dynamic && shape.static_only() {
            return Err(Error(
                "triangle mesh and heightfield shapes require fixed or kinematic bodies",
            ));
        }
        let shape = shape.shared()?;
        if self.core.bodies.contains_key(&id) {
            return Err(Error("duplicate body ID"));
        }
        if self.core.bodies.len() >= self.core.capacity {
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
            .user_data(id as u128)
            .build();
        body.set_rotation(rotation(desc.rotation), true);
        body.set_angvel(angular(desc.angular_velocity), true);
        let collider = ColliderBuilder::new(shape)
            .density(material.density)
            .friction(material.friction)
            .friction_combine_rule(combine(material.friction_combine))
            .restitution(material.restitution)
            .restitution_combine_rule(combine(material.restitution_combine))
            .sensor(material.sensor)
            .active_collision_types(if material.sensor {
                ActiveCollisionTypes::all()
            } else {
                ActiveCollisionTypes::default()
            })
            .collision_groups(InteractionGroups::new(
                Group::from_bits_truncate(material.layer),
                Group::from_bits_truncate(material.mask),
                InteractionTestMode::And,
            ))
            .active_events(ActiveEvents::COLLISION_EVENTS)
            .user_data(id as u128);
        let (body, collider) = self.core.rapier.insert(body, collider);
        self.core.rapier.bodies[body]
            .recompute_mass_properties_from_colliders(&self.core.rapier.colliders);
        self.core.bodies.insert(id, BodyEntry { body, collider });
        Ok(())
    }
    pub fn remove(&mut self, id: u64) -> bool {
        let Some(entry) = self.core.bodies.remove(&id) else {
            return false;
        };
        self.core.rapier.remove_body(entry.body);
        for slot in &mut self.extensions {
            slot.value.body_removed(&mut self.core, id);
        }
        true
    }
    pub fn len(&self) -> usize {
        self.core.bodies.len()
    }
    /// Most bodies the world holds; joints are capped at four per body slot.
    pub fn capacity(&self) -> usize {
        self.core.capacity
    }
    pub fn is_empty(&self) -> bool {
        self.core.bodies.is_empty()
    }
    pub fn contains(&self, id: u64) -> bool {
        self.core.bodies.contains_key(&id)
    }
    pub fn pose(&self, id: u64) -> Option<BodyPose> {
        self.core.body(id).ok().map(body_pose)
    }
    pub fn velocity(&self, id: u64) -> Option<BodyVelocity> {
        let body = self.core.body(id).ok()?;
        Some(BodyVelocity {
            linear: body.linvel().to_array(),
            angular: angular_velocity(body),
        })
    }
    /// Puts body `id` in contact exclusion `group`: two bodies in the same nonzero group never
    /// touch (no contact forces and no contact events), whatever their
    /// collision layers say. Group `0` removes the body from its group. Joint-connected
    /// neighbours already ignore each other; groups also cover non-adjacent pairs, such as the
    /// bones of one ragdoll. Set the group before stepping.
    pub fn set_contact_exclusion(&mut self, id: u64, group: u64) -> Result<()> {
        let handle = self.core.entry(id)?.collider;
        let collider = self
            .core
            .rapier
            .colliders
            .get_mut(handle)
            .ok_or(Error("unknown body"))?;
        collider.user_data = u128::from(id) | (u128::from(group) << 64);
        collider.set_active_hooks(if group == 0 {
            ActiveHooks::empty()
        } else {
            ActiveHooks::FILTER_CONTACT_PAIRS
        });
        Ok(())
    }
    /// Contact exclusion group of body `id` (`0`: none).
    pub fn contact_exclusion(&self, id: u64) -> Option<u64> {
        let handle = self.core.entry(id).ok()?.collider;
        let collider = self.core.rapier.colliders.get(handle)?;
        Some((collider.user_data >> 64) as u64)
    }
    /// Total mass in kilograms (zero for fixed and kinematic bodies).
    pub fn mass(&self, id: u64) -> Option<f32> {
        Some(self.core.body(id).ok()?.mass())
    }
    pub fn gravity(&self) -> Position {
        self.core.rapier.gravity.to_array()
    }
    /// Replaces gravity and wakes sleeping bodies so they observe the change.
    pub fn set_gravity(&mut self, gravity: Position) -> Result<()> {
        if !crate::finite(&gravity) {
            return Err(Error("invalid gravity"));
        }
        self.core.rapier.gravity = vector(gravity);
        self.core.rapier.wake_up_all(true);
        Ok(())
    }
    pub fn solver_iterations(&self) -> usize {
        self.core
            .rapier
            .integration_parameters
            .num_solver_iterations
    }
    /// Rapier solver iterations per step, 1..=64 (Rapier's default is 4).
    pub fn set_solver_iterations(&mut self, iterations: usize) -> Result<()> {
        if !(1..=crate::MAX_SOLVER_ITERATIONS).contains(&iterations) {
            return Err(Error("solver iterations must be 1..=64"));
        }
        self.core
            .rapier
            .integration_parameters
            .num_solver_iterations = iterations;
        Ok(())
    }
    pub fn apply_angular_impulse(&mut self, id: u64, impulse: AngularImpulse) -> Result<()> {
        if !valid_angular(impulse) {
            return Err(Error("invalid angular impulse"));
        }
        let body = self.core.body_mut(id)?;
        if !body.is_dynamic() {
            return Err(Error("angular impulse requires dynamic body"));
        }
        body.apply_torque_impulse(angular(impulse), true);
        Ok(())
    }
    pub fn apply_impulse(&mut self, id: u64, impulse: Position) -> Result<()> {
        if !crate::finite(&impulse) {
            return Err(Error("invalid impulse"));
        }
        let body = self.core.body_mut(id)?;
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
        let body = self.core.body_mut(id)?;
        if !body.is_kinematic() {
            return Err(Error("target requires kinematic body"));
        }
        body.set_next_kinematic_translation(vector(position));
        body.set_next_kinematic_rotation(rotation(orientation));
        Ok(())
    }
    /// Events are replaced each step, bounding storage even if the host never drains them.
    /// Order: extension `pre_step` hooks, Rapier step, contact events, `post_step` hooks.
    pub fn step(&mut self, dt: f32) -> Result<()> {
        if !dt.is_finite() || !(0.0..=0.25).contains(&dt) || dt == 0.0 {
            return Err(Error("step requires 0 < dt <= 0.25"));
        }
        self.core.rapier.integration_parameters.dt = dt;
        for slot in &mut self.extensions {
            slot.value.pre_step(&mut self.core, dt)?;
        }
        self.core.rapier.step_with_events(&ExclusionHooks, &());
        self.collect_contact_events();
        for slot in &mut self.extensions {
            slot.value.post_step(&mut self.core, dt)?;
        }
        Ok(())
    }
    fn collect_contact_events(&mut self) {
        self.events.clear();
        let narrow = &self.core.rapier.narrow_phase;
        let solids = narrow
            .contact_pairs()
            .filter(|p| p.has_any_active_contact())
            .map(|p| (p.collider1, p.collider2));
        let sensors = narrow
            .intersection_pairs()
            .filter(|(_, _, active)| *active)
            .map(|(a, b, _)| (a, b));
        let mut current = BTreeSet::new();
        for (a, b) in solids.chain(sensors) {
            if let (Some(a), Some(b)) =
                (self.core.collider_body_id(a), self.core.collider_body_id(b))
            {
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
        for (&id, entry) in &self.core.bodies {
            let collider = &self.core.rapier.colliders[entry.collider];
            if collider.collision_groups().memberships.bits() & mask == 0 {
                continue;
            }
            if let Some(distance) = collider.shape().cast_ray(
                self.core.rapier.bodies[entry.body].position(),
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
