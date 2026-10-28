use bevy_ecs::prelude::*;
use engine_project::Result;
use engine_project::gameplay::{
    self,
    progression::{
        add_item, advance_dialogue, equip_item, equipped_item, item_count, quest_objectives,
        quest_state, remove_item, select_choice, start_dialogue, start_quest, transfer_item,
        unequip_item,
    },
};
use engine_project::*;
use serde_json::{Value, json};
use std::fs;

fn definitions() -> Value {
    json!({"version":1,
        "attribute_sets":[{"name":"Stats","attributes":[{"name":"Strength","base":10}]}],
        "effects":[
            {"name":"Might","policy":"infinite","granted_tags":["State.Mighty"],
                "modifiers":[{"attribute":"Strength","magnitude":{"scalable":{"value":5}}}]},
            {"name":"Blessing","policy":"infinite","granted_tags":["State.Blessed"]}],
        "items":[
            {"id":"potion","stack":5,"weight":0.5,"tags":["consumable"]},
            {"id":"sword","stack":1,"weight":4,"tags":["weapon"],"equip_effect":"Might"},
            {"id":"pelt","stack":10,"weight":1},
            {"id":"coin","stack":100}],
        "quests":[{"id":"wolves",
            "stages":[
                {"id":"hunt","objectives":[{"type":"counter","id":"kills","event":"enemy.killed",
                    "filter":{"kind":"wolf"},"target":2}]},
                {"id":"collect","objectives":[{"type":"has_item","id":"pelts","item":"pelt","count":2}]}],
            "rewards":{"items":[{"item":"coin","count":10}],"effect":"Blessing",
                "event":{"name":"quest.reward","payload":{"big":true}}},
            "fail_on":[{"event":"player.died"}]}]})
}

/// start -> hello -> choice { accept (quest inactive) -> start_quest -> thanks -> end,
///                            gift -> give_item potion x2 -> end, bye -> end }
fn elder() -> Value {
    let node = |id: &str, ty: &str, params: Value| json!({"id":id,"node_type":ty,"params":params});
    let next = |from: &str, to: &str| json!({"from":{"node":from,"port":"next"},"to":{"node":to,"port":"parent"}});
    let option = |choice: &str, id: &str| json!({"from":{"node":choice,"port":"options"},"to":{"node":id,"port":"owner"}});
    json!({"version":1,"kind":"dialogue",
        "nodes":[
            {"id":"start","node_type":"start"},
            node("hello","line",json!({"speaker":"elder","text":"elder.hello"})),
            node("ask","choice",json!({"prompt":"elder.prompt"})),
            node("o_accept","option",json!({"text":"opt.accept",
                "conditions":[{"type":"quest","quest":"wolves","state":"inactive"}]})),
            node("o_gift","option",json!({"text":"opt.gift","conditions":[]})),
            node("o_bye","option",json!({"text":"opt.bye"})),
            node("take","start_quest",json!({"quest":"wolves"})),
            node("thanks","line",json!({"speaker":"elder","text":"elder.thanks"})),
            node("done","end",json!({"reason":"accepted"})),
            node("give","give_item",json!({"item":"potion","count":2})),
            node("gifted","end",json!({"reason":"gifted"})),
            node("leave","end",json!({}))],
        "edges":[
            next("start","hello"), next("hello","ask"),
            option("ask","o_accept"), option("ask","o_gift"), option("ask","o_bye"),
            next("o_accept","take"), next("take","thanks"), next("thanks","done"),
            next("o_gift","give"), next("give","gifted"),
            next("o_bye","leave")]})
}

const EVENTS: [&str; 16] = [
    "inventory.changed",
    "inventory.full",
    "inventory.equipped",
    "inventory.unequipped",
    "quest.started",
    "quest.objective_progressed",
    "quest.stage_advanced",
    "quest.completed",
    "quest.failed",
    "quest.reward",
    "dialogue.line",
    "dialogue.choices",
    "dialogue.ended",
    "effect.applied",
    "effect.removed",
    "enemy.killed",
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

fn hero_components() -> Value {
    json!({
        "engine.Inventory":{"version":1,"data":{"definitions":"defs",
            "capacity":{"slots":4,"max_weight":20},
            "items":[{"item":"potion","count":3}],
            "equipment":[{"name":"main_hand","tag":"weapon"}]}},
        "engine.QuestLog":{"version":1,"data":{"definitions":"defs"}},
        "engine.AbilitySystem":{"version":1,"data":{"definitions":"defs","attribute_sets":["Stats"]}}})
}

fn scene() -> Vec<Value> {
    vec![
        entity("hero", hero_components()),
        entity(
            "elder",
            json!({"engine.DialogueParticipant":{"version":1,"data":{"graph":"elder","definitions":"defs"}}}),
        ),
        entity(
            "chest",
            json!({"engine.Inventory":{"version":1,"data":{"definitions":"defs",
                "capacity":{"slots":1,"max_weight":0}}}}),
        ),
    ]
}

fn project(entities: Vec<Value>, defs: Value, graph: Value) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("defs.gameplay.json"),
        serde_json::to_vec_pretty(&defs).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("elder.graph.json"),
        serde_json::to_vec_pretty(&graph).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        json!({"schema":1,"name":"progression","startup_scene":"scene.json","assets":[
            {"id":"defs","kind":"gameplay","path":"defs.gameplay.json"},
            {"id":"elder","kind":"graph","path":"elder.graph.json"}]})
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

#[derive(Resource, Default)]
struct Log(Vec<(String, Value)>);
fn record(world: &mut World, _: Entity, event: &EngineEvent) -> Result<()> {
    world
        .resource_mut::<Log>()
        .0
        .push((event.name.clone(), event.payload.clone()));
    Ok(())
}

fn started(dir: &tempfile::TempDir) -> Runtime {
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
        runtime.tick(0.1, InputSnapshot::default()).unwrap();
    }
}

fn names(runtime: &Runtime) -> Vec<String> {
    runtime
        .app()
        .world()
        .resource::<Log>()
        .0
        .iter()
        .map(|(n, _)| n.clone())
        .collect()
}

fn events(runtime: &Runtime, name: &str) -> Vec<Value> {
    runtime
        .app()
        .world()
        .resource::<Log>()
        .0
        .iter()
        .filter(|(n, _)| n == name)
        .map(|(_, p)| p.clone())
        .collect()
}

fn send(
    runtime: &mut Runtime,
    name: &str,
    source: Option<Entity>,
    target: Option<Entity>,
    payload: Value,
) {
    emit_event(
        runtime.app_mut().world_mut(),
        EngineEvent {
            name: name.into(),
            source,
            target,
            payload,
        },
    )
    .unwrap();
}

fn progression(runtime: &Runtime) -> Value {
    serde_json::to_value(runtime.try_snapshot().unwrap().progression).unwrap()
}

fn texts(payload: &Value) -> Vec<String> {
    payload["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["text"].as_str().unwrap().to_owned())
        .collect()
}

/// The whole story: talk, take the quest, hunt, collect, get paid, equip.
fn story() -> (Vec<(String, Value)>, Value) {
    let dir = project(scene(), definitions(), elder());
    let mut runtime = started(&dir);
    let hero = runtime.entity("hero").unwrap();
    let elder = runtime.entity("elder").unwrap();
    let chest = runtime.entity("chest").unwrap();
    tick(&mut runtime, 1);

    // Dialogue through engine events: start, read the line, advance, pick "accept".
    send(
        &mut runtime,
        "dialogue.start",
        Some(hero),
        Some(elder),
        json!({}),
    );
    tick(&mut runtime, 2);
    let line = events(&runtime, "dialogue.line");
    assert_eq!(line[0]["text"], "elder.hello");
    assert_eq!(line[0]["npc"], "elder");
    send(
        &mut runtime,
        "dialogue.advance",
        None,
        Some(elder),
        json!({}),
    );
    tick(&mut runtime, 2);
    let choices = &events(&runtime, "dialogue.choices")[0];
    assert_eq!(texts(choices), ["opt.accept", "opt.gift", "opt.bye"]);
    send(
        &mut runtime,
        "dialogue.select",
        None,
        Some(elder),
        json!({"index":0}),
    );
    tick(&mut runtime, 2);
    assert_eq!(
        quest_state(runtime.app().world(), hero, "wolves"),
        engine_gameplay_state("active")
    );
    assert_eq!(events(&runtime, "quest.started").len(), 1);
    send(
        &mut runtime,
        "dialogue.advance",
        None,
        Some(elder),
        json!({}),
    );
    tick(&mut runtime, 2);
    assert_eq!(events(&runtime, "dialogue.ended")[0]["reason"], "accepted");

    // Hunt: only wolves count.
    send(
        &mut runtime,
        "enemy.killed",
        None,
        Some(hero),
        json!({"kind":"bear"}),
    );
    tick(&mut runtime, 2);
    assert!(events(&runtime, "quest.objective_progressed").is_empty());
    for _ in 0..2 {
        send(
            &mut runtime,
            "enemy.killed",
            None,
            Some(hero),
            json!({"kind":"wolf"}),
        );
        tick(&mut runtime, 2);
    }
    let progress = events(&runtime, "quest.objective_progressed");
    assert_eq!(progress.len(), 2);
    assert_eq!(
        (progress[1]["value"].clone(), progress[1]["target"].clone()),
        (json!(2), json!(2))
    );
    assert_eq!(
        events(&runtime, "quest.stage_advanced")[0]["stage"],
        "collect"
    );
    assert_eq!(
        quest_objectives(runtime.app().world(), hero, "wolves"),
        [("pelts".to_owned(), 0, 1)]
    );

    // Collect: the second pelt completes the quest, once; rewards arrive once.
    let w = runtime.app_mut().world_mut();
    add_item(w, hero, "pelt", 1).unwrap();
    assert_eq!(events_len(w), 0, "one pelt is not enough");
    add_item(w, hero, "pelt", 1).unwrap();
    tick(&mut runtime, 2);
    assert_eq!(events(&runtime, "quest.completed").len(), 1);
    assert_eq!(item_count(runtime.app().world(), hero, "coin"), 10);
    assert!(gameplay::has_tag(
        runtime.app().world(),
        hero,
        "State.Blessed",
        true
    ));
    assert_eq!(events(&runtime, "quest.reward"), [json!({"big":true})]);
    add_item(runtime.app_mut().world_mut(), hero, "pelt", 3).unwrap();
    tick(&mut runtime, 2);
    assert_eq!(
        events(&runtime, "quest.completed").len(),
        1,
        "no second completion"
    );
    assert_eq!(item_count(runtime.app().world(), hero, "coin"), 10);

    // Equip a sword: the Might effect raises Strength while it is worn.
    let w = runtime.app_mut().world_mut();
    remove_item(w, hero, "pelt", 5).unwrap();
    add_item(w, hero, "sword", 1).unwrap();
    let slot = {
        let p = progression_slot(w, hero, "sword");
        p.expect("the sword has a slot")
    };
    assert_eq!(
        gameplay::attribute(w, hero, "Strength").unwrap().current,
        10.0
    );
    equip_item(w, hero, slot, "main_hand").unwrap();
    assert_eq!(
        gameplay::attribute(w, hero, "Strength").unwrap().current,
        15.0
    );
    assert_eq!(
        equipped_item(w, hero, "main_hand").as_deref(),
        Some("sword")
    );
    assert!(gameplay::has_tag(w, hero, "State.Mighty", true));
    unequip_item(w, hero, "main_hand").unwrap();
    assert_eq!(
        gameplay::attribute(w, hero, "Strength").unwrap().current,
        10.0
    );
    assert_eq!(item_count(w, hero, "sword"), 1);

    // Overflow is refused whole and reported; transfers are atomic.
    let before = item_count(w, hero, "coin");
    let err = add_item(w, hero, "coin", 100_000).unwrap_err();
    assert_eq!(err.code(), "inventory.full");
    assert_eq!(item_count(w, hero, "coin"), before);
    assert_eq!(
        transfer_item(w, hero, chest, "coin", 11)
            .unwrap_err()
            .code(),
        "inventory.not_enough"
    );
    assert!(transfer_item(w, hero, chest, "coin", 10).is_ok());
    assert_eq!(
        (item_count(w, hero, "coin"), item_count(w, chest, "coin")),
        (0, 10)
    );
    assert_eq!(
        transfer_item(w, hero, chest, "potion", 1)
            .unwrap_err()
            .code(),
        "inventory.full",
        "the chest has a single slot and it holds coins"
    );
    assert_eq!(item_count(w, hero, "potion"), 3);
    tick(&mut runtime, 2);
    assert!(!events(&runtime, "inventory.full").is_empty());
    assert!(!events(&runtime, "inventory.changed").is_empty());
    (
        runtime.app().world().resource::<Log>().0.clone(),
        progression(&runtime),
    )
}

fn engine_gameplay_state(name: &str) -> engine_gameplay::quest::QuestState {
    use engine_gameplay::quest::QuestState;
    match name {
        "active" => QuestState::Active,
        "completed" => QuestState::Completed,
        "failed" => QuestState::Failed,
        _ => QuestState::Inactive,
    }
}

fn events_len(w: &World) -> usize {
    w.resource::<Log>()
        .0
        .iter()
        .filter(|(n, _)| n == "quest.completed")
        .count()
}

/// The first inventory slot holding `item`, found by splitting nothing: scans telemetry-free
/// through successive `split_item` probes would change state, so read it from the layout.
fn progression_slot(w: &mut World, hero: Entity, item: &str) -> Option<usize> {
    (0..4).find(|slot| {
        // equipping an empty slot fails with `inventory.empty_slot`; any other outcome
        // means the slot holds something, which we then check by item name.
        let probe = engine_project::gameplay::progression::slot_item(w, hero, *slot);
        probe.as_deref() == Some(item)
    })
}

#[test]
fn inventory_quests_and_dialogue_work_together_headlessly() {
    let (events, status) = story();
    assert!(events.iter().any(|(n, _)| n == "quest.completed"));
    // Telemetry.
    let hero_inventory = status["inventories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["entity"] == "hero")
        .unwrap();
    assert_eq!(hero_inventory["slots"], 4);
    assert_eq!(hero_inventory["items"]["potion"], 3);
    let log = &status["quest_logs"][0];
    assert_eq!(log["quests"][0]["quest"], "wolves");
    assert_eq!(log["quests"][0]["state"], "completed");
    assert_eq!(status["dialogues"][0]["npc"], "elder");
    assert!(
        status["dialogues"][0]["waiting"].is_null(),
        "the conversation is over"
    );
}

#[test]
fn two_runs_give_identical_events_and_telemetry() {
    let first = story();
    let second = story();
    assert_eq!(first.0, second.0);
    assert_eq!(first.1, second.1);
}

#[test]
fn natives_drive_dialogue_and_quests_and_reject_bad_calls() {
    let dir = project(scene(), definitions(), elder());
    let mut runtime = started(&dir);
    tick(&mut runtime, 1);
    let hero = runtime.entity("hero").unwrap();
    let elder = runtime.entity("elder").unwrap();
    let w = runtime.app_mut().world_mut();
    assert_eq!(
        advance_dialogue(w, elder).unwrap_err().code(),
        "progression.no_conversation"
    );
    let first = start_dialogue(w, elder, hero).unwrap();
    assert!(matches!(
        first,
        engine_gameplay::dialogue::Output::Line { .. }
    ));
    assert_eq!(
        select_choice(w, elder, 0).unwrap_err().code(),
        "dialogue.wrong_state"
    );
    advance_dialogue(w, elder).unwrap();
    assert_eq!(
        select_choice(w, elder, 7).unwrap_err().code(),
        "dialogue.bad_choice"
    );
    // "gift" gives two potions; the conversation ends.
    let ended = select_choice(w, elder, 1).unwrap();
    assert!(
        matches!(ended, engine_gameplay::dialogue::Output::Ended { ref reason } if reason == "gifted")
    );
    assert_eq!(item_count(w, hero, "potion"), 5);
    // Quests: start once, then refused; abandon and fail paths.
    start_quest(w, hero, "wolves").unwrap();
    assert_eq!(
        start_quest(w, hero, "wolves").unwrap_err().code(),
        "quest.not_startable"
    );
    assert_eq!(
        start_quest(w, hero, "ghost").unwrap_err().code(),
        "quest.unknown"
    );
    assert_eq!(
        start_quest(w, elder, "wolves").unwrap_err().code(),
        "progression.no_quest_log"
    );
    assert!(gameplay::progression::abandon_quest(w, hero, "wolves").unwrap());
    start_quest(w, hero, "wolves").unwrap();
    gameplay::progression::fail_quest(w, hero, "wolves").unwrap();
    assert_eq!(
        quest_state(w, hero, "wolves"),
        engine_gameplay_state("failed")
    );
    assert_eq!(
        add_item(w, elder, "potion", 1).unwrap_err().code(),
        "progression.no_inventory"
    );
    tick(&mut runtime, 2);
    assert!(names(&runtime).contains(&"quest.failed".to_owned()));
}

#[test]
fn project_validation_checks_definitions_and_references() {
    let load = |entities: Vec<Value>, defs: Value, graph: Value| {
        let dir = project(entities, defs, graph);
        Project::load(dir.path().join("project.json"))
            .and_then(Runtime::new)
            .and_then(|mut r| r.start())
            .unwrap_err()
            .diagnostics[0]
            .code
            .clone()
    };
    // A gameplay asset with a broken quest.
    let mut bad = definitions();
    bad["quests"][0]["rewards"]["items"][0]["item"] = json!("ghost");
    assert_eq!(load(scene(), bad, elder()), "gameplay.definitions.invalid");
    // Initial items that do not fit.
    let mut crowded = scene();
    crowded[0]["components"]["custom"]["engine.Inventory"]["data"]["items"] =
        json!([{"item":"sword","count":9}]);
    assert_eq!(
        load(crowded, definitions(), elder()),
        "progression.component.invalid"
    );
    // A dialogue naming a quest the definitions do not have.
    let mut graph = elder();
    graph["nodes"][6]["params"]["quest"] = json!("ghost");
    assert_eq!(
        load(scene(), definitions(), graph),
        "dialogue.reference.unknown"
    );
    // A behavior tree is not a dialogue.
    let tree = json!({"version":1,"kind":"behavior_tree",
        "nodes":[{"id":"root","node_type":"root"},{"id":"w","node_type":"wait"}],
        "edges":[{"from":{"node":"root","port":"child"},"to":{"node":"w","port":"parent"}}]});
    assert_eq!(load(scene(), definitions(), tree), "dialogue.graph.kind");
    // The dialogue graph itself is validated with the project.
    let mut broken = elder();
    broken["nodes"][8]["params"]["reason"] = json!(5);
    assert_eq!(load(scene(), definitions(), broken), "graph.param.type");
    // Components are listed with schemas.
    let dir = project(scene(), definitions(), elder());
    let loaded = Project::load(dir.path().join("project.json")).unwrap();
    for name in [
        "engine.Inventory",
        "engine.QuestLog",
        "engine.DialogueParticipant",
    ] {
        assert_eq!(loaded.component_catalog.0[name].version, 1, "{name}");
    }
    assert_eq!(
        engine_project::graphs::kinds().names().collect::<Vec<_>>(),
        ["behavior_tree", "dialogue", "script_graph", "state_tree"]
    );
}
