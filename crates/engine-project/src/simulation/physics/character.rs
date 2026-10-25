//! Authored kinematic character controllers (`engine.CharacterController2D`,
//! `engine.CharacterController3D`) driven by engine-physics' character controller.
//!
//! The component sits on an entity that also carries a **kinematic** `engine.PhysicsBody2D/3D`
//! whose collider (box, ball, capsule, cylinder (3D) or convex hull) is the character shape.
//! Scripts, behaviours or MCP write `desired_velocity` (world space, m/s); the controller adds
//! gravity along `up`, collision response, slopes, autostep, ground snapping, pushing of
//! dynamic bodies and platform riding in every physics step, so movement is deterministic with
//! the fixed-step option. The simulated position is written back to `Transform` after the step.
//!
//! A `NavigationAgent` on the same entity is rejected: navigation writes `Transform`
//! positions directly and would fight the controller. Feed path directions into
//! `desired_velocity` instead. See `docs/physics-character.md`.
use super::super::{invalid, mapped, write_world_pose};
use super::{
    Context, Dimension, Feature, Resolved, Worlds, bodies::Record, emit, service, service_mut,
};
use crate::Result;
use bevy_ecs::prelude::*;
use engine_physics::{dim2, dim3};
use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Ledge stepping settings (see `docs/physics-character.md#autostep`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AutostepSettings {
    /// Highest ledge in meters that is stepped over.
    pub max_height: f32,
    /// Free space in meters required beyond the ledge edge.
    pub min_width: f32,
    /// Whether dynamic bodies count as steps.
    pub include_dynamic_bodies: bool,
}
impl Default for AutostepSettings {
    fn default() -> Self {
        Self {
            max_height: 0.3,
            min_width: 0.1,
            include_dynamic_bodies: true,
        }
    }
}

/// Fields shared by both dimensions' settings, generated once per dimension below.
macro_rules! controller {
    ($component:ident, $n:literal, $dim:ident, $up:expr, $doc:literal) => {
        #[doc = $doc]
        #[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        pub struct $component {
            /// World "up" direction (normalized on use).
            pub up: [f32; $n],
            /// Skin gap in meters kept between the character and what it touches.
            pub offset: f32,
            /// Slide along obstacles instead of stopping at the first hit.
            pub slide: bool,
            /// Steepest walkable slope in radians.
            pub max_slope_climb_angle: f32,
            /// Slopes steeper than this (radians) make a grounded character slide down.
            pub min_slope_slide_angle: f32,
            #[serde(skip_serializing_if = "Option::is_none")]
            pub autostep: Option<AutostepSettings>,
            /// Ground snapping distance in meters; 0 disables snapping.
            pub snap_to_ground: f32,
            /// Character mass in kg for pushing dynamic bodies; 0 disables pushing.
            pub push_mass: f32,
            /// Multiplier on world gravity along `up`; 0 gives a flying character.
            pub gravity_scale: f32,
            /// Desired velocity in m/s (world space), applied every physics step.
            pub desired_velocity: [f32; $n],
            /// Speed in m/s of a jump requested through `jump`.
            pub jump_speed: f32,
            /// Increment to request a jump; ignored unless the character is grounded.
            pub jump: u64,
            /// Smallest touchdown speed in m/s that emits `physics.character.landed`.
            pub landing_min_speed: f32,
        }
        impl Default for $component {
            fn default() -> Self {
                let base = $dim::CharacterDesc::default();
                Self {
                    up: $up,
                    offset: base.offset,
                    slide: base.slide,
                    max_slope_climb_angle: base.max_slope_climb_angle,
                    min_slope_slide_angle: base.min_slope_slide_angle,
                    autostep: None,
                    snap_to_ground: base.snap_to_ground.unwrap_or(0.),
                    push_mass: base.push_mass,
                    gravity_scale: base.gravity_scale,
                    desired_velocity: [0.; $n],
                    jump_speed: 5.,
                    jump: 0,
                    landing_min_speed: 1.,
                }
            }
        }
        impl $component {
            fn desc(&self) -> Result<$dim::CharacterDesc> {
                if !self.desired_velocity.iter().all(|v| v.is_finite())
                    || self.desired_velocity.iter().any(|v| v.abs() > 1e5)
                {
                    return Err(invalid("Character desired_velocity must be finite"));
                }
                if !(0. ..=1000.).contains(&self.jump_speed)
                    || !(0. ..=1000.).contains(&self.landing_min_speed)
                {
                    return Err(invalid(
                        "Character jump_speed and landing_min_speed must be 0..=1000",
                    ));
                }
                $dim::CharacterDesc {
                    up: self.up,
                    offset: self.offset,
                    slide: self.slide,
                    max_slope_climb_angle: self.max_slope_climb_angle,
                    min_slope_slide_angle: self.min_slope_slide_angle,
                    autostep: self.autostep.map(|a| $dim::Autostep {
                        max_height: a.max_height,
                        min_width: a.min_width,
                        include_dynamic_bodies: a.include_dynamic_bodies,
                    }),
                    snap_to_ground: (self.snap_to_ground > 0.).then_some(self.snap_to_ground),
                    push_mass: self.push_mass,
                    gravity_scale: self.gravity_scale,
                    ..Default::default()
                }
                .validate()
                .map_err(mapped)
            }
        }
    };
}
controller!(
    CharacterController2D,
    2,
    dim2,
    [0., 1.],
    "Authored 2D character controller. Requires a kinematic `engine.PhysicsBody2D`."
);
controller!(
    CharacterController3D,
    3,
    dim3,
    [0., 1., 0.],
    "Authored 3D character controller. Requires a kinematic `engine.PhysicsBody3D`."
);

/// What the feature last pushed into engine-physics for one entity.
struct Link<D> {
    desc: D,
    jump: u64,
}
pub(super) struct Characters {
    two: BTreeMap<Entity, Link<dim2::CharacterDesc>>,
    three: BTreeMap<Entity, Link<dim3::CharacterDesc>>,
}
impl Characters {
    pub(super) fn new() -> Self {
        Self {
            two: BTreeMap::new(),
            three: BTreeMap::new(),
        }
    }
}

fn shape_allowed(shape: super::Shape, three: bool) -> bool {
    use super::Shape::*;
    match shape {
        Box | Ball | Capsule | ConvexHull => true,
        Cylinder => three,
        TriMesh | HeightField | Compound => false,
    }
}
fn check_scene(world: &World) -> Result<()> {
    for entity in world.iter_entities() {
        let two = entity.get::<CharacterController2D>();
        let three = entity.get::<CharacterController3D>();
        if two.is_none() && three.is_none() {
            continue;
        }
        if two.is_some() && three.is_some() {
            return Err(invalid("One character controller per entity"));
        }
        if entity.contains::<super::super::NavigationAgent>() {
            return Err(invalid(
                "A character controller cannot share an entity with a NavigationAgent; \
                 write the agent direction to desired_velocity instead",
            ));
        }
        let (motion, shape) = if let Some(c) = two {
            c.desc()?;
            let body = entity.get::<super::PhysicsBody2D>();
            (body.map(|b| b.motion), body.map(|b| b.shape))
        } else {
            three.expect("one controller is present").desc()?;
            let body = entity.get::<super::PhysicsBody3D>();
            (body.map(|b| b.motion), body.map(|b| b.shape))
        };
        let (Some(motion), Some(shape)) = (motion, shape) else {
            return Err(invalid(
                "A character controller requires a PhysicsBody of the same dimension",
            ));
        };
        if motion != super::Motion::Kinematic {
            return Err(invalid(
                "A character controller requires a kinematic physics body",
            ));
        }
        if !shape_allowed(shape, three.is_some()) {
            return Err(invalid(
                "Character shapes are box, ball, capsule, cylinder (3D) or convex_hull",
            ));
        }
    }
    Ok(())
}

macro_rules! dimension {
    ($sync:ident, $events:ident, $component:ident, $dim:ident, $field:ident, $number:literal) => {
        fn $sync(
            worlds: &mut Worlds,
            bodies: &super::Bodies,
            links: &mut BTreeMap<Entity, Link<$dim::CharacterDesc>>,
            world: &World,
        ) -> Result<()> {
            let physics = &mut worlds.$field;
            let stale: Vec<Entity> = links
                .keys()
                .filter(|e| world.get::<$component>(**e).is_none())
                .copied()
                .collect();
            for entity in stale {
                physics.remove_character(entity.to_bits());
                links.remove(&entity);
            }
            for entity in world.iter_entities() {
                let e = entity.id();
                let Some(config) = entity.get::<$component>() else {
                    continue;
                };
                if !bodies.$field.contains_key(&e) {
                    continue;
                }
                let id = e.to_bits();
                let desc = config.desc()?;
                match links.get_mut(&e) {
                    Some(link) if physics.is_character(id) => {
                        if link.desc != desc {
                            physics.set_character_desc(id, desc).map_err(mapped)?;
                            link.desc = desc;
                        }
                    }
                    _ => {
                        physics.insert_character(id, desc).map_err(mapped)?;
                        links.insert(
                            e,
                            Link {
                                desc,
                                jump: config.jump,
                            },
                        );
                    }
                }
                physics
                    .set_character_velocity(id, config.desired_velocity)
                    .map_err(mapped)?;
                let link = links.get_mut(&e).expect("link was inserted above");
                if config.jump != link.jump {
                    link.jump = config.jump;
                    if physics.character_state(id).is_some_and(|s| s.grounded) {
                        physics
                            .character_jump(id, config.jump_speed)
                            .map_err(mapped)?;
                    }
                }
            }
            Ok(())
        }
        fn $events(feature: &dyn Feature, worlds: &mut Worlds, world: &mut World) -> Result<()> {
            for event in worlds.$field.drain_character_events() {
                let Some(target) = Entity::try_from_bits(event.id) else {
                    continue;
                };
                if world.get_entity(target).is_err() {
                    continue;
                }
                let mut payload = json!({"dimension": $number});
                let mut names = Vec::new();
                match event.kind {
                    $dim::CharacterEventKind::Grounded { impact_speed } => {
                        payload["impact_speed"] = json!(impact_speed);
                        names.push("grounded");
                        let threshold = world
                            .get::<$component>(target)
                            .map_or(f32::MAX, |c| c.landing_min_speed);
                        if impact_speed >= threshold {
                            names.push("landed");
                        }
                    }
                    $dim::CharacterEventKind::LeftGround => names.push("left_ground"),
                    $dim::CharacterEventKind::WallStart { normal } => {
                        payload["phase"] = json!("start");
                        payload["normal"] = json!(normal);
                        names.push("wall");
                    }
                    $dim::CharacterEventKind::WallStop { normal } => {
                        payload["phase"] = json!("stop");
                        payload["normal"] = json!(normal);
                        names.push("wall");
                    }
                }
                for name in names {
                    emit(world, feature, name, target, None, payload.clone())?;
                }
            }
            Ok(())
        }
    };
}
dimension!(sync_two, events_two, CharacterController2D, dim2, two, 2);
dimension!(
    sync_three,
    events_three,
    CharacterController3D,
    dim3,
    three,
    3
);

/// Writes the simulated pose of every controlled body back to its `Transform`.
fn write_poses(
    links: impl Iterator<Item = Entity>,
    pose_of: impl Fn(Entity) -> Option<(Vec3, Quat, Vec3)>,
    world: &mut World,
) -> Result<()> {
    let mut writes: Vec<_> = links
        .filter_map(|e| pose_of(e).map(|(s, r, p)| (e, s, r, p)))
        .collect();
    // Parents before children, like dynamic write-back.
    writes.sort_by_key(|(e, ..)| super::bodies::depth(world, *e));
    for (e, scale, rotation, position) in writes {
        if world.get_entity(e).is_ok() {
            write_world_pose(world, e, scale, rotation, position)?;
        }
    }
    Ok(())
}

impl Feature for Characters {
    fn name(&self) -> &'static str {
        "character"
    }
    fn descriptors(&self) -> Vec<crate::ComponentDescriptor> {
        descriptors()
    }
    fn register_components(&self, app: &mut engine_core::EngineApp) -> Result<()> {
        crate::register_component::<CharacterController2D>(app, "engine.CharacterController2D", 1)?;
        crate::register_component::<CharacterController3D>(app, "engine.CharacterController3D", 1)
    }
    fn validate(&self, world: &World, _settings: &Resolved) -> Result<()> {
        check_scene(world)
    }
    fn sync(&mut self, cx: Context<'_>, world: &World) -> Result<()> {
        sync_two(cx.worlds, cx.bodies, &mut self.two, world)?;
        sync_three(cx.worlds, cx.bodies, &mut self.three, world)
    }
    fn after_substep(&mut self, cx: Context<'_>, world: &mut World, _dt: f32) -> Result<()> {
        events_two(self, cx.worlds, world)?;
        events_three(self, cx.worlds, world)
    }
    fn after_step(&mut self, cx: Context<'_>, world: &mut World) -> Result<()> {
        let (worlds, bodies) = (&*cx.worlds, cx.bodies);
        write_poses(
            self.two.keys().copied(),
            |e| pose_two(worlds, bodies.two.get(&e)?, e),
            world,
        )?;
        write_poses(
            self.three.keys().copied(),
            |e| pose_three(worlds, bodies.three.get(&e)?, e),
            world,
        )
    }
}
fn pose_two(
    worlds: &Worlds,
    record: &Record<super::PhysicsBody2D>,
    e: Entity,
) -> Option<(Vec3, Quat, Vec3)> {
    let pose = worlds.two.pose(e.to_bits())?;
    Some((
        record.pose.0,
        Quat::from_rotation_z(pose.rotation),
        Vec3::new(pose.position[0], pose.position[1], record.pose.2.z),
    ))
}
fn pose_three(
    worlds: &Worlds,
    record: &Record<super::PhysicsBody3D>,
    e: Entity,
) -> Option<(Vec3, Quat, Vec3)> {
    let pose = worlds.three.pose(e.to_bits())?;
    Some((
        record.pose.0,
        Quat::from_array(pose.rotation),
        Vec3::from(pose.position),
    ))
}

fn descriptors() -> Vec<crate::ComponentDescriptor> {
    fn schema(n: usize) -> Value {
        let vector = json!({"type":"array","items":{"type":"number"},"minItems":n,"maxItems":n});
        json!({"type":"object","additionalProperties":false,"properties":{
            "up":vector,
            "offset":{"type":"number","minimum":0.0001,"maximum":1,
                "description":"Skin gap in meters kept around the character."},
            "slide":{"type":"boolean"},
            "max_slope_climb_angle":{"type":"number","minimum":0,"maximum":std::f32::consts::FRAC_PI_2,
                "description":"Steepest walkable slope, radians."},
            "min_slope_slide_angle":{"type":"number","minimum":0,"maximum":std::f32::consts::FRAC_PI_2,
                "description":"Slopes steeper than this make a grounded character slide, radians."},
            "autostep":{"type":"object","description":"Present enables ledge stepping.","additionalProperties":false,"properties":{
                "max_height":{"type":"number","minimum":0.000001,"maximum":10000},
                "min_width":{"type":"number","minimum":0,"maximum":100},
                "include_dynamic_bodies":{"type":"boolean"}}},
            "snap_to_ground":{"type":"number","minimum":0,"maximum":10,
                "description":"Ground snapping distance in meters; 0 disables."},
            "push_mass":{"type":"number","minimum":0,"maximum":1000000,
                "description":"Character mass in kg when pushing dynamic bodies; 0 disables pushing."},
            "gravity_scale":{"type":"number","minimum":0,"maximum":100},
            "desired_velocity":{"type":"array","items":{"type":"number"},"minItems":n,"maxItems":n,
                "description":"World-space velocity in m/s applied every physics step."},
            "jump_speed":{"type":"number","minimum":0,"maximum":1000},
            "jump":{"type":"integer","minimum":0,
                "description":"Increment to request a jump of jump_speed while grounded."},
            "landing_min_speed":{"type":"number","minimum":0,"maximum":1000,
                "description":"Smallest touchdown speed that emits physics.character.landed."}
        }})
    }
    let make = |name: &str, defaults: Value, n: usize, body: &str| crate::ComponentDescriptor {
        name: name.into(),
        version: 1,
        schema: schema(n),
        defaults,
        requires: vec!["Transform".into(), body.into()],
        entity_references: vec![],
        data_only: false,
    };
    vec![
        make(
            "engine.CharacterController2D",
            serde_json::to_value(CharacterController2D::default()).expect("finite defaults"),
            2,
            "engine.PhysicsBody2D",
        ),
        make(
            "engine.CharacterController3D",
            serde_json::to_value(CharacterController3D::default()).expect("finite defaults"),
            3,
            "engine.PhysicsBody3D",
        ),
    ]
}

// ---- Native API ----------------------------------------------------------------------

/// Simulated contact state of a character, from [`character_state`]. 2D characters report
/// X/Y vectors with Z 0.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharacterReadout {
    pub dimension: Dimension,
    pub grounded: bool,
    pub on_slope: bool,
    pub sliding: bool,
    pub ground_normal: Vec3,
    /// The entity the character stands on, if it is a physics body.
    pub ground_entity: Option<Entity>,
    pub wall_contact: bool,
    pub wall_normal: Vec3,
    /// Displacement of the last step including platform motion, and its velocity.
    pub translation: Vec3,
    pub velocity: Vec3,
    /// Part of `velocity` inherited from the platform.
    pub ground_velocity: Vec3,
    /// Speed along `up` from gravity and jumps.
    pub vertical_speed: f32,
}
fn vec3(v: [f32; 2]) -> Vec3 {
    Vec3::new(v[0], v[1], 0.)
}
/// Sets `desired_velocity` (world space, m/s) on the entity's character controller; the
/// controller applies it every physics step. 2D controllers use X and Y.
pub fn set_character_velocity(world: &mut World, entity: Entity, velocity: Vec3) -> Result<()> {
    if !velocity.is_finite() {
        return Err(invalid("Character velocity must be finite"));
    }
    if let Some(mut c) = world.get_mut::<CharacterController3D>(entity) {
        c.desired_velocity = velocity.to_array();
        return Ok(());
    }
    let mut c = world
        .get_mut::<CharacterController2D>(entity)
        .ok_or_else(|| invalid("Entity has no character controller"))?;
    c.desired_velocity = [velocity.x, velocity.y];
    Ok(())
}
/// Requests a jump of the controller's `jump_speed`; applied if the character is grounded
/// on the next simulation sync.
pub fn character_jump(world: &mut World, entity: Entity) -> Result<()> {
    if let Some(mut c) = world.get_mut::<CharacterController3D>(entity) {
        c.jump += 1;
        return Ok(());
    }
    let mut c = world
        .get_mut::<CharacterController2D>(entity)
        .ok_or_else(|| invalid("Entity has no character controller"))?;
    c.jump += 1;
    Ok(())
}
/// Sets the character's speed along `up` to `speed` (m/s) on the next physics step,
/// whether or not it is grounded (air jumps, bounce pads). The character must be synced.
pub fn character_impulse(world: &mut World, entity: Entity, speed: f32) -> Result<()> {
    let dimension = if world.get::<CharacterController3D>(entity).is_some() {
        Dimension::Three
    } else if world.get::<CharacterController2D>(entity).is_some() {
        Dimension::Two
    } else {
        return Err(invalid("Entity has no character controller"));
    };
    let id = entity.to_bits();
    let mut physics = service_mut(world)?;
    match dimension {
        Dimension::Two => physics.worlds.two.character_jump(id, speed),
        Dimension::Three => physics.worlds.three.character_jump(id, speed),
    }
    .map_err(mapped)
}
/// Contact and motion state of the entity's character after the last physics step.
pub fn character_state(world: &World, entity: Entity) -> Result<CharacterReadout> {
    let physics = service(world)?;
    let id = entity.to_bits();
    let missing = || invalid("Entity has no synced character controller");
    if let Some(s) = physics.worlds.three.character_state(id) {
        return Ok(CharacterReadout {
            dimension: Dimension::Three,
            grounded: s.grounded,
            on_slope: s.on_slope,
            sliding: s.sliding,
            ground_normal: Vec3::from(s.ground_normal),
            ground_entity: s.ground_body.and_then(Entity::try_from_bits),
            wall_contact: s.wall_contact,
            wall_normal: Vec3::from(s.wall_normal),
            translation: Vec3::from(s.translation),
            velocity: Vec3::from(s.velocity),
            ground_velocity: Vec3::from(s.ground_velocity),
            vertical_speed: s.vertical_speed,
        });
    }
    let s = physics.worlds.two.character_state(id).ok_or_else(missing)?;
    Ok(CharacterReadout {
        dimension: Dimension::Two,
        grounded: s.grounded,
        on_slope: s.on_slope,
        sliding: s.sliding,
        ground_normal: vec3(s.ground_normal),
        ground_entity: s.ground_body.and_then(Entity::try_from_bits),
        wall_contact: s.wall_contact,
        wall_normal: vec3(s.wall_normal),
        translation: vec3(s.translation),
        velocity: vec3(s.velocity),
        ground_velocity: vec3(s.ground_velocity),
        vertical_speed: s.vertical_speed,
    })
}
