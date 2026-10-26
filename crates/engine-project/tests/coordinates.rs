//! Large-world coordinates: floating-origin rebasing, f64 authoring, physics, navigation,
//! world features and render extraction far from the origin.
use bevy_ecs::prelude::*;
use engine_project::simulation::physics::{self, BodyState};
use engine_project::simulation::{
    BakeInput, BakeSettings, NavMeshDocument, NavigationAgent, Obstacle,
};
use engine_project::world::coordinates::{
    self, AbsolutePosition, EVENT_ORIGIN_SHIFTED, ShiftCause, WorldOrigin,
};
use engine_project::world::{foliage, pcg::PcgStatus, terrain};
use engine_project::{InputSnapshot, Project, Runtime, Transform};
use glam::{Mat4, Vec3};
use serde_json::{Value, json};
use std::fs;

const STEP: f32 = 1.0 / 60.0;
const MESH: &str = "CGMESH 1\nvertex -1 -1 0 0 0\nvertex 1 -1 0 1 0\nvertex 0 1 0 0 1\n";

fn custom(entries: &[(&str, Value)]) -> Value {
    let mut map = serde_json::Map::new();
    for (name, data) in entries {
        map.insert((*name).into(), json!({"version": 1, "data": data}));
    }
    Value::Object(map)
}
fn entity(
    id: &str,
    position: [f32; 3],
    mut components: Value,
    custom_entries: &[(&str, Value)],
) -> Value {
    components["Transform"] = json!({ "position": position });
    if !custom_entries.is_empty() {
        components["custom"] = custom(custom_entries);
    }
    json!({"id": id, "name": id, "components": components})
}
fn absolute(position: [f64; 3]) -> (&'static str, Value) {
    ("engine.AbsolutePosition", json!({ "position": position }))
}
fn large_world(threshold: f64, snap: u32, anchor: &str) -> (&'static str, Value) {
    (
        "engine.LargeWorld",
        json!({"threshold": threshold, "snap": snap, "anchor": anchor}),
    )
}
fn physics_settings() -> (&'static str, Value) {
    ("engine.PhysicsSettings", json!({ "fixed_timestep": STEP }))
}

struct Fixture {
    dir: tempfile::TempDir,
}
impl Fixture {
    fn new(entities: Vec<Value>, assets: Vec<Value>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("mesh.cgmesh"), MESH).unwrap();
        let mut all = vec![json!({"id": "mesh", "kind": "mesh", "path": "mesh.cgmesh"})];
        all.extend(assets);
        fs::write(
            dir.path().join("scene.json"),
            json!({"schema": 1, "scene_id": "main", "entities": entities}).to_string(),
        )
        .unwrap();
        fs::write(
            dir.path().join("project.json"),
            json!({"schema": 1, "name": "coordinates-test", "startup_scene": "scene.json", "assets": all})
                .to_string(),
        )
        .unwrap();
        Self { dir }
    }
    fn project(&self) -> engine_project::Result<Project> {
        Project::load(self.dir.path().join("project.json"))
    }
    fn runtime(&self) -> Runtime {
        let mut runtime = Runtime::new(self.project().unwrap()).unwrap();
        runtime.start().unwrap();
        runtime
    }
}

fn tick(runtime: &mut Runtime, n: usize, dt: f32) {
    for _ in 0..n {
        runtime.tick(dt, InputSnapshot::default()).unwrap();
    }
}
fn id(runtime: &Runtime, name: &str) -> Entity {
    runtime
        .entity(name)
        .unwrap_or_else(|| panic!("entity {name}"))
}
fn abs(runtime: &Runtime, name: &str) -> [f64; 3] {
    coordinates::absolute_position(runtime.app().world(), id(runtime, name)).unwrap()
}
fn local(runtime: &Runtime, name: &str) -> [f32; 3] {
    runtime
        .app()
        .world()
        .get::<Transform>(id(runtime, name))
        .unwrap()
        .position
}
fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| (a[i] - b[i]).abs()).fold(0.0, f64::max)
}
fn state(runtime: &Runtime, name: &str) -> BodyState {
    physics::body_state(runtime.app().world(), id(runtime, name)).unwrap()
}

// ---- f64 authoring and exact shifts ---------------------------------------------------------

#[test]
fn absolute_positions_survive_repeated_shifts_exactly() {
    let far = [12_345_678.123_456, 1.5, -9_876_543.987_654];
    let fixture = Fixture::new(
        vec![
            entity("far", [0.0; 3], json!({}), &[absolute(far)]),
            entity("plain", [100.25, 0.0, 3.5], json!({}), &[]),
            json!({"id":"child","name":"child","parent":"plain","components":{"Transform":{"position":[1.0,2.0,3.0]}}}),
        ],
        vec![],
    );
    let mut runtime = fixture.runtime();
    // The hook converted the f64 position on insertion (offset 0: rounded to f32 once).
    assert_eq!(
        local(&runtime, "far"),
        [far[0] as f32, far[1] as f32, far[2] as f32]
    );
    tick(&mut runtime, 1, 0.1);
    let plain = abs(&runtime, "plain");
    let child = abs(&runtime, "child");
    // 100 shifts on a 512 m grid toward and around the far entity.
    let mut worst_far: f64 = 0.0;
    let mut worst_plain: f64 = 0.0;
    let mut worst_child: f64 = 0.0;
    for n in 1..=100_i32 {
        let k = f64::from(n);
        let offset = [
            (12_345_678.0_f64 / 512.0).round() * 512.0 + 512.0 * f64::from(n % 7 - 3),
            512.0 * f64::from(n % 3 - 1),
            (-9_876_543.0_f64 / 512.0).round() * 512.0 - 512.0 * (k % 5.0),
        ];
        let world = runtime.app_mut().world_mut();
        let report = coordinates::rebase_now(world, offset).unwrap();
        assert_eq!(report.offset, offset);
        assert_eq!(coordinates::origin(world), offset);
        worst_far = worst_far.max(distance(abs(&runtime, "far"), far));
        // Entities near the origin are moved 12e6 m away and back: f32 locals are rounded.
        worst_plain = worst_plain.max(distance(abs(&runtime, "plain"), plain));
        worst_child = worst_child.max(distance(abs(&runtime, "child"), child));
    }
    assert!(
        worst_far <= 1e-6,
        "f64-authored entity drifted {worst_far} m"
    );
    assert_eq!(worst_far, 0.0, "recomputed from f64 every time");
    let status = coordinates::status(runtime.app().world()).unwrap();
    assert_eq!(status.shifts, 100);
    assert_eq!(status.last_cause, Some(ShiftCause::Api));
    // The far entity sits within 4 km of the origin in f32: 1/4096 m resolution or better.
    let far_local = local(&runtime, "far");
    assert!(far_local.iter().all(|v| v.abs() <= 4096.0), "{far_local:?}");
    println!(
        "absolute drift after 100 shifts: f64-authored {worst_far:e} m, plain root {worst_plain:e} m, child {worst_child:e} m"
    );
    // Shifting back to the start brings the plain entities back exactly within f32 rounding.
    coordinates::rebase_now(runtime.app_mut().world_mut(), [0.0; 3]).unwrap();
    assert!(distance(abs(&runtime, "plain"), plain) <= 1.0);
    // The child keeps its parent-relative transform untouched.
    let child_entity = id(&runtime, "child");
    assert_eq!(
        runtime
            .app()
            .world()
            .get::<Transform>(child_entity)
            .unwrap()
            .position,
        [1.0, 2.0, 3.0]
    );
}

#[test]
fn shifting_toward_an_entity_preserves_its_absolute_position_exactly() {
    let fixture = Fixture::new(
        vec![entity("plain", [3000.125, -7.25, 1234.5], json!({}), &[])],
        vec![],
    );
    let mut runtime = fixture.runtime();
    tick(&mut runtime, 1, 0.1);
    let before = abs(&runtime, "plain");
    coordinates::rebase_now(runtime.app_mut().world_mut(), [2560.0, 0.0, 1024.0]).unwrap();
    assert_eq!(local(&runtime, "plain"), [440.125, -7.25, 210.5]);
    assert!(distance(abs(&runtime, "plain"), before) <= 1e-6);
    assert_eq!(abs(&runtime, "plain"), before);
}

#[test]
fn set_absolute_position_writes_f64_and_reads_back() {
    let fixture = Fixture::new(vec![entity("thing", [0.0; 3], json!({}), &[])], vec![]);
    let mut runtime = fixture.runtime();
    tick(&mut runtime, 1, 0.1);
    let target = [7_000_001.000_001, 2.0, -3_000_000.5];
    let thing = id(&runtime, "thing");
    let world = runtime.app_mut().world_mut();
    coordinates::rebase_now(world, [7_000_064.0, 0.0, -3_000_064.0]).unwrap();
    coordinates::set_absolute_position(world, thing, target).unwrap();
    assert_eq!(
        coordinates::absolute_position(world, thing).unwrap(),
        target
    );
    assert!(world.get::<AbsolutePosition>(thing).is_some());
    let expected = WorldOrigin {
        offset: [7_000_064.0, 0.0, -3_000_064.0],
    }
    .local(target);
    assert_eq!(world.get::<Transform>(thing).unwrap().position, expected);
}

// ---- automatic rebasing, events, telemetry ----------------------------------------------------

#[test]
fn camera_anchor_rebases_once_per_tick_on_the_snap_grid() {
    let fixture = Fixture::new(
        vec![
            entity(
                "settings",
                [0.0; 3],
                json!({}),
                &[large_world(100.0, 64, "")],
            ),
            entity("camera", [0.0, 2.0, 0.0], json!({"Camera3D": {}}), &[]),
            entity(
                "rock",
                [10.0, 0.0, 10.0],
                json!({"MeshRenderer": {"mesh": "mesh"}}),
                &[],
            ),
        ],
        vec![],
    );
    let mut runtime = fixture.runtime();
    let rock = abs(&runtime, "rock");
    let mut expected_camera = [0.0_f64, 2.0, 0.0];
    let mut shifts = 0;
    for _ in 0..12 {
        // The camera flies 150 m along +x every tick (in local space).
        let camera = id(&runtime, "camera");
        runtime
            .app_mut()
            .world_mut()
            .get_mut::<Transform>(camera)
            .unwrap()
            .position[0] += 150.0;
        expected_camera[0] += 150.0;
        let before = coordinates::status(runtime.app().world()).unwrap().shifts;
        tick(&mut runtime, 1, 0.1);
        let status = coordinates::status(runtime.app().world()).unwrap();
        assert!(status.shifts - before <= 1, "at most one shift per tick");
        shifts = status.shifts;
        let offset = status.offset;
        assert!(offset.iter().all(|v| v % 64.0 == 0.0), "{offset:?}");
        assert_eq!(abs(&runtime, "camera"), expected_camera);
        assert_eq!(abs(&runtime, "rock"), rock);
        assert!(local(&runtime, "camera")[0].abs() <= 100.0 + 150.0);
    }
    assert!(shifts >= 10, "{shifts} shifts");
    let snapshot = runtime.try_snapshot().unwrap();
    let telemetry = snapshot
        .world_origin
        .clone()
        .expect("world origin telemetry");
    assert_eq!(telemetry.shifts, shifts);
    assert_eq!(telemetry.settings.as_deref(), Some("settings"));
    assert_eq!(telemetry.anchor.as_deref(), Some("camera"));
    assert_eq!(telemetry.last_cause, Some(ShiftCause::Threshold));
    let bytes = serde_json::to_vec(&telemetry).unwrap().len();
    assert!(bytes < 4096, "{bytes} bytes");
    let event = snapshot
        .events
        .iter()
        .find(|e| e.name == EVENT_ORIGIN_SHIFTED)
        .expect("origin shift event delivered");
    let delta = event.payload["delta"].as_array().unwrap();
    assert_eq!(delta.len(), 3);
    assert!(delta[0].as_f64().unwrap() > 0.0, "{event:?}");
    assert_eq!(event.payload["cause"], "threshold");
}

#[test]
fn a_direct_origin_write_is_reconciled_by_one_shift() {
    let fixture = Fixture::new(
        vec![entity("rock", [110.25, 0.0, 10.25], json!({}), &[])],
        vec![],
    );
    let mut runtime = fixture.runtime();
    tick(&mut runtime, 1, 0.1);
    runtime.app_mut().world_mut().insert_resource(WorldOrigin {
        offset: [100.0, 0.0, 0.0],
    });
    tick(&mut runtime, 1, 0.1);
    assert_eq!(local(&runtime, "rock"), [10.25, 0.0, 10.25]);
    let status = coordinates::status(runtime.app().world()).unwrap();
    assert_eq!(
        (status.shifts, status.last_cause),
        (1, Some(ShiftCause::External))
    );
    tick(&mut runtime, 2, 0.1);
    assert_eq!(local(&runtime, "rock"), [10.25, 0.0, 10.25], "shifted once");
}

#[test]
fn ragdoll_or_deformable_physics_blocks_rebasing_with_a_reason() {
    let fixture = Fixture::new(
        vec![
            entity(
                "settings",
                [0.0; 3],
                json!({}),
                &[physics_settings(), large_world(64.0, 64, "")],
            ),
            entity("camera", [500.0, 2.0, 0.0], json!({"Camera3D": {}}), &[]),
            entity(
                "flag",
                [0.0, 2.0, 0.0],
                json!({}),
                &[(
                    "engine.Cloth",
                    json!({"size": [1.0, 1.0], "resolution": [4, 4]}),
                )],
            ),
        ],
        vec![],
    );
    let mut runtime = fixture.runtime();
    tick(&mut runtime, 2, STEP);
    let status = coordinates::status(runtime.app().world()).unwrap();
    assert_eq!(status.shifts, 0);
    assert!(
        status.blocked.as_deref().unwrap().contains("flag"),
        "{status:?}"
    );
    let error = coordinates::rebase_now(runtime.app_mut().world_mut(), [512.0, 0.0, 0.0])
        .unwrap_err()
        .to_string();
    assert!(error.starts_with("world.origin.unsupported"), "{error}");
    assert_eq!(
        local(&runtime, "camera"),
        [500.0, 2.0, 0.0],
        "nothing moved"
    );
}

#[test]
fn scene_validation_rejects_bad_large_world_settings() {
    let rejected = |entities: Vec<Value>| {
        let fixture = Fixture::new(entities, vec![]);
        fixture
            .project()
            .and_then(Runtime::new)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default()
    };
    let snap_too_big = rejected(vec![entity(
        "s",
        [0.0; 3],
        json!({}),
        &[large_world(100.0, 128, "")],
    )]);
    assert!(
        snap_too_big.contains("snap must not exceed threshold"),
        "{snap_too_big}"
    );
    let two = rejected(vec![
        entity("a", [0.0; 3], json!({}), &[large_world(100.0, 64, "")]),
        entity("b", [0.0; 3], json!({}), &[large_world(100.0, 64, "")]),
    ]);
    assert!(two.contains("at most one"), "{two}");
    let child = rejected(vec![
        entity("p", [0.0; 3], json!({}), &[]),
        json!({"id":"c","name":"c","parent":"p","components":{"Transform":{"position":[0,0,0]},
            "custom":{"engine.AbsolutePosition":{"version":1,"data":{"position":[1.0,2.0,3.0]}}}}}),
    ]);
    assert!(child.contains("only valid on root entities"), "{child}");
}

// ---- physics -----------------------------------------------------------------------------------

/// A ball thrown from `start` (absolute) for three seconds; absolute positions and velocities.
fn flight(start: [f64; 3], rebasing: Option<(f64, u32)>) -> (Vec<[f64; 3]>, Vec<Vec3>, u64) {
    let mut settings = vec![physics_settings()];
    if let Some((threshold, snap)) = rebasing {
        settings.push(large_world(threshold, snap, "ball"));
    }
    let fixture = Fixture::new(
        vec![
            entity("settings", [0.0; 3], json!({}), &settings),
            entity(
                "ball",
                [0.0; 3],
                json!({}),
                &[
                    absolute(start),
                    (
                        "engine.PhysicsBody3D",
                        json!({"shape": "ball", "radius": 0.5, "velocity": [3.3, 4.0, 1.7],
                            "angular_velocity": [0.0, 1.0, 0.0]}),
                    ),
                ],
            ),
        ],
        vec![],
    );
    let mut runtime = fixture.runtime();
    let (mut positions, mut velocities) = (Vec::new(), Vec::new());
    for _ in 0..180 {
        tick(&mut runtime, 1, STEP);
        positions.push(abs(&runtime, "ball"));
        velocities.push(state(&runtime, "ball").linear_velocity);
    }
    let shifts = coordinates::status(runtime.app().world()).unwrap().shifts;
    (positions, velocities, shifts)
}

#[test]
fn a_body_at_50_km_with_rebasing_matches_the_same_flight_near_the_origin() {
    let near = [1.25, 10.0, -2.5];
    let base = [50_176.0, 0.0, 50_176.0];
    let far: [f64; 3] = std::array::from_fn(|i| near[i] + base[i]);
    let (reference, reference_velocity, _) = flight(near, None);
    let (rebased, rebased_velocity, shifts) = flight(far, Some((4.0, 4)));
    let (control, _, control_shifts) = flight(far, None);
    let error = |run: &[[f64; 3]]| {
        run.iter()
            .zip(&reference)
            .map(|(p, r)| distance(*p, std::array::from_fn(|i| r[i] + base[i])))
            .fold(0.0, f64::max)
    };
    let rebased_error = error(&rebased);
    let control_error = error(&control);
    let velocity_error = rebased_velocity
        .iter()
        .zip(&reference_velocity)
        .map(|(a, b)| (*a - *b).abs().max_element())
        .fold(0.0, f32::max);
    println!(
        "50 km flight over 3 s: rebased error {rebased_error:e} m ({shifts} shifts), \
         velocity error {velocity_error:e} m/s; without rebasing {control_error:e} m"
    );
    assert!(shifts >= 3, "{shifts} shifts");
    assert_eq!(control_shifts, 0);
    assert!(rebased_error <= 1e-3, "rebased error {rebased_error} m");
    assert!(
        velocity_error <= 1e-4,
        "velocity error {velocity_error} m/s"
    );
    // The control shows why: f32 positions at 50 km have 3.9 mm steps.
    assert!(
        control_error >= 1e-2,
        "control error only {control_error} m"
    );
}

#[test]
fn a_shift_keeps_body_velocities_and_moves_poses_by_exactly_the_delta() {
    let fixture = Fixture::new(
        vec![
            entity("settings", [0.0; 3], json!({}), &[physics_settings()]),
            entity(
                "ball",
                [5.0, 20.0, -3.0],
                json!({}),
                &[(
                    "engine.PhysicsBody3D",
                    json!({"shape": "ball", "velocity": [7.0, 2.0, -1.0], "angular_velocity": [0.5, 1.5, -2.0]}),
                )],
            ),
            entity(
                "lift",
                [-4.0, 1.0, 2.0],
                json!({}),
                &[("engine.PhysicsBody3D", json!({"motion": "kinematic"}))],
            ),
        ],
        vec![],
    );
    let mut runtime = fixture.runtime();
    tick(&mut runtime, 30, STEP);
    let before = state(&runtime, "ball");
    let absolute_before = abs(&runtime, "ball");
    let lift_before = abs(&runtime, "lift");
    // Shift toward the ball, on a 1 m grid.
    let offset = absolute_before.map(f64::round);
    coordinates::rebase_now(runtime.app_mut().world_mut(), offset).unwrap();
    let after = state(&runtime, "ball");
    assert_eq!(after.linear_velocity, before.linear_velocity);
    assert_eq!(after.angular_velocity, before.angular_velocity);
    assert_eq!(after.rotation, before.rotation);
    let moved: [f64; 3] = std::array::from_fn(|i| f64::from(before.position[i]) - offset[i]);
    assert_eq!(after.position.to_array().map(f64::from), moved);
    assert_eq!(abs(&runtime, "ball"), absolute_before);
    tick(&mut runtime, 1, STEP);
    // The kinematic body was teleported, not swept: it stands still at the same place.
    let lift = state(&runtime, "lift");
    assert!(lift.linear_velocity.length() <= 1e-6, "{lift:?}");
    assert!(distance(abs(&runtime, "lift"), lift_before) <= 1e-4);
}

#[test]
fn a_resting_box_keeps_its_contact_through_shifts_at_50_km() {
    let base = [50_000.0, 0.0, 50_000.0];
    let at = |x: f64, y: f64, z: f64| [base[0] + x, base[1] + y, base[2] + z];
    let fixture = Fixture::new(
        vec![
            entity(
                "settings",
                [0.0; 3],
                json!({}),
                &[physics_settings(), large_world(64.0, 64, "box")],
            ),
            entity(
                "ground",
                [0.0; 3],
                json!({}),
                &[
                    absolute(at(0.0, 0.0, 0.0)),
                    (
                        "engine.PhysicsBody3D",
                        json!({"motion": "fixed", "half_extents": [20.0, 0.5, 20.0]}),
                    ),
                ],
            ),
            entity(
                "box",
                [0.0; 3],
                json!({}),
                &[
                    absolute(at(1.0, 1.0, 2.0)),
                    ("engine.PhysicsBody3D", json!({})),
                ],
            ),
        ],
        vec![],
    );
    let mut runtime = fixture.runtime();
    tick(&mut runtime, 120, STEP);
    let rest = abs(&runtime, "box");
    assert!(
        (rest[1] - 1.0).abs() <= 0.02,
        "resting on the ground: {rest:?}"
    );
    // Push the origin 2 km away; the anchor rule brings it back next tick (two shifts).
    let offset = coordinates::origin(runtime.app().world());
    coordinates::rebase_now(
        runtime.app_mut().world_mut(),
        [offset[0] + 2048.0, 0.0, offset[2] - 2048.0],
    )
    .unwrap();
    let mut worst: f64 = 0.0;
    let mut fastest: f32 = 0.0;
    for _ in 0..60 {
        tick(&mut runtime, 1, STEP);
        worst = worst.max(distance(abs(&runtime, "box"), rest));
        fastest = fastest.max(state(&runtime, "box").linear_velocity.length());
    }
    let status = coordinates::status(runtime.app().world()).unwrap();
    println!(
        "resting box through {} shifts: moved {worst:e} m, peak speed {fastest:e} m/s",
        status.shifts
    );
    assert!(status.shifts >= 3, "{status:?}");
    assert!(worst <= 1e-3, "box moved {worst} m");
    assert!(fastest <= 0.05, "box bounced at {fastest} m/s");
}

// ---- navigation ---------------------------------------------------------------------------------

#[test]
fn a_navigation_path_resolves_and_is_followed_across_a_shift() {
    let document = NavMeshDocument {
        version: 1,
        input: BakeInput {
            vertices: vec![[0., 0., 0.], [10., 0., 0.], [10., 0., 10.], [0., 0., 10.]],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
            obstacles: vec![Obstacle {
                min: [4., -1., 3.],
                max: [6., 3., 7.],
            }],
            settings: BakeSettings::default(),
        },
    };
    let fixture = Fixture::new(
        vec![entity(
            "agent",
            [2.0, 0.0, 5.0],
            json!({}),
            &[(
                "engine.NavigationAgent",
                json!({"navmesh": "nav", "target": [8.0, 0.0, 5.0], "speed": 4.0}),
            )],
        )],
        vec![json!({"id": "nav", "kind": "navigation", "path": "navigation.json"})],
    );
    fs::write(
        fixture.dir.path().join("navigation.json"),
        serde_json::to_vec(&document).unwrap(),
    )
    .unwrap();
    let mut runtime = fixture.runtime();
    let outside = |p: [f64; 3]| {
        p[0] <= 3.75 + 1e-4 || p[0] >= 6.25 - 1e-4 || p[2] <= 2.75 + 1e-4 || p[2] >= 7.25 - 1e-4
    };
    let mut names = Vec::new();
    let mut previous = abs(&runtime, "agent");
    for n in 0..40 {
        if n == 3 {
            coordinates::rebase_now(runtime.app_mut().world_mut(), [512.0, 0.0, -1024.0]).unwrap();
            // The agent moves 512 m further from the origin: f32 rounding there is 0.03 mm.
            let shifted = abs(&runtime, "agent");
            assert!(
                distance(shifted, previous) <= 1e-4,
                "{previous:?} -> {shifted:?}"
            );
        }
        tick(&mut runtime, 1, 0.1);
        let p = abs(&runtime, "agent");
        assert!(outside(p), "inside the obstacle at {p:?}");
        assert!(
            distance(p, previous) <= 0.4 + 1e-4,
            "jumped from {previous:?} to {p:?}"
        );
        previous = p;
        let snapshot = runtime.try_snapshot().unwrap();
        names.extend(
            snapshot
                .events
                .iter()
                .filter(|e| e.name.starts_with("navigation."))
                .map(|e| e.name.clone()),
        );
    }
    assert!(distance(previous, [8.0, 0.0, 5.0]) <= 1e-3, "{previous:?}");
    // A repath after the shift queries the absolute navmesh from the shifted local position.
    let agent = id(&runtime, "agent");
    {
        let world = runtime.app_mut().world_mut();
        let mut config = world.get_mut::<NavigationAgent>(agent).unwrap();
        config.target = Some([2.0, 0.0, 5.0]);
        config.repath += 1;
    }
    for _ in 0..40 {
        tick(&mut runtime, 1, 0.1);
        let p = abs(&runtime, "agent");
        assert!(outside(p), "inside the obstacle at {p:?}");
        let snapshot = runtime.try_snapshot().unwrap();
        names.extend(
            snapshot
                .events
                .iter()
                .filter(|e| e.name.starts_with("navigation."))
                .map(|e| e.name.clone()),
        );
    }
    assert!(distance(abs(&runtime, "agent"), [2.0, 0.0, 5.0]) <= 1e-3);
    names.dedup();
    assert_eq!(
        names,
        [
            "navigation.repathed",
            "navigation.arrived",
            "navigation.repathed",
            "navigation.arrived"
        ]
    );
    assert_eq!(
        coordinates::origin(runtime.app().world()),
        [512.0, 0.0, -1024.0]
    );
}

// ---- world features ------------------------------------------------------------------------------

fn noise_graph() -> String {
    use engine_pcg::{GraphBuilder, Registry};
    let registry = Registry::builtin();
    GraphBuilder::new(&registry, 4)
        .node("grid", "grid_sampler", [("spacing", json!(1.0))])
        .node("n", "noise_field", [("frequency", json!(0.21))])
        .node("keep", "density_filter", [("threshold", json!(0.55))])
        .node(
            "spawn",
            "spawner",
            [("entries", json!([{"asset": "mesh", "kind": "mesh"}]))],
        )
        .connect("grid.points", "keep.points")
        .connect("n.field", "keep.field")
        .connect("keep.points", "spawn.points")
        .build()
        .unwrap()
        .to_json()
}

/// Absolute positions of every vertex drawn by meshes whose id starts with `prefix`, rounded
/// to millimetres and sorted.
fn drawn(runtime: &Runtime, prefix: &str) -> Vec<[i64; 3]> {
    let frame = runtime.extract_scene_frame().unwrap();
    let offset = coordinates::origin(runtime.app().world());
    let mut out = Vec::new();
    for mesh in frame.meshes.iter().filter(|m| m.id.starts_with(prefix)) {
        let model = Mat4::from_cols_array_2d(&mesh.model);
        for vertex in &mesh.mesh.vertices {
            let p = model.transform_point3(Vec3::from(vertex.position));
            out.push(std::array::from_fn(|i| {
                ((f64::from(p[i]) + offset[i]) * 1000.0).round() as i64
            }));
        }
    }
    out.sort_unstable();
    out
}

/// Largest distance, in millimetres, from a point of `a` to its nearest unused point of `b`
/// (both multisets of the same length).
fn mismatch(a: &[[i64; 3]], b: &[[i64; 3]]) -> i64 {
    assert_eq!(a.len(), b.len(), "point counts differ");
    let mut used = vec![false; b.len()];
    let mut worst = 0;
    for p in a {
        let (index, d) = b
            .iter()
            .enumerate()
            .filter(|(i, _)| !used[*i])
            .map(|(i, q)| (i, (0..3).map(|k| (p[k] - q[k]).abs()).max().unwrap()))
            .min_by_key(|(_, d)| *d)
            .unwrap();
        used[index] = true;
        worst = worst.max(d);
    }
    worst
}

fn generated_pcg(runtime: &Runtime) -> Vec<[i64; 3]> {
    let mut out = Vec::new();
    for n in 0..100_000 {
        let Some(entity) = runtime.entity(&format!("pcg:forest:{n}")) else {
            break;
        };
        let p = coordinates::absolute_position(runtime.app().world(), entity).unwrap();
        out.push(p.map(|v| (v * 1000.0).round() as i64));
    }
    out.sort_unstable();
    out
}

#[test]
fn terrain_foliage_pcg_and_bodies_keep_their_absolute_places_and_are_not_shifted_twice() {
    use engine_foliage::FoliageType;
    use engine_terrain::{Heightfield, asset};
    let dir_assets = vec![
        json!({"id": "land", "kind": "terrain", "path": "land.terrain"}),
        json!({"id": "noise", "kind": "pcg", "path": "noise.pcg.json"}),
    ];
    let mut grass = FoliageType::new("grass", "mesh");
    grass.density = 0.5;
    let fixture = Fixture::new(
        vec![
            entity("settings", [0.0; 3], json!({}), &[physics_settings()]),
            entity("camera", [16.0, 8.0, 16.0], json!({"Camera3D": {}}), &[]),
            entity(
                "land",
                [0.0; 3],
                json!({}),
                &[(
                    "engine.Terrain",
                    json!({"asset": "land", "lod": {"node_resolution": 16}, "render_mode": "cpu"}),
                )],
            ),
            entity(
                "plants",
                [0.0; 3],
                json!({}),
                &[(
                    "engine.Foliage",
                    json!({"types": [grass], "terrain": "land", "seed": 5, "center": [16.0, 16.0, 16.0],
                        "size": [32.0, 64.0, 32.0], "render_mode": "cpu", "culling": {"max_distance": 200.0}}),
                )],
            ),
            entity(
                "forest",
                [16.0, 0.0, 16.0],
                json!({}),
                &[(
                    "engine.PcgVolume",
                    json!({"graph": "noise", "seed": 7, "size": [16, 64, 16], "surface": "terrain"}),
                )],
            ),
            entity(
                "crate",
                [8.0, 3.0, 8.0],
                json!({}),
                &[("engine.PhysicsBody3D", json!({}))],
            ),
        ],
        dir_assets,
    );
    let heights = vec![2.0; 33 * 33];
    let field = Heightfield::from_heights(33, 33, 1.0, heights).unwrap();
    fs::write(
        fixture.dir.path().join("land.terrain"),
        asset::encode(&field),
    )
    .unwrap();
    fs::write(fixture.dir.path().join("noise.pcg.json"), noise_graph()).unwrap();
    let mut runtime = fixture.runtime();
    tick(&mut runtime, 90, STEP);
    let plants = id(&runtime, "plants");
    let foliage_before = foliage::status(runtime.app().world(), plants).unwrap();
    let instances = |runtime: &mut Runtime| {
        let offset = coordinates::origin(runtime.app().world());
        let field = foliage::field(runtime.app_mut().world_mut(), plants).unwrap();
        let mut out: Vec<[i64; 3]> = field
            .iter()
            .map(|i| {
                std::array::from_fn(|a| {
                    ((f64::from(i.position[a]) + offset[a]) * 1000.0).round() as i64
                })
            })
            .collect();
        out.sort_unstable();
        out
    };
    let foliage_instances = instances(&mut runtime);
    let foliage_drawn = drawn(&runtime, "foliage:");
    let terrain_drawn = drawn(&runtime, "terrain:");
    let pcg = generated_pcg(&runtime);
    let crate_rest = abs(&runtime, "crate");
    assert!(
        !foliage_instances.is_empty() && !foliage_drawn.is_empty(),
        "{} instances, {} vertices: {foliage_before:?}",
        foliage_instances.len(),
        foliage_drawn.len()
    );
    assert!(!terrain_drawn.is_empty(), "terrain chunks drawn");
    assert!(!pcg.is_empty());
    assert!(
        (crate_rest[1] - 2.5).abs() <= 0.02,
        "crate rests on the terrain: {crate_rest:?}"
    );
    let terrain_entity = id(&runtime, "land");
    let height_before =
        terrain::height_at_world(runtime.app_mut().world_mut(), terrain_entity, 8.0, 8.0).unwrap();

    coordinates::rebase_now(runtime.app_mut().world_mut(), [512.0, 0.0, -1024.0]).unwrap();
    tick(&mut runtime, 30, STEP);

    let foliage_after = foliage::status(runtime.app().world(), plants).unwrap();
    assert_eq!(
        foliage_after.attempts, foliage_before.attempts,
        "translated, not regenerated"
    );
    // Millimetre-rounded absolute positions. Foliage instances are translated (not
    // regenerated), drawn vertices rebuilt and the PCG output regenerated on the re-registered
    // terrain surface (noise sampled at absolute coordinates): they differ by f32 rounding at
    // 1 km (0.06 mm), which can flip the last millimetre digit.
    let instance_error = mismatch(&instances(&mut runtime), &foliage_instances);
    let foliage_error = mismatch(&drawn(&runtime, "foliage:"), &foliage_drawn);
    let terrain_error = mismatch(&drawn(&runtime, "terrain:"), &terrain_drawn);
    let pcg_error = mismatch(&generated_pcg(&runtime), &pcg);
    println!(
        "after a shift: {} foliage instances within {instance_error} mm, foliage vertices within {foliage_error} mm, terrain vertices within {terrain_error} mm, {} PCG instances within {pcg_error} mm",
        foliage_instances.len(),
        pcg.len()
    );
    assert!(instance_error <= 1 && foliage_error <= 1 && terrain_error <= 1 && pcg_error <= 1);
    let crate_after = abs(&runtime, "crate");
    assert!(
        distance(crate_after, crate_rest) <= 1e-3,
        "{crate_rest:?} -> {crate_after:?}"
    );
    // Terrain queries in the new local frame see the same ground.
    let height_after = terrain::height_at_world(
        runtime.app_mut().world_mut(),
        terrain_entity,
        8.0 - 512.0,
        8.0 + 1024.0,
    )
    .unwrap();
    assert_eq!(height_after, height_before);
    let reports = runtime.app().world().resource::<PcgStatus>();
    assert!(reports.0["forest"].diagnostics.is_empty());
}

// ---- rendering --------------------------------------------------------------------------------------

/// The scene's camera-relative matrices: `inverse(camera world) * model` per mesh id.
fn view_space(base: [f64; 3], rebasing: bool) -> Vec<(String, [[f32; 4]; 4])> {
    let at = |p: [f64; 3]| absolute(std::array::from_fn(|i| base[i] + p[i]));
    let mut settings_entries = vec![];
    if rebasing {
        settings_entries.push(large_world(2048.0, 512, ""));
    }
    let mut entities = vec![
        json!({"id":"camera","name":"camera","components":{
            "Transform":{"position":[0,0,0],"rotation":[0.0,0.38268343,0.0,0.9238795]},
            "Camera3D":{},
            "custom": custom(&[at([1.0, 2.0, 3.0])])}}),
        json!({"id":"rock","name":"rock","components":{
            "Transform":{"position":[0,0,0],"rotation":[0.0,0.0,0.70710677,0.70710677],"scale":[2.0,2.0,2.0]},
            "MeshRenderer":{"mesh":"mesh"},
            "custom": custom(&[at([5.0, 0.0, -10.0])])}}),
        json!({"id":"pebble","name":"pebble","parent":"rock","components":{
            "Transform":{"position":[1.0,0.0,0.0]},"MeshRenderer":{"mesh":"mesh"}}}),
        json!({"id":"hill","name":"hill","components":{
            "Transform":{"position":[0,0,0]},"MeshRenderer":{"mesh":"mesh"},
            "custom": custom(&[at([-300.0, 40.0, -900.0])])}}),
    ];
    if !settings_entries.is_empty() {
        entities.push(entity("settings", [0.0; 3], json!({}), &settings_entries));
    }
    let fixture = Fixture::new(entities, vec![]);
    let mut runtime = fixture.runtime();
    tick(&mut runtime, 1, 0.1);
    let frame = runtime.extract_scene_frame().unwrap();
    let camera = frame.camera3d.expect("camera");
    let view = Mat4::from_cols_array_2d(&camera.world).inverse();
    let mut out: Vec<_> = frame
        .meshes
        .iter()
        .map(|m| {
            (
                m.id.clone(),
                (view * Mat4::from_cols_array_2d(&m.model)).to_cols_array_2d(),
            )
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn largest_difference(a: &[(String, [[f32; 4]; 4])], b: &[(String, [[f32; 4]; 4])]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|((ia, ma), (ib, mb))| {
            assert_eq!(ia, ib);
            ma.as_flattened()
                .iter()
                .zip(mb.as_flattened())
                .map(|(x, y)| (x - y).abs())
                .fold(0.0, f32::max)
        })
        .fold(0.0, f32::max)
}

#[test]
fn camera_relative_matrices_far_from_the_origin_match_the_scene_at_the_origin() {
    let reference = view_space([0.0; 3], false);
    assert_eq!(reference.len(), 3);
    for base in [
        [1_000_000.37, 0.0, -1_000_000.81],
        [9_999_999.5, 120.25, 10_000_000.123],
    ] {
        let rebased = largest_difference(&view_space(base, true), &reference);
        let control = largest_difference(&view_space(base, false), &reference);
        println!(
            "view-space matrices at {base:?}: rebased {rebased:e}, without rebasing {control:e}"
        );
        assert!(rebased <= 1e-4, "rebased difference {rebased} at {base:?}");
        assert!(
            control >= 1e-2,
            "control difference only {control} at {base:?}"
        );
    }
}
