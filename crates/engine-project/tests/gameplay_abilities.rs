use bevy_ecs::prelude::*;
use engine_project::Result;
use engine_project::gameplay::{
    self, AbilityBehaviour, BehaviourContext, BehaviourStatus, FailReason,
    register_ability_behaviour,
};
use engine_project::*;
use serde_json::{Value, json};
use std::fs;

fn definitions() -> Value {
    json!({"version":1,
        "attribute_sets":[{"name":"Vitals","attributes":[
            {"name":"MaxHealth","base":100},
            {"name":"Health","base":100,"min":0,"max":"MaxHealth"},
            {"name":"Mana","base":100,"min":0}]}],
        "effects":[
            {"name":"Damage","modifiers":[{"attribute":"Health","magnitude":{"scalable":{"value":-25}}}]},
            {"name":"ManaCost","modifiers":[{"attribute":"Mana","magnitude":{"scalable":{"value":-10}}}]},
            {"name":"StrikeCooldown","policy":"duration","duration":1.0,"granted_tags":["Cooldown.Strike"]},
            {"name":"Armor","policy":"infinite","granted_tags":["State.Armored"],
                "modifiers":[{"attribute":"MaxHealth","magnitude":{"scalable":{"value":50}}}]}],
        "abilities":[
            {"name":"Strike","cost":"ManaCost","cooldown":"StrikeCooldown","tags":["Ability.Attack"],
                "steps":[{"wait":{"seconds":0.3}},{"apply_effect":{"effect":"Damage","to":"target"}},
                    {"emit_event":{"event":"strike.hit","payload":{"by":"attacker"}}}]},
            {"name":"Boom","native":"boom"},
            {"name":"Flinch","triggers":["hit"],"steps":[{"emit_event":{"event":"flinched"}}]}]})
}

/// sequence [activate Strike on `enemy`, wait for it to end, wait 0.8 s]
fn duel() -> Value {
    json!({"version":1,"kind":"behavior_tree",
        "nodes":[
            {"id":"root","node_type":"root"},
            {"id":"seq","node_type":"sequence","params":{}},
            {"id":"go","node_type":"native_task","params":{"name":"engine.activate_ability",
                "params":{"ability":"Strike","target_key":"enemy"}}},
            {"id":"end","node_type":"native_task","params":{"name":"engine.wait_ability_end",
                "params":{"ability":"Strike"}}},
            {"id":"rest","node_type":"wait","params":{"seconds":0.8}}],
        "edges":[
            {"from":{"node":"root","port":"child"},"to":{"node":"seq","port":"parent"}},
            {"from":{"node":"seq","port":"children"},"to":{"node":"go","port":"parent"}},
            {"from":{"node":"seq","port":"children"},"to":{"node":"end","port":"parent"}},
            {"from":{"node":"seq","port":"children"},"to":{"node":"rest","port":"parent"}}],
        "variables":[{"name":"enemy","type":"entity"}]})
}

const EVENTS: [&str; 11] = [
    "ability.activated",
    "ability.committed",
    "ability.ended",
    "ability.cancelled",
    "ability.failed",
    "effect.applied",
    "effect.removed",
    "attribute.changed",
    "strike.hit",
    "flinched",
    "hit",
];

fn entity(id: &str, custom: Value) -> Value {
    let bindings: Vec<Value> = EVENTS
        .iter()
        .map(|e| json!({"event":e,"handler":"record"}))
        .collect();
    json!({"id":id,"name":id,"components":{
        "Transform":{"position":[0,0,0]},
        "EventBindings":{"bindings":bindings},
        "custom":custom}})
}

fn system(abilities: &[&str], effects: &[&str]) -> Value {
    json!({"version":1,"data":{"definitions":"abilities","attribute_sets":["Vitals"],
        "abilities":abilities.iter().map(|a| json!({"ability":a})).collect::<Vec<_>>(),
        "effects":effects.iter().map(|e| json!({"effect":e})).collect::<Vec<_>>()}})
}

fn project(entities: Vec<Value>, definitions: Value) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("abilities.gameplay.json"),
        serde_json::to_vec_pretty(&definitions).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("duel.graph.json"),
        serde_json::to_vec_pretty(&duel()).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        json!({"schema":1,"name":"gas-test","startup_scene":"scene.json","assets":[
            {"id":"abilities","kind":"gameplay","path":"abilities.gameplay.json"},
            {"id":"duel","kind":"graph","path":"duel.graph.json"}]})
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.path().join("scene.json"),
        json!({"schema":1,"scene_id":"main","entities":entities}).to_string(),
    )
    .unwrap();
    dir
}

fn duel_project() -> tempfile::TempDir {
    project(
        vec![
            entity(
                "attacker",
                json!({
                    "engine.AbilitySystem": system(&["Strike"], &[]),
                    "engine.BehaviorTreeAgent": {"version":1,"data":{"graph":"duel","blackboard":{"enemy":"dummy"}}}
                }),
            ),
            entity(
                "dummy",
                json!({"engine.AbilitySystem": system(&["Flinch"], &["Armor"])}),
            ),
        ],
        definitions(),
    )
}

/// (tick the event was delivered at, entity, name, payload)
type Entry = (usize, String, String, Value);

#[derive(Resource, Default)]
struct Log(Vec<Entry>, usize);

fn record(world: &mut World, entity: Entity, event: &EngineEvent) -> Result<()> {
    let id = world
        .get::<runtime::AuthoredId>(entity)
        .map(|a| a.id.clone())
        .unwrap_or_default();
    let mut log = world.resource_mut::<Log>();
    let tick = log.1;
    log.0
        .push((tick, id, event.name.clone(), event.payload.clone()));
    Ok(())
}

fn start(dir: &tempfile::TempDir) -> Runtime {
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Log::default());
    register_event_handler(runtime.app_mut(), "record", record).unwrap();
    runtime.start().unwrap();
    runtime
}

fn tick(runtime: &mut Runtime, n: usize) {
    for _ in 0..n {
        runtime.app_mut().world_mut().resource_mut::<Log>().1 += 1;
        runtime.tick(0.1, InputSnapshot::default()).unwrap();
    }
}

fn log(runtime: &Runtime) -> Vec<Entry> {
    runtime.app().world().resource::<Log>().0.clone()
}

fn duel_run() -> (Vec<Entry>, Value, f32) {
    let dir = duel_project();
    let mut runtime = start(&dir);
    tick(&mut runtime, 30);
    let dummy = runtime.entity("dummy").unwrap();
    let health = gameplay::attribute(runtime.app().world(), dummy, "Health")
        .unwrap()
        .current;
    let telemetry = serde_json::to_value(runtime.try_snapshot().unwrap().abilities).unwrap();
    (log(&runtime), telemetry, health)
}

#[test]
fn a_behavior_tree_agent_strikes_a_target_through_the_ability_system() {
    let (events, telemetry, health) = duel_run();
    let attacker: Vec<(usize, &str)> = events
        .iter()
        .filter(|e| e.1 == "attacker")
        .map(|e| (e.0, e.2.as_str()))
        .collect();
    let names: Vec<&str> = attacker.iter().map(|e| e.1).collect();
    assert_eq!(
        names[..9],
        [
            "ability.activated",
            "effect.applied",
            "attribute.changed",
            "effect.applied",
            "ability.committed",
            "strike.hit",
            "ability.ended",
            "effect.removed",
            "ability.activated",
        ],
        "{names:?}"
    );
    // Activation and commit in one tick, the hit 0.3 s later, the cooldown gone exactly
    // 1.0 s (10 ticks) after it was applied.
    assert_eq!(attacker[4].0, attacker[0].0);
    assert_eq!(attacker[5].0 - attacker[0].0, 3);
    assert_eq!(attacker[7].0 - attacker[3].0, 10);
    let cooldown_removed = &events
        .iter()
        .find(|e| e.1 == "attacker" && e.2 == "effect.removed")
        .unwrap()
        .3;
    assert_eq!(
        cooldown_removed["effect"], "StrikeCooldown",
        "{cooldown_removed}"
    );
    assert_eq!(cooldown_removed["reason"], "expired");
    assert!(
        !names.contains(&"ability.failed"),
        "the tree waits out the cooldown: {names:?}"
    );

    // The target lost 25 health per hit.
    let hits = names.iter().filter(|n| **n == "strike.hit").count();
    assert!(hits >= 2, "{hits}");
    let delivered_hits = events
        .iter()
        .filter(|e| e.1 == "dummy" && e.2 == "attribute.changed" && e.3["attribute"] == "Health")
        .count();
    assert_eq!(delivered_hits, hits);
    assert_eq!(health, 100.0 - 25.0 * hits as f32);
    let first_damage = events
        .iter()
        .find(|e| e.1 == "dummy" && e.2 == "attribute.changed" && e.3["attribute"] == "Health")
        .unwrap();
    assert_eq!(
        first_damage.3,
        json!({"attribute":"Health","old":100.0,"new":75.0,"base":75.0})
    );
    let applied = events
        .iter()
        .find(|e| e.1 == "dummy" && e.2 == "effect.applied" && e.3["effect"] == "Damage")
        .unwrap();
    assert_eq!(applied.3["source"], "attacker");

    // Telemetry.
    assert_eq!(telemetry["definitions"], "abilities");
    assert_eq!(telemetry["actors"], 2);
    let listed = telemetry["listed"].as_array().unwrap();
    assert_eq!(listed[0]["entity"], "attacker");
    assert_eq!(listed[1]["entity"], "dummy");
    assert_eq!(listed[1]["tags"], json!(["State.Armored"]));
    assert_eq!(listed[1]["effects"][0]["effect"], "Armor");
    assert_eq!(
        listed[1]["attributes"][0],
        json!({"name":"MaxHealth","base":100.0,"current":150.0})
    );

    // Deterministic.
    let again = duel_run();
    assert_eq!(events, again.0);
    assert_eq!(telemetry, again.1);
}

struct Boom;
impl AbilityBehaviour<World> for Boom {
    fn activate(&mut self, cx: &mut BehaviourContext<'_, World>) -> BehaviourStatus {
        let target = cx.target.unwrap();
        let _ = cx.system.apply_effect(cx.actor, target, "Damage", 1, None);
        cx.host.resource_mut::<Log>().0.push((
            0,
            "native".into(),
            "boom".into(),
            cx.params.clone(),
        ));
        BehaviourStatus::Ended
    }
    fn tick(&mut self, _: &mut BehaviourContext<'_, World>) -> BehaviourStatus {
        BehaviourStatus::Ended
    }
}
fn boom(_: &mut World, _: u64) -> std::result::Result<Box<dyn AbilityBehaviour<World>>, String> {
    Ok(Box::new(Boom))
}

#[test]
fn native_api_behaviours_tags_and_triggers() {
    let dir = project(
        vec![
            entity("a", json!({"engine.AbilitySystem": system(&["Boom"], &[])})),
            entity(
                "b",
                json!({"engine.AbilitySystem": system(&["Flinch"], &[])}),
            ),
        ],
        definitions(),
    );
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Log::default());
    register_event_handler(runtime.app_mut(), "record", record).unwrap();
    register_ability_behaviour(runtime.app_mut(), "boom", boom).unwrap();
    assert!(register_ability_behaviour(runtime.app_mut(), "boom", boom).is_err());
    runtime.start().unwrap();
    let (a, b) = (runtime.entity("a").unwrap(), runtime.entity("b").unwrap());
    let world = runtime.app_mut().world_mut();
    let id = gameplay::try_activate_ability(world, a, "Boom", Some(b))
        .unwrap()
        .unwrap();
    assert_eq!(
        gameplay::instance_outcome(world, id),
        Some(gameplay::Outcome::Ended)
    );
    assert_eq!(
        gameplay::attribute(world, b, "Health").unwrap().current,
        75.0
    );
    assert_eq!(
        gameplay::try_activate_ability(world, a, "Flinch", None).unwrap(),
        Err(FailReason::NotGranted)
    );
    gameplay::add_tag(world, b, "State.Stunned.Hard").unwrap();
    assert!(gameplay::has_tag(world, b, "State.Stunned", false));
    assert!(!gameplay::has_tag(world, b, "State.Stunned", true));
    assert!(
        gameplay::apply_effect(world, a, b, "Damage", 1)
            .unwrap()
            .is_ok()
    );
    assert_eq!(gameplay::attribute(world, b, "Health").unwrap().base, 50.0);
    // A gameplay event triggers Flinch on b.
    assert_eq!(
        gameplay::send_gameplay_event(world, b, "hit", Some(a)).unwrap(),
        1
    );
    tick(&mut runtime, 2);
    let names: Vec<String> = log(&runtime).into_iter().map(|e| e.2).collect();
    assert!(names.contains(&"boom".to_owned()));
    assert!(names.contains(&"flinched".to_owned()), "{names:?}");
    assert!(names.contains(&"ability.failed".to_owned()));
}

#[test]
fn invalid_ability_systems_fail_project_validation() {
    let load = |entities: Vec<Value>, defs: Value| {
        let dir = project(entities, defs);
        Project::load(dir.path().join("project.json"))
            .err()
            .map(|e| e.diagnostics[0].code.clone())
    };
    assert_eq!(
        load(
            vec![entity(
                "a",
                json!({"engine.AbilitySystem": system(&["Missing"], &[])})
            )],
            definitions()
        )
        .as_deref(),
        Some("gameplay.component.invalid")
    );
    assert_eq!(
        load(
            vec![entity(
                "a",
                json!({"engine.AbilitySystem": system(&[], &["Missing"])})
            )],
            definitions()
        )
        .as_deref(),
        Some("gameplay.component.invalid")
    );
    assert_eq!(
        load(
            vec![],
            json!({"version":1,"effects":[{"name":"E","policy":"duration"}]})
        )
        .as_deref(),
        Some("gameplay.definitions.invalid")
    );
    let other = json!({"version":1,"data":{"definitions":"nope"}});
    assert_eq!(
        load(
            vec![entity("a", json!({"engine.AbilitySystem": other}))],
            definitions()
        )
        .as_deref(),
        Some("project.asset.unknown")
    );
}
