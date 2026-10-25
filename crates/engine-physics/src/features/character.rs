//! Kinematic character controllers (docs/physics-character.md).
//!
//! Dimension-generic: `dim2.rs` and `dim3.rs` each include this file with
//! `#[path = "features/character.rs"] mod character;`.
//!
//! A character is an existing kinematic body (its single collider is the character shape).
//! The controller is Rapier's `KinematicCharacterController` (slopes, autostep, snap to
//! ground, sliding, collision impulses on dynamic bodies). The pieces Rapier does not
//! provide are implemented here: gravity and jump velocity, riding moving platforms, the
//! ground body/normal read-back and grounded / wall events.
//!
//! # Step contract
//!
//! Everything happens in the extension `pre_step` hook: the controller moves the body with
//! `set_next_kinematic_translation`, Rapier then carries it to the target during the step.
//! Scene queries use the broad phase built by the previous `step`, so colliders inserted
//! since then are invisible until the next step (the same freshness note as Rapier's own
//! query pipeline); step the world once after building a scene.
//!
//! # Platforms
//!
//! The ground found after a move is remembered as `(body, body-local point)`. On the next
//! step the character receives that point's displacement: exact `next_position - position`
//! for kinematic platforms (including rotation), `velocity * dt` for dynamic ones. The
//! character's own motion is computed relative to the platform. Rapier's controller adds
//! the velocity of a kinematic parent itself, but only while it resolves a hit (and its
//! velocity setters cannot zero a position-based body), so a standing character is not pushed
//! into the floor with gravity: it is held by snap-to-ground, which avoids double counting.
//! A character pressed into a moving kinematic platform by a wall or slope may still
//! receive Rapier's platform velocity as well.
use super::*;
use crate::{Error, Result};
use rapier::control::{CharacterCollision, CharacterLength, KinematicCharacterController};
use rapier::parry::query::ShapeCastOptions;
use rapier::pipeline::{QueryFilter as RapierFilter, QueryFilterFlags, QueryPipeline};
use std::collections::BTreeMap;

/// Most undrained character events kept; later events are dropped (and counted).
pub const MAX_CHARACTER_EVENTS: usize = 65_536;
/// Largest vertical speed (m/s) the controller integrates.
const MAX_VERTICAL_SPEED: f32 = 1000.;
/// Ground contact normals within this angle (radians) of `up` are not "on a slope".
const FLAT_ANGLE: f32 = 0.035;
/// Extra distance (meters) beyond the skin used to look for the ground body.
const GROUND_PROBE: f32 = 0.06;
/// A wall hit has a normal no more than ~105 degrees from `up`.
const CEILING_DOT: f32 = -0.26;
/// Seconds a wall contact persists after the last hit.
const WALL_HOLD: f32 = 0.05;

/// Automatic stepping over small ledges (Rapier `CharacterAutostep`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Autostep {
    /// Highest ledge in meters that is stepped over.
    pub max_height: f32,
    /// Free space in meters required beyond the ledge edge.
    pub min_width: f32,
    /// Whether dynamic bodies count as steps.
    pub include_dynamic_bodies: bool,
}
impl Default for Autostep {
    fn default() -> Self {
        Self {
            max_height: 0.3,
            min_width: 0.1,
            include_dynamic_bodies: true,
        }
    }
}

/// Controller settings. Lengths are meters, angles radians.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharacterDesc {
    /// World "up" direction (normalized on insertion). Default +Y.
    pub up: Position,
    /// Gap kept between the character and what it touches (`0.0001..=1`).
    pub offset: f32,
    /// Slide along obstacles instead of stopping at the first hit.
    pub slide: bool,
    /// Steepest slope the character walks up (`0..=pi/2`).
    pub max_slope_climb_angle: f32,
    /// Slopes steeper than this make a grounded character slide down (`0..=pi/2`).
    pub min_slope_slide_angle: f32,
    pub autostep: Option<Autostep>,
    /// Keeps the character on the ground when it is at most this far above it (`0..=10`).
    pub snap_to_ground: Option<f32>,
    /// Mass in kg used when pushing dynamic bodies it walks into; `0` disables pushing.
    /// Rapier transfers the character velocity scaled by `m_body * m / (m_body + m)`, so a
    /// heavier character pushes harder.
    pub push_mass: f32,
    /// Multiplier on the world gravity along `up` (`0..=100`); `0` gives a flying character.
    pub gravity_scale: f32,
    /// Rapier's `normal_nudge_factor` (`0..=0.1`).
    pub normal_nudge: f32,
}
impl Default for CharacterDesc {
    fn default() -> Self {
        let mut up = Position::default();
        up[1] = 1.;
        Self {
            up,
            offset: 0.01,
            slide: true,
            max_slope_climb_angle: std::f32::consts::FRAC_PI_4,
            min_slope_slide_angle: std::f32::consts::FRAC_PI_4,
            autostep: None,
            snap_to_ground: Some(0.2),
            push_mass: 0.,
            gravity_scale: 1.,
            normal_nudge: 1e-4,
        }
    }
}
impl CharacterDesc {
    /// Checks every field and returns the settings with `up` normalized.
    pub fn validate(self) -> Result<Self> {
        let half_pi = std::f32::consts::FRAC_PI_2;
        let up = vector(self.up);
        let length = up.length();
        let ok = crate::finite(&self.up)
            && (1e-3..=1e8).contains(&length)
            && (1e-4..=1.).contains(&self.offset)
            && (0. ..=half_pi).contains(&self.max_slope_climb_angle)
            && (0. ..=half_pi).contains(&self.min_slope_slide_angle)
            && self.snap_to_ground.is_none_or(|d| (0. ..=10.).contains(&d))
            && (0. ..=1e6).contains(&self.push_mass)
            && (0. ..=100.).contains(&self.gravity_scale)
            && (0. ..=0.1).contains(&self.normal_nudge)
            && self.autostep.is_none_or(|a| {
                crate::dimension(a.max_height) && (0. ..=100.).contains(&a.min_width)
            });
        if !ok {
            return Err(Error("invalid character controller settings"));
        }
        Ok(Self {
            up: (up / length).to_array(),
            ..self
        })
    }
    fn controller(&self) -> KinematicCharacterController {
        KinematicCharacterController {
            up: vector(self.up),
            offset: CharacterLength::Absolute(self.offset),
            slide: self.slide,
            // Autostep is implemented by `try_autostep`.
            autostep: None,
            max_slope_climb_angle: self.max_slope_climb_angle,
            min_slope_slide_angle: self.min_slope_slide_angle,
            snap_to_ground: self.snap_to_ground.map(CharacterLength::Absolute),
            normal_nudge_factor: self.normal_nudge,
        }
    }
}

/// Read-back of the last controller step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharacterState {
    /// Touching walkable ground and not moving upward.
    pub grounded: bool,
    /// Grounded on a surface tilted more than about 2 degrees from `up`.
    pub on_slope: bool,
    /// Grounded on a slope steeper than `min_slope_slide_angle` and moving down it.
    pub sliding: bool,
    /// Ground surface normal (`up` when not grounded).
    pub ground_normal: Position,
    /// Application body the character stands on, if it is one.
    pub ground_body: Option<u64>,
    /// Pushed against a wall (surface steeper than `max_slope_climb_angle`) this step.
    pub wall_contact: bool,
    /// Normal of the last wall touched, facing the character (zero until one was touched).
    pub wall_normal: Position,
    /// Total displacement applied this step, including platform motion.
    pub translation: Position,
    /// `translation / dt`.
    pub velocity: Position,
    /// Platform displacement per second included in `velocity`.
    pub ground_velocity: Position,
    /// Speed along `up` integrated by gravity and jumps.
    pub vertical_speed: f32,
}

/// A change in a character's contact state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CharacterEventKind {
    /// Became grounded; `impact_speed` is the downward speed (m/s) at touchdown.
    Grounded {
        impact_speed: f32,
    },
    LeftGround,
    WallStart {
        normal: Position,
    },
    WallStop {
        normal: Position,
    },
}
/// An event of character `id`, in step order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharacterEvent {
    pub id: u64,
    pub kind: CharacterEventKind,
}

struct Ground {
    body: u64,
    local: Position,
}
struct Controller {
    desc: CharacterDesc,
    velocity: Position,
    vertical: f32,
    state: CharacterState,
    ground: Option<Ground>,
    /// Grounded on a slope steeper than `min_slope_slide_angle`.
    steep: bool,
    wall_timer: f32,
}
#[derive(Default)]
struct Characters {
    map: BTreeMap<u64, Controller>,
    events: Vec<CharacterEvent>,
    dropped: u64,
}
impl Extension for Characters {
    fn order(&self) -> u16 {
        20
    }
    fn pre_step(&mut self, core: &mut Core, dt: f32) -> Result<()> {
        let ids: Vec<u64> = self.map.keys().copied().collect();
        for id in ids {
            if let Some(controller) = self.map.get_mut(&id) {
                for kind in step_one(core, id, controller, dt)? {
                    if self.events.len() < MAX_CHARACTER_EVENTS {
                        self.events.push(CharacterEvent { id, kind });
                    } else {
                        self.dropped += 1;
                    }
                }
            }
        }
        Ok(())
    }
    fn body_removed(&mut self, _core: &mut Core, id: u64) {
        self.map.remove(&id);
        for controller in self.map.values_mut() {
            if controller.ground.as_ref().is_some_and(|g| g.body == id) {
                controller.ground = None;
            }
        }
    }
}

fn angle_from_up(normal: Vector, up: Vector) -> f32 {
    normal.dot(up).clamp(-1., 1.).acos()
}

/// Displacement of the remembered ground point over the coming step.
fn platform_displacement(core: &Core, ground: Option<&Ground>, dt: f32) -> Vector {
    let Some(ground) = ground else {
        return Vector::ZERO;
    };
    let Ok(body) = core.body(ground.body) else {
        return Vector::ZERO;
    };
    let local = vector(ground.local);
    let now = body.position().transform_point(local);
    if body.is_kinematic() {
        body.next_position().transform_point(local) - now
    } else if body.is_dynamic() {
        body.velocity_at_point(now) * dt
    } else {
        Vector::ZERO
    }
}

fn step_one(
    core: &mut Core,
    id: u64,
    c: &mut Controller,
    dt: f32,
) -> Result<Vec<CharacterEventKind>> {
    let entry = core.entry(id)?;
    let (pose, shape, groups) = {
        let body = core.body(id)?;
        let collider = core
            .rapier
            .colliders
            .get(entry.collider)
            .ok_or(Error("unknown body"))?;
        (
            *body.position(),
            collider.shared_shape().clone(),
            collider.collision_groups(),
        )
    };
    let controller = c.desc.controller();
    let up = controller.up;
    let mut out = Vec::new();

    // A character standing on walkable ground is held there by snapping, not by a gravity
    // push. Pushing into the ground every step makes Rapier resolve a hit each time, which
    // double counts kinematic platform motion (see the module docs) and, in 2D, can drop the
    // whole horizontal movement when float noise tilts the contact normal (its 2D slope
    // handling keeps no horizontal tangent). Steep slopes still get gravity so they slide.
    if c.state.grounded && !c.steep && c.vertical <= 0. {
        c.vertical = 0.;
    } else {
        let gravity_up = core.rapier.gravity.dot(up) * c.desc.gravity_scale;
        c.vertical = (c.vertical + gravity_up * dt).clamp(-MAX_VERTICAL_SPEED, MAX_VERTICAL_SPEED);
    }
    let rising = c.vertical > 0.;
    let platform = platform_displacement(core, c.ground.as_ref(), dt);
    let desired = (vector(c.velocity) + up * c.vertical) * dt;

    let filter = RapierFilter::new()
        .exclude_rigid_body(entry.body)
        .exclude_sensors()
        .groups(groups);

    let mut collisions: Vec<CharacterCollision> = Vec::new();
    let movement = {
        let rapier = &core.rapier;
        let queries = rapier.broad_phase.as_query_pipeline(
            rapier.narrow_phase.query_dispatcher(),
            &rapier.bodies,
            &rapier.colliders,
            filter,
        );
        controller.move_shape(dt, &queries, &*shape, &pose, desired, |hit| {
            collisions.push(hit);
        })
    };
    let mut translation = movement.translation;

    if c.desc.push_mass > 0. && !collisions.is_empty() {
        let rapier = &mut core.rapier;
        let mut queries = rapier.broad_phase.as_query_pipeline_mut(
            rapier.narrow_phase.query_dispatcher(),
            &mut rapier.bodies,
            &mut rapier.colliders,
            filter,
        );
        controller.solve_character_collision_impulses(
            dt,
            &mut queries,
            &*shape,
            c.desc.push_mass,
            &collisions,
        );
    }

    // Walls and ceilings from the collisions of this move.
    let mut wall = None;
    let mut hit_ceiling = false;
    for collision in &collisions {
        let normal = collision.hit.normal1;
        let facing = normal.dot(up);
        if facing < -0.5 {
            hit_ceiling = true;
        }
        if facing > CEILING_DOT && angle_from_up(normal, up) >= c.desc.max_slope_climb_angle {
            wall = Some(normal);
        }
    }
    if hit_ceiling && c.vertical > 0. {
        c.vertical = 0.;
    }

    // Own autostep (see `try_autostep`) when blocked by a wall while on the ground.
    if let (Some(step), Some(_), true) = (c.desc.autostep, wall, movement.grounded && !rising) {
        let rapier = &core.rapier;
        let mut step_filter = filter;
        if !step.include_dynamic_bodies {
            step_filter.flags |= QueryFilterFlags::EXCLUDE_DYNAMIC;
        }
        let queries = rapier.broad_phase.as_query_pipeline(
            rapier.narrow_phase.query_dispatcher(),
            &rapier.bodies,
            &rapier.colliders,
            step_filter,
        );
        let reached = Pose::from_translation(translation) * pose;
        if let Some(extra) = try_autostep(
            &queries,
            &*shape,
            &reached,
            desired - translation,
            &c.desc,
            step,
        ) {
            translation += extra;
        }
    }

    // Ground body, normal and the point remembered for platform riding.
    let moved = Pose::from_translation(translation) * pose;
    let mut ground_normal = up;
    let mut ground = None;
    if movement.grounded {
        let rapier = &core.rapier;
        let queries = rapier.broad_phase.as_query_pipeline(
            rapier.narrow_phase.query_dispatcher(),
            &rapier.bodies,
            &rapier.colliders,
            filter,
        );
        let probe = c.desc.offset + GROUND_PROBE;
        let hit = queries.cast_shape(
            &moved,
            -up,
            &*shape,
            ShapeCastOptions {
                target_distance: 0.,
                stop_at_penetration: false,
                max_time_of_impact: probe,
                compute_impact_geometry_on_penetration: true,
            },
        );
        if let Some((handle, hit)) = hit {
            ground_normal = hit.normal1;
            if let Some(body_id) = core.collider_body_id(handle)
                && let Ok(body) = core.body(body_id)
            {
                let local = body.position().inverse_transform_point(hit.witness1);
                ground = Some(Ground {
                    body: body_id,
                    local: local.to_array(),
                });
            }
        }
    }

    let total = translation + platform;
    let grounded = movement.grounded && !rising;
    let slope_angle = angle_from_up(ground_normal, up);
    let on_slope = grounded && slope_angle > FLAT_ANGLE;
    c.steep = grounded && slope_angle > c.desc.min_slope_slide_angle;
    let sliding = c.steep && translation.dot(up) < 0.;
    let was = c.state;
    if grounded && !was.grounded {
        out.push(CharacterEventKind::Grounded {
            impact_speed: (-c.vertical).max(0.),
        });
    } else if !grounded && was.grounded {
        out.push(CharacterEventKind::LeftGround);
    }
    // Wall contact is held for a short time so a character pressed against a wall does not
    // flicker (the controller keeps a skin gap and some steps report no hit).
    c.wall_timer = if wall.is_some() {
        WALL_HOLD
    } else {
        (c.wall_timer - dt).max(0.)
    };
    let wall_contact = c.wall_timer > 0.;
    match (was.wall_contact, wall_contact, wall) {
        (false, true, Some(normal)) => out.push(CharacterEventKind::WallStart {
            normal: normal.to_array(),
        }),
        (true, false, _) => out.push(CharacterEventKind::WallStop {
            normal: was.wall_normal,
        }),
        _ => {}
    }
    if grounded && !c.steep {
        c.vertical = 0.;
    }
    c.ground = if grounded { ground } else { None };
    c.state = CharacterState {
        grounded,
        on_slope,
        sliding,
        ground_normal: ground_normal.to_array(),
        ground_body: c.ground.as_ref().map(|g| g.body),
        wall_contact,
        wall_normal: wall.map_or(was.wall_normal, |n| n.to_array()),
        translation: total.to_array(),
        velocity: (total / dt).to_array(),
        ground_velocity: (platform / dt).to_array(),
        vertical_speed: c.vertical,
    };

    let body = core.body_mut(id)?;
    let target = body.translation() + total;
    body.set_next_kinematic_translation(target);
    Ok(out)
}

/// Steps over a ledge the character is blocked by, returning the extra translation.
///
/// Rapier's own autostep is not used: it did not step reliably in tests (cause not isolated)
/// and its 2D wall test uses a signed angle so walls facing -x are never walls. This sweep
/// does the same job in three casts from the pose `at` where the character was stopped:
///
/// 1. up by `max_height` (shortened by a ceiling);
/// 2. forward by `min_width` at that height (a taller obstacle blocks it);
/// 3. down by the raised height, which must land on walkable ground. The step height is the
///    raised height minus the fall distance.
///
/// The character is then raised by the step height and nudged forward by at most
/// `min_width`; the rest of its motion continues on the next step.
fn try_autostep(
    queries: &QueryPipeline,
    shape: &dyn rapier::parry::shape::Shape,
    at: &Pose,
    remaining: Vector,
    desc: &CharacterDesc,
    step: Autostep,
) -> Option<Vector> {
    let up = vector(desc.up);
    let horizontal = remaining - up * remaining.dot(up);
    let length = horizontal.length();
    if length < 1e-5 {
        return None;
    }
    let direction = horizontal / length;
    let cast = |pose: &Pose, velocity: Vector, distance: f32, target: f32| {
        queries.cast_shape(
            pose,
            velocity,
            shape,
            ShapeCastOptions {
                target_distance: target,
                stop_at_penetration: false,
                max_time_of_impact: distance,
                compute_impact_geometry_on_penetration: true,
            },
        )
    };
    let reach = step.max_height + desc.offset;
    let raise = cast(at, up, reach, 0.).map_or(reach, |(_, hit)| hit.time_of_impact);
    if raise <= desc.offset {
        return None;
    }
    let raised = Pose::from_translation(up * raise) * *at;
    let width = step.min_width + desc.offset;
    if cast(&raised, direction, width, desc.offset).is_some() {
        return None;
    }
    let ahead = Pose::from_translation(direction * width) * raised;
    let (_, landing) = cast(&ahead, -up, raise, desc.offset)?;
    let height = raise - landing.time_of_impact;
    let walkable = landing.normal1.dot(up) >= desc.max_slope_climb_angle.cos();
    if landing.time_of_impact <= 1e-6 || height <= desc.offset * 0.5 || !walkable {
        return None;
    }
    Some(up * height + direction * length.min(width))
}

fn debug_characters(world: &World, sink: &mut DebugSink) {
    let Some(table) = world.extension::<Characters>() else {
        return;
    };
    for (&id, c) in &table.map {
        let Ok(body) = world.core.body(id) else {
            continue;
        };
        let origin = body.translation();
        sink.anchor(id, "character", origin.to_array());
        let tip =
            |direction: Position, length: f32| (origin + vector(direction) * length).to_array();
        if c.state.grounded {
            sink.line(
                origin.to_array(),
                tip(c.state.ground_normal, 0.75),
                [0.2, 0.9, 0.3, 1.],
            );
        }
        if c.state.wall_contact {
            sink.line(
                origin.to_array(),
                tip(c.state.wall_normal, 0.75),
                [0.95, 0.3, 0.2, 1.],
            );
        }
    }
}

impl World {
    /// Turns kinematic body `id` into a character controller. Its collider is the character
    /// shape; capsules (Y-axis, so `up` should stay +Y unless the shape is rotated), balls
    /// and boxes work best.
    pub fn insert_character(&mut self, id: u64, desc: CharacterDesc) -> Result<()> {
        let desc = desc.validate()?;
        if !self.core.body(id)?.is_kinematic() {
            return Err(Error("character requires a kinematic body"));
        }
        self.add_debug_contributor("character", debug_characters);
        let (_, table) = self.extension_mut::<Characters>();
        if table.map.contains_key(&id) {
            return Err(Error("body is already a character"));
        }
        let mut zero = Position::default();
        zero.fill(0.);
        table.map.insert(
            id,
            Controller {
                desc,
                velocity: zero,
                vertical: 0.,
                state: CharacterState {
                    grounded: false,
                    on_slope: false,
                    sliding: false,
                    ground_normal: desc.up,
                    ground_body: None,
                    wall_contact: false,
                    wall_normal: zero,
                    translation: zero,
                    velocity: zero,
                    ground_velocity: zero,
                    vertical_speed: 0.,
                },
                ground: None,
                steep: false,
                wall_timer: 0.,
            },
        );
        Ok(())
    }
    /// Stops controlling `id`; the body stays a plain kinematic body.
    pub fn remove_character(&mut self, id: u64) -> bool {
        let (_, table) = self.extension_mut::<Characters>();
        table.map.remove(&id).is_some()
    }
    pub fn is_character(&self, id: u64) -> bool {
        self.extension::<Characters>()
            .is_some_and(|t| t.map.contains_key(&id))
    }
    pub fn character_desc(&self, id: u64) -> Option<CharacterDesc> {
        Some(self.extension::<Characters>()?.map.get(&id)?.desc)
    }
    /// Replaces the settings of an existing character, keeping its velocity and state.
    pub fn set_character_desc(&mut self, id: u64, desc: CharacterDesc) -> Result<()> {
        let desc = desc.validate()?;
        self.character_mut(id)?.desc = desc;
        Ok(())
    }
    fn character_mut(&mut self, id: u64) -> Result<&mut Controller> {
        let (_, table) = self.extension_mut::<Characters>();
        table.map.get_mut(&id).ok_or(Error("unknown character"))
    }
    /// Desired velocity (m/s, world space) applied every step until changed. Gravity and
    /// jumps are added on top along `up`.
    pub fn set_character_velocity(&mut self, id: u64, velocity: Position) -> Result<()> {
        if !crate::finite(&velocity) || vector(velocity).length() > 1e5 {
            return Err(Error("invalid character velocity"));
        }
        self.character_mut(id)?.velocity = velocity;
        Ok(())
    }
    /// Equivalent to [`World::set_character_velocity`] with `translation / dt`.
    pub fn move_character(&mut self, id: u64, translation: Position, dt: f32) -> Result<()> {
        if !dt.is_finite() || dt <= 0. || !crate::finite(&translation) {
            return Err(Error("invalid character movement"));
        }
        self.set_character_velocity(id, (vector(translation) / dt).to_array())
    }
    /// Sets the speed along `up` (m/s); the character leaves the ground on its next step.
    pub fn character_jump(&mut self, id: u64, speed: f32) -> Result<()> {
        if !speed.is_finite() || !(0. ..=1000.).contains(&speed) {
            return Err(Error("invalid jump speed"));
        }
        self.character_mut(id)?.vertical = speed;
        Ok(())
    }
    pub fn character_state(&self, id: u64) -> Option<CharacterState> {
        Some(self.extension::<Characters>()?.map.get(&id)?.state)
    }
    /// Characters in ascending id order.
    pub fn character_ids(&self) -> Vec<u64> {
        self.extension::<Characters>()
            .map_or_else(Vec::new, |t| t.map.keys().copied().collect())
    }
    /// Takes the events collected since the last drain (at most [`MAX_CHARACTER_EVENTS`]).
    pub fn drain_character_events(&mut self) -> Vec<CharacterEvent> {
        let (_, table) = self.extension_mut::<Characters>();
        std::mem::take(&mut table.events)
    }
    /// Events dropped because the buffer was full.
    pub fn dropped_character_events(&self) -> u64 {
        self.extension::<Characters>().map_or(0, |t| t.dropped)
    }
}
