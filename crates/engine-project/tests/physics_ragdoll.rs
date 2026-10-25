//! Skeleton ragdolls: build, modes, blending, get-up, hit reactions, events, determinism.
use bevy_ecs::prelude::*;
use engine_project::Result;
use engine_project::simulation::physics::{
    self, HitReaction, Ragdoll, RagdollBone, RagdollMode, RagdollStatus,
};
use engine_project::*;
use glam::{Quat, Vec3};
use serde_json::{Value, json};
use std::fs;

const DT: f32 = 0.02;
const BONES: [&str; 11] = [
    "pelvis",
    "spine",
    "head",
    "arm_l",
    "forearm_l",
    "arm_r",
    "forearm_r",
    "thigh_l",
    "shin_l",
    "thigh_r",
    "shin_r",
];

fn value(data: Value) -> Value {
    json!({"version":1,"data":data})
}
fn node(id: &str, parent: &str, position: [f32; 3]) -> Value {
    json!({"id":id,"name":id,"parent":parent,"components":{"Transform":{"position":position}}})
}
/// A T-pose humanoid: pelvis (root bone), spine, head, two arms, two legs, below a "rig" root.
fn skeleton() -> Vec<Value> {
    vec![
        bind(json!({"id":"rig","name":"rig","components":{"Transform":{"position":[0,0,0]}}})),
        node("pelvis", "rig", [0., 1., 0.]),
        node("spine", "pelvis", [0., 0.25, 0.]),
        node("head", "spine", [0., 0.3, 0.]),
        node("arm_l", "spine", [0.2, 0.2, 0.]),
        node("forearm_l", "arm_l", [0.3, 0., 0.]),
        node("arm_r", "spine", [-0.2, 0.2, 0.]),
        node("forearm_r", "arm_r", [-0.3, 0., 0.]),
        node("thigh_l", "pelvis", [0.1, -0.05, 0.]),
        node("shin_l", "thigh_l", [0., -0.4, 0.]),
        node("thigh_r", "pelvis", [-0.1, -0.05, 0.]),
        node("shin_r", "thigh_r", [0., -0.4, 0.]),
    ]
}
fn floor() -> Value {
    json!({"id":"floor","name":"floor","components":{"Transform":{"position":[0,-0.5,0]},
        "custom":{"engine.PhysicsBody3D":value(json!({"motion":"fixed","shape":"box","half_extents":[20.,0.5,20.]}))}}})
}
fn settings(data: Value) -> Value {
    json!({"id":"settings","name":"settings","components":{"custom":{"engine.PhysicsSettings":value(data)}}})
}
fn bind(mut entity: Value) -> Value {
    entity["components"]["EventBindings"] = json!({"bindings":[
        {"event":"physics.ragdoll.mode_changed","handler":"record"},
        {"event":"physics.ragdoll.settled","handler":"record"}]});
    entity
}
fn fixture(entities: Vec<Value>, clip: Option<Value>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut assets = vec![];
    if let Some(clip) = clip {
        fs::write(
            dir.path().join("clip.json"),
            serde_json::to_vec(&clip).unwrap(),
        )
        .unwrap();
        assets.push(json!({"id":"clip","kind":"animation","path":"clip.json"}));
    }
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"main","entities":entities})).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(
            &json!({"schema":1,"name":"ragdoll-test","startup_scene":"scene.json","assets":assets}),
        )
        .unwrap(),
    )
    .unwrap();
    dir
}
#[derive(Resource, Default)]
struct Events(Vec<(String, Value)>);
fn record(world: &mut World, _: Entity, event: &EngineEvent) -> Result<()> {
    world
        .resource_mut::<Events>()
        .0
        .push((event.name.clone(), event.payload.clone()));
    Ok(())
}
struct Rig {
    _dir: tempfile::TempDir,
    runtime: Runtime,
    rig: Entity,
}
fn start(entities: Vec<Value>, clip: Option<Value>) -> Rig {
    let dir = fixture(entities, clip);
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Events::default());
    register_event_handler(runtime.app_mut(), "record", record).unwrap();
    runtime.start().unwrap();
    let rig = runtime.entity("rig").unwrap();
    Rig {
        _dir: dir,
        runtime,
        rig,
    }
}
/// The humanoid on a floor; the ragdoll is generated from the hierarchy, then tuned by `tune`.
fn humanoid(tune: impl FnOnce(&mut Ragdoll)) -> Rig {
    humanoid_with(vec![], tune)
}
fn humanoid_with(extra: Vec<Value>, tune: impl FnOnce(&mut Ragdoll)) -> Rig {
    let mut entities = skeleton();
    entities.push(floor());
    entities.extend(extra);
    let mut rig = start(entities, None);
    let world = rig.runtime.app_mut().world_mut();
    let mut ragdoll = physics::ragdoll_from_hierarchy(world, rig.rig).unwrap();
    tune(&mut ragdoll);
    physics::add_ragdoll(world, rig.rig, ragdoll).unwrap();
    rig
}
impl Rig {
    fn ticks(&mut self, n: usize) {
        for _ in 0..n {
            self.runtime.tick(DT, InputSnapshot::default()).unwrap();
        }
    }
    fn bone(&self, id: &str) -> Entity {
        self.runtime.entity(id).unwrap()
    }
    fn world(&self) -> &World {
        self.runtime.app().world()
    }
    fn world_mut(&mut self) -> &mut World {
        self.runtime.app_mut().world_mut()
    }
    fn status(&self) -> RagdollStatus {
        physics::ragdoll_status(self.world(), self.rig).unwrap()
    }
    fn mode(&mut self, mode: RagdollMode) {
        let rig = self.rig;
        physics::ragdoll_set_mode(self.world_mut(), rig, mode).unwrap();
    }
    fn local(&self, id: &str) -> (Vec3, Quat) {
        let t = self.world().get::<Transform>(self.bone(id)).unwrap();
        (Vec3::from(t.position), Quat::from_array(t.rotation))
    }
    fn set_rotation(&mut self, id: &str, rotation: Quat) {
        let e = self.bone(id);
        self.world_mut().get_mut::<Transform>(e).unwrap().rotation = rotation.to_array();
    }
    fn events(&self, name: &str) -> Vec<Value> {
        self.world()
            .resource::<Events>()
            .0
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, p)| p.clone())
            .collect()
    }
    fn bone_state(&self, id: &str) -> physics::BoneReadout {
        physics::ragdoll_bone_state(self.world(), self.rig, self.bone(id)).unwrap()
    }
}
fn same_rotation(a: Quat, b: Quat) -> f32 {
    let b = if a.dot(b) < 0. { -b } else { b };
    a.to_array()
        .iter()
        .zip(b.to_array())
        .map(|(x, y)| (x - y).abs())
        .fold(0., f32::max)
}

#[test]
fn the_helper_defines_one_bone_per_entity_with_hinges_at_knees_and_elbows() {
    let rig = humanoid(|_| {});
    let ragdoll = rig.world().get::<Ragdoll>(rig.rig).unwrap();
    assert_eq!(ragdoll.bones.len(), BONES.len());
    let joint = |id: &str| ragdoll.bones.iter().find(|b| b.bone == id).unwrap();
    assert_eq!(joint("forearm_l").joint, physics::BoneJoint::Hinge);
    assert_eq!(joint("shin_r").joint, physics::BoneJoint::Hinge);
    assert_eq!(joint("thigh_l").joint, physics::BoneJoint::Spherical);
    // The forearm hinge axis is perpendicular to the bone (which runs along X).
    assert!(joint("forearm_l").axis[0].abs() < 1e-5);
    // Knee axis perpendicular to the leg (which runs along -Y).
    assert!(joint("shin_l").axis[1].abs() < 1e-5);
    ragdoll.validate().unwrap();
}

#[test]
fn building_creates_a_body_per_bone_and_joint_anchors_coincide() {
    let mut rig = humanoid(|r| r.mode = RagdollMode::Ragdoll);
    rig.ticks(1);
    let (_, worlds) = (0, physics::physics_worlds(rig.world()).unwrap());
    // 11 bones plus the floor.
    assert_eq!(worlds.1.len(), BONES.len() + 1);
    let status = rig.status();
    assert!(status.active);
    assert_eq!(status.bones.len(), BONES.len());
    for id in &BONES[1..] {
        let state = physics::ragdoll_joint_state(rig.world(), rig.rig, rig.bone(id)).unwrap();
        let error = Vec3::from(state.position).length();
        assert!(error < 1e-3, "{id} anchors differ by {error}");
    }
    assert!(physics::ragdoll_joint_state(rig.world(), rig.rig, rig.bone("pelvis")).is_err());
}

#[test]
fn animated_bodies_sit_on_the_bone_joints() {
    let mut rig = humanoid(|_| {});
    rig.ticks(3);
    for id in BONES {
        let bone = rig.bone(id);
        let expected = Vec3::from(world_transform(rig.world(), bone).unwrap().position());
        let state = rig.bone_state(id);
        assert!(
            (state.position - expected).length() < 1e-4,
            "{id}: {:?} vs {expected:?}",
            state.position
        );
        assert!(!state.dynamic);
    }
    // Animated mode never rewrites the transforms.
    assert_eq!(rig.local("arm_l").1, Quat::IDENTITY);
}

#[test]
fn a_limp_ragdoll_falls_settles_above_the_floor_and_respects_joint_limits() {
    let mut rig = humanoid_with(vec![settings(json!({"solver_iterations":16}))], |r| {
        r.mode = RagdollMode::Ragdoll
    });
    let bones = rig.world().get::<Ragdoll>(rig.rig).unwrap().bones.clone();
    // Largest excess over the limit, per tick: hinge range, twist and the two swing axes.
    let excess = |rig: &Rig| {
        let mut worst = 0f32;
        for bone in &bones[..] {
            if bone.bone == "pelvis" {
                continue;
            }
            let a = physics::ragdoll_joint_state(rig.world(), rig.rig, rig.bone(&bone.bone))
                .unwrap()
                .angles;
            let over = |v: f32, l: [f32; 2]| (l[0] - v).max(v - l[1]);
            worst = worst.max(match bone.joint {
                physics::BoneJoint::Hinge => over(a[0], bone.hinge_limits),
                _ => over(a[0], bone.twist_limits)
                    .max(over(a[1], [-bone.swing_limit, bone.swing_limit]))
                    .max(over(a[2], [-bone.swing_limit, bone.swing_limit])),
            });
        }
        worst
    };
    let mut worst = 0f32;
    let mut lowest = f32::MAX;
    let mut settled_at = None;
    let mut final_excess = 0.;
    for tick in 0..1500 {
        rig.ticks(1);
        let status = rig.status();
        lowest = status
            .bones
            .iter()
            .map(|b| b.position.y)
            .fold(lowest, f32::min);
        worst = worst.max(excess(&rig));
        if status.settled && settled_at.is_none() {
            settled_at = Some(tick);
        }
        if settled_at.is_some_and(|t| tick > t + 10) {
            final_excess = excess(&rig);
            break;
        }
    }
    assert!(settled_at.is_some(), "the ragdoll never settled");
    assert!(lowest > -0.01, "a bone went through the floor: {lowest}");
    // Limits are soft constraints: they hold within a transient while landing, and tightly at rest.
    println!("limp: worst excess {worst}, resting {final_excess}");
    assert!(worst < 0.6, "joint limit exceeded by {worst} rad");
    assert!(
        final_excess < 0.15,
        "resting limit excess {final_excess} rad"
    );
    assert_eq!(rig.events("physics.ragdoll.settled").len(), 1);
    assert!(rig.bone_state("pelvis").position.y < 0.4);
    let pelvis = world_transform(rig.world(), rig.bone("pelvis")).unwrap();
    assert!((Vec3::from(pelvis.position()) - rig.bone_state("pelvis").position).length() < 1e-3);
}

#[test]
fn powered_mode_tracks_the_animation_pose_and_collapses_without_strength() {
    let run = |strength: f32| {
        let mut rig = humanoid(|r| {
            r.mode = RagdollMode::Powered;
            r.pin_root = true;
            r.strength = strength;
        });
        rig.ticks(2);
        // The animation pose: raise the left arm and swing a thigh forward.
        rig.set_rotation("arm_l", Quat::from_rotation_z(0.6));
        rig.set_rotation("thigh_l", Quat::from_rotation_x(0.5));
        let mut errors = vec![];
        for tick in 0..200 {
            rig.ticks(1);
            if tick >= 150 {
                errors.push(rig.status().pose_error);
            }
        }
        errors.iter().sum::<f32>() / errors.len() as f32
    };
    let powered = run(1.);
    let limp = run(0.);
    println!("powered {powered} limp {limp}");
    assert!(powered < 0.1, "powered pose error {powered} rad");
    assert!(
        limp > 0.15 && limp > 3. * powered,
        "limp {limp} vs {powered}"
    );
}

#[test]
fn the_blend_weight_mixes_the_animation_and_physics_poses() {
    let mut rig = humanoid(|r| {
        r.mode = RagdollMode::Ragdoll;
        r.blend = 0.5;
    });
    let anim = Quat::from_rotation_z(0.4);
    rig.set_rotation("arm_l", anim);
    let anim_position = rig.local("arm_l").0;
    rig.ticks(40);
    let spine = rig.bone_state("spine");
    let arm = rig.bone_state("arm_l");
    let physics_rotation = spine.rotation.inverse() * arm.rotation;
    let physics_position = spine.rotation.inverse() * (arm.position - spine.position);
    let expected = anim.slerp(physics_rotation, 0.5);
    let (position, rotation) = rig.local("arm_l");
    assert!(
        same_rotation(rotation, expected) < 1e-4,
        "{rotation:?} vs {expected:?}"
    );
    assert!((position - anim_position.lerp(physics_position, 0.5)).length() < 1e-4);
    // The physics pose really differs from the animation pose, so the test is not vacuous.
    assert!(same_rotation(physics_rotation, anim) > 1e-3);
    assert!((rig.status().blend - 0.5).abs() < 1e-6);
}

#[test]
fn switching_from_animated_to_ragdoll_inherits_the_animation_velocity() {
    let run = |inherit: bool| {
        let mut rig = humanoid(|r| r.inherit_velocity = inherit);
        let pelvis = rig.bone("pelvis");
        let advance = |rig: &mut Rig| {
            rig.world_mut()
                .get_mut::<Transform>(pelvis)
                .unwrap()
                .position[0] += 1.0 * DT;
        };
        for _ in 0..10 {
            advance(&mut rig);
            rig.ticks(1);
        }
        advance(&mut rig);
        rig.mode(RagdollMode::Ragdoll);
        rig.ticks(1);
        BONES.map(|id| rig.bone_state(id).linear_velocity)
    };
    for velocity in run(true) {
        assert!(
            (velocity.x - 1.0).abs() < 0.05 && velocity.z.abs() < 0.05,
            "inherited {velocity:?}"
        );
    }
    for velocity in run(false) {
        assert!(velocity.x.abs() < 0.05, "no inheritance but {velocity:?}");
    }
}

#[test]
fn get_up_blend_out_reaches_the_animation_pose_and_returns_to_animated() {
    let mut rig = humanoid(|r| r.mode = RagdollMode::Ragdoll);
    let rest: Vec<(Vec3, Quat)> = BONES.iter().map(|id| rig.local(id)).collect();
    rig.ticks(100);
    assert!(BONES.iter().zip(&rest).any(|(id, r)| {
        let now = rig.local(id);
        (now.0 - r.0).length() > 1e-3 || same_rotation(now.1, r.1) > 1e-2
    }));
    let owner = rig.rig;
    physics::ragdoll_blend_out(rig.world_mut(), owner, 1.0).unwrap();
    rig.ticks(25);
    let middle = rig.status();
    assert!(middle.fading);
    assert!((middle.blend - 0.5).abs() < 0.08, "blend {}", middle.blend);
    rig.ticks(30);
    let end = rig.status();
    assert!(!end.fading && end.mode == RagdollMode::Animated);
    assert_eq!(
        rig.world().get::<Ragdoll>(owner).unwrap().mode,
        RagdollMode::Animated
    );
    for (id, r) in BONES.iter().zip(&rest) {
        let now = rig.local(id);
        assert!((now.0 - r.0).length() < 1e-5, "{id} position");
        assert!(same_rotation(now.1, r.1) < 1e-5, "{id} rotation");
    }
    assert!(end.bones.iter().all(|b| !b.dynamic));
    let changes = rig.events("physics.ragdoll.mode_changed");
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0], json!({"from":"ragdoll","to":"animated"}));
}

#[test]
fn leaving_ragdoll_mode_fades_out_over_blend_out_seconds() {
    let mut rig = humanoid(|r| {
        r.mode = RagdollMode::Ragdoll;
        r.blend_out_seconds = 0.5;
    });
    rig.ticks(40);
    rig.mode(RagdollMode::Animated);
    rig.ticks(5);
    assert!(rig.status().fading);
    assert_eq!(rig.status().mode, RagdollMode::Ragdoll);
    rig.ticks(30);
    assert!(!rig.status().fading);
    assert_eq!(rig.status().mode, RagdollMode::Animated);
    // Returning to ragdoll mid-fade cancels it.
    rig.mode(RagdollMode::Ragdoll);
    rig.ticks(3);
    assert_eq!(rig.status().mode, RagdollMode::Ragdoll);
    assert!(rig.status().bones.iter().any(|b| b.dynamic));
}

#[test]
fn hit_impulses_move_bones_and_reactions_drop_the_powered_strength() {
    let mut rig = humanoid(|r| {
        r.mode = RagdollMode::Powered;
        r.pin_root = true;
    });
    rig.ticks(100);
    let baseline = rig.status().pose_error;
    assert!(baseline < 0.1, "baseline {baseline}");
    let forearm = rig.bone("forearm_l");
    let point = rig.bone_state("forearm_l").position + Vec3::new(0.1, 0., 0.);
    physics::ragdoll_hit(
        rig.world(),
        rig.rig,
        forearm,
        Vec3::new(0., 0., 1.5),
        Some(point),
        HitReaction {
            strength_scale: 0.05,
            recover_seconds: 1.0,
        },
    )
    .unwrap();
    rig.ticks(1);
    let status = rig.status();
    assert!(status.hit_scale < 0.2, "hit scale {}", status.hit_scale);
    assert!(rig.bone_state("forearm_l").linear_velocity.z > 0.5);
    let mut peak = 0f32;
    for _ in 0..25 {
        rig.ticks(1);
        peak = peak.max(rig.status().pose_error);
    }
    assert!(peak > baseline + 0.05, "peak {peak} vs baseline {baseline}");
    rig.ticks(60);
    assert_eq!(rig.status().hit_scale, 1.0);
    // Kinematic (animated) bones refuse impulses.
    rig.mode(RagdollMode::Animated);
    rig.ticks(1);
    assert!(physics::ragdoll_bone_impulse(rig.world(), rig.rig, forearm, Vec3::X, None).is_err());
}

#[test]
fn identical_runs_are_bitwise_identical_with_a_fixed_step() {
    let run = || {
        let mut rig = humanoid_with(vec![settings(json!({"fixed_timestep":0.01}))], |r| {
            r.mode = RagdollMode::Ragdoll
        });
        rig.ticks(40);
        physics::ragdoll_hit(
            rig.world(),
            rig.rig,
            rig.bone("spine"),
            Vec3::new(2., 0., 1.),
            Some(Vec3::new(0., 1.3, 0.)),
            HitReaction {
                strength_scale: 0.5,
                recover_seconds: 0.5,
            },
        )
        .unwrap();
        rig.ticks(160);
        let mut bits = vec![];
        for id in BONES {
            let (p, r) = rig.local(id);
            bits.extend(p.to_array().map(f32::to_bits));
            bits.extend(r.to_array().map(f32::to_bits));
            let s = rig.bone_state(id);
            bits.extend(s.position.to_array().map(f32::to_bits));
        }
        bits
    };
    assert_eq!(run(), run());
}

fn authored(extra: Value) -> Vec<Value> {
    let mut entities = skeleton();
    entities[0]["components"]["custom"] = json!({"engine.Ragdoll":value(extra)});
    entities.push(floor());
    entities
}
fn rejection(entities: Vec<Value>) -> String {
    let dir = fixture(entities, None);
    let result = Project::load(dir.path().join("project.json"))
        .and_then(Runtime::new)
        .and_then(|mut runtime| {
            runtime
                .app_mut()
                .world_mut()
                .insert_resource(Events::default());
            register_event_handler(runtime.app_mut(), "record", record)?;
            runtime.start()?;
            runtime.tick(DT, InputSnapshot::default())
        });
    result.expect_err("scene must be rejected").to_string()
}

#[test]
fn authored_ragdolls_load_and_invalid_data_is_rejected_precisely() {
    let ok = authored(
        json!({"mode":"ragdoll","bones":[{"bone":"pelvis"},{"bone":"spine"},{"bone":"head"}]}),
    );
    let mut rig = start(ok, None);
    rig.ticks(2);
    assert_eq!(rig.status().bones.len(), 3);
    let message = rejection(authored(
        json!({"bones":[{"bone":"pelvis"},{"bone":"pelvis"}]}),
    ));
    assert!(message.contains("twice"), "{message}");
    let message = rejection(authored(json!({"bones":[{"bone":"nobody"}]})));
    assert!(message.contains("nobody"), "{message}");
    let message = rejection(authored(json!({"blend":2.0,"bones":[]})));
    assert!(message.contains("outside bounds"), "{message}");
    let message = rejection(authored(
        json!({"bones":[{"bone":"pelvis","twist_limits":[0.5,-0.5]}]}),
    ));
    assert!(message.contains("limits"), "{message}");
    // spine is missing between pelvis and head.
    let message = rejection(authored(
        json!({"bones":[{"bone":"pelvis"},{"bone":"head"}]}),
    ));
    assert!(message.contains("non-bone"), "{message}");
    // A bone cannot also be a physics body.
    let mut entities = authored(json!({"bones":[{"bone":"pelvis"}]}));
    entities[1]["components"]["custom"] = json!({"engine.PhysicsBody3D":value(json!({}))});
    let message = rejection(entities);
    assert!(message.contains("cannot also"), "{message}");
}

#[test]
fn the_component_is_discoverable_with_remappable_bone_references() {
    let dir = fixture(skeleton(), None);
    let project = Project::load(dir.path().join("project.json")).unwrap();
    let descriptor = project.component_catalog.0.get("engine.Ragdoll").unwrap();
    assert_eq!(descriptor.requires, vec!["Transform".to_string()]);
    assert!(
        descriptor
            .entity_references
            .contains(&"/bones/0/bone".to_string())
    );
    assert!(
        descriptor
            .entity_references
            .contains(&"/bones/63/bone".to_string())
    );
    assert_eq!(
        descriptor.defaults,
        serde_json::to_value(Ragdoll::default()).unwrap()
    );
    let bone = RagdollBone::default();
    assert_eq!(bone.strength, 1.0);
}

#[test]
fn ragdolls_blend_with_a_playing_animation_that_shares_the_bones() {
    let mut entities = skeleton();
    entities[0]["components"]["custom"] = json!({
        "engine.AnimationPlayer":value(json!({"clip":"clip","looping":true,"bindings":BONES})),
        "engine.Ragdoll":value(json!({
            "mode":"powered","pin_root":true,
            "bones":BONES.iter().map(|b| json!({"bone":b})).collect::<Vec<_>>()
        }))
    });
    entities.push(floor());
    let half = 0.4f32;
    let clip = json!({"version":1,"duration":2.,"joints":11,"tracks":[
        {"joint":3,"rotation":{"times":[0.,2.],"values":[[0.,0.,0.,1.],[0.,0.,half.sin(),half.cos()]],"interpolation":"linear"}}]});
    let mut rig = start(entities, Some(clip));
    rig.ticks(60);
    let status = rig.status();
    assert!(status.pose_error < 0.1, "pose error {}", status.pose_error);
    // The left arm follows the animation (angle about Z grows with time).
    let (_, rotation) = rig.local("arm_l");
    let angle = 2. * rotation.z.atan2(rotation.w);
    let expected = 0.8 * 1.2 / 2.;
    assert!(
        (angle - expected).abs() < 0.25,
        "arm angle {angle} vs {expected}"
    );
}

// ---- Critic fixes: per-frame pose, layers, rebuilds, inherited velocity ---------------------

/// The 144 Hz render frame used with a 60 Hz physics step: about 58% of frames run no step.
const FRAME: f32 = 1. / 144.;
const STEP: f32 = 1. / 60.;

impl Rig {
    fn frames(&mut self, n: usize, dt: f32) {
        for _ in 0..n {
            self.runtime.tick(dt, InputSnapshot::default()).unwrap();
        }
    }
    fn world_rotation(&self, id: &str) -> Quat {
        let matrix = world_transform(self.world(), self.bone(id)).unwrap().matrix;
        glam::Mat4::from_cols_array_2d(&matrix)
            .to_scale_rotation_translation()
            .1
    }
    fn world_position(&self, id: &str) -> Vec3 {
        Vec3::from(
            world_transform(self.world(), self.bone(id))
                .unwrap()
                .position(),
        )
    }
}
/// Angle between two rotations; `atan2` keeps full `f32` precision near zero (`acos` of a dot
/// product near 1 only resolves about 1e-3 rad).
fn angle(a: Quat, b: Quat) -> f32 {
    let relative = a.inverse() * b;
    2. * relative.xyz().length().atan2(relative.w.abs())
}
/// A limp ragdoll whose left arm is also driven by a looping animation clip.
fn animated_ragdoll(physics_settings: Value) -> Rig {
    let mut entities = skeleton();
    entities[0]["components"]["custom"] = json!({
        "engine.AnimationPlayer":value(json!({"clip":"clip","looping":true,"bindings":BONES})),
        "engine.Ragdoll":value(json!({
            "mode":"ragdoll",
            "bones":BONES.iter().map(|b| json!({"bone":b})).collect::<Vec<_>>()
        }))
    });
    entities.push(floor());
    entities.push(settings(physics_settings));
    let half = 0.4f32;
    let clip = json!({"version":1,"duration":2.,"joints":11,"tracks":[
        {"joint":3,"rotation":{"times":[0.,2.],"values":[[0.,0.,0.,1.],[0.,0.,half.sin(),half.cos()]],"interpolation":"linear"}}]});
    start(entities, Some(clip))
}

/// H2: the animation service rewrites the bone `Transform`s every frame, so a frame without a
/// physics step must still show the ragdoll pose. At 144 Hz frames and a 60 Hz step the
/// animation pose used to flicker through on every frame that ran no step.
#[test]
fn the_ragdoll_pose_is_rewritten_on_frames_without_a_physics_step() {
    let mut rig = animated_ragdoll(json!({"fixed_timestep":STEP}));
    let mut idle_frames = 0;
    let mut worst = 0f32;
    for frame in 0..120 {
        rig.frames(1, FRAME);
        if frame < 10 {
            continue;
        }
        idle_frames += usize::from(physics::step_status(rig.world()).unwrap().steps == 0);
        for id in ["arm_l", "forearm_l", "spine"] {
            let state = rig.bone_state(id);
            worst = worst
                .max(angle(rig.world_rotation(id), state.rotation))
                .max((rig.world_position(id) - state.position).length());
        }
    }
    println!("{idle_frames} frames without a step; worst pose difference {worst:.2e}");
    assert!(idle_frames > 40, "{idle_frames}");
    assert!(worst < 1e-5, "worst pose difference {worst}");
}

/// H2/D1: with interpolation the ragdoll pose is blended between the previous and the latest
/// physics step on every frame, so the motion is smooth instead of stepping at 60 Hz.
#[test]
fn the_ragdoll_pose_is_interpolated_between_physics_steps() {
    let mut rig = animated_ragdoll(json!({"fixed_timestep":STEP,"interpolation":"interpolate"}));
    let (mut previous, mut latest) = (Vec3::ZERO, Vec3::ZERO);
    let mut heights = vec![];
    let mut worst = 0f32;
    for frame in 0..60 {
        rig.frames(1, FRAME);
        let status = physics::step_status(rig.world()).unwrap();
        if status.steps > 0 {
            (previous, latest) = (latest, rig.bone_state("head").position);
        }
        if frame >= 10 {
            let expected = previous.lerp(latest, status.alpha);
            worst = worst.max((rig.world_position("head") - expected).length());
            heights.push(rig.world_position("head").y);
        }
    }
    println!("worst distance to the blended pose {worst:.2e}");
    assert!(worst < 1e-5, "{worst}");
    // The head keeps falling smoothly until it lands: no frame holds still.
    assert!(
        heights[..24].windows(2).all(|w| w[1] < w[0] - 1e-5),
        "{heights:?}"
    );
}

fn named_floor() -> Value {
    json!({"id":"floor","name":"floor","components":{"Transform":{"position":[0,-0.5,0]},
        "custom":{"engine.PhysicsBody3D":value(json!({"motion":"fixed","shape":"box",
            "half_extents":[20.,0.5,20.],"material":{"layer_name":"world"}}))}}})
}

/// H3: ragdoll bones keep their authored collision groups, so they collide with a floor on a
/// named layer, while bone-bone contacts stay off through the contact exclusion group.
#[test]
fn bones_collide_with_named_layer_floors_but_never_with_each_other() {
    for layer_name in [None, Some("actors")] {
        let mut entities = skeleton();
        entities.push(named_floor());
        entities.push(settings(
            json!({"layers":["world","actors"],"solver_iterations":8}),
        ));
        let mut rig = start(entities, None);
        let world = rig.runtime.app_mut().world_mut();
        let mut ragdoll = physics::ragdoll_from_hierarchy(world, rig.rig).unwrap();
        ragdoll.mode = RagdollMode::Ragdoll;
        ragdoll.layer_name = layer_name.map(str::to_owned);
        physics::add_ragdoll(world, rig.rig, ragdoll).unwrap();
        let bindings = EventBindings {
            bindings: vec![EventBinding {
                event: "physics.contact.start".into(),
                handler: "record".into(),
            }],
        };
        for id in BONES {
            let bone = rig.bone(id);
            rig.world_mut().entity_mut(bone).insert(bindings.clone());
        }
        rig.ticks(150);
        let lowest = BONES
            .iter()
            .map(|id| rig.bone_state(id).position.y)
            .fold(f32::MAX, f32::min);
        let contacts = rig.events("physics.contact.start");
        let between_bones = contacts
            .iter()
            .filter(|c| BONES.contains(&c["other"].as_str().unwrap_or_default()))
            .count();
        println!(
            "layer {layer_name:?}: lowest bone {lowest:.3} m, {} floor contacts, {between_bones} bone contacts",
            contacts.len() - between_bones
        );
        assert!(
            lowest > -0.05,
            "{layer_name:?}: fell through the floor to {lowest}"
        );
        assert!(
            contacts.len() > between_bones,
            "{layer_name:?}: no floor contact"
        );
        assert_eq!(between_bones, 0, "{layer_name:?}");
    }
}

/// M6: changing the settings layers or materials rebuilds ragdoll bodies with the new
/// groups, keeping their pose. The floor is on raw layer bits, so only the bones' own
/// (stale) groups decide whether they still stand on it.
#[test]
fn ragdolls_follow_layer_changes_in_the_physics_settings() {
    let mut entities = skeleton();
    entities.push(floor());
    entities.push(settings(json!({"layers":["world","actors"]})));
    let mut rig = start(entities, None);
    let world = rig.runtime.app_mut().world_mut();
    let mut ragdoll = physics::ragdoll_from_hierarchy(world, rig.rig).unwrap();
    ragdoll.mode = RagdollMode::Ragdoll;
    ragdoll.layer_name = Some("actors".into());
    physics::add_ragdoll(world, rig.rig, ragdoll).unwrap();
    rig.ticks(120);
    let resting = rig.bone_state("pelvis").position;
    assert!(resting.y > -0.05, "{resting}");
    // Stop the actors layer from colliding with the world: the next tick rebuilds the bones
    // at their current pose, and they now fall through.
    let settings_entity = rig.runtime.entity("settings").unwrap();
    rig.world_mut()
        .get_mut::<physics::PhysicsSettings>(settings_entity)
        .unwrap()
        .disabled_pairs = vec![["world".into(), "actors".into()]];
    rig.ticks(1);
    let after_rebuild = rig.bone_state("pelvis");
    assert!(
        (after_rebuild.position - resting).length() < 0.05,
        "the rebuild kept the pose: {} vs {resting}",
        after_rebuild.position
    );
    rig.ticks(120);
    let fallen = rig.bone_state("pelvis").position.y;
    println!(
        "pelvis {} m -> {fallen} m after the pair was disabled",
        resting.y
    );
    assert!(fallen < -1.0, "bones kept their old groups: {fallen}");
}

/// L3: the velocity a ragdoll inherits from the animation is the pose change over the
/// animation time between samples, however many physics steps the frame ran.
#[test]
fn inherited_animation_velocity_does_not_depend_on_the_physics_step_pattern() {
    for lead_frames in 0..5 {
        let mut entities = skeleton();
        entities.push(settings(
            json!({"fixed_timestep":STEP,"gravity_3d":[0.,0.,0.]}),
        ));
        let mut rig = start(entities, None);
        let world = rig.runtime.app_mut().world_mut();
        let ragdoll = physics::ragdoll_from_hierarchy(world, rig.rig).unwrap();
        physics::add_ragdoll(world, rig.rig, ragdoll).unwrap();
        let pelvis = rig.bone("pelvis");
        let advance = |rig: &mut Rig| {
            rig.world_mut()
                .get_mut::<Transform>(pelvis)
                .unwrap()
                .position[0] += 1.0 * FRAME;
        };
        for _ in 0..(12 + lead_frames) {
            advance(&mut rig);
            rig.frames(1, FRAME);
        }
        advance(&mut rig);
        rig.mode(RagdollMode::Ragdoll);
        rig.frames(1, FRAME);
        // Let any step pending in the accumulator run, with no further animation input.
        rig.frames(3, FRAME);
        let speeds: Vec<f32> = BONES
            .iter()
            .map(|id| rig.bone_state(id).linear_velocity.x)
            .collect();
        println!(
            "lead {lead_frames}: pelvis {:.3} m/s, all {speeds:?}",
            speeds[0]
        );
        for speed in speeds {
            assert!(
                (speed - 1.0).abs() < 0.08,
                "lead {lead_frames}: inherited {speed} m/s"
            );
        }
    }
}

/// L4: a ragdoll whose bones do not all fit in the body table is not built, says why in its
/// status, and does not fail the tick.
#[test]
fn a_ragdoll_that_does_not_fit_the_body_table_waits_and_says_why() {
    let mut entities = skeleton();
    entities.push(floor());
    let dir = fixture(entities, None);
    let mut runtime = Runtime::new_with_simulation(
        Project::load(dir.path().join("project.json")).unwrap(),
        Some(engine_project::simulation::SimulationConfig {
            body_capacity: 8,
            ..Default::default()
        }),
    )
    .unwrap();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Events::default());
    register_event_handler(runtime.app_mut(), "record", record).unwrap();
    runtime.start().unwrap();
    let rig = runtime.entity("rig").unwrap();
    let world = runtime.app_mut().world_mut();
    let mut ragdoll = physics::ragdoll_from_hierarchy(world, rig).unwrap();
    ragdoll.mode = RagdollMode::Ragdoll;
    physics::add_ragdoll(world, rig, ragdoll).unwrap();
    for _ in 0..5 {
        runtime.tick(DT, InputSnapshot::default()).unwrap();
    }
    let status = physics::ragdoll_status(runtime.app().world(), rig).unwrap();
    assert!(!status.active);
    let reason = status.inactive_reason.unwrap();
    assert!(reason.contains("capacity"), "{reason}");
    let pressure = physics::pressure_status(runtime.app().world()).unwrap();
    assert_eq!(
        (pressure.bodies_3d, pressure.starved_bodies),
        (1, 1),
        "{pressure:?}"
    );
}
