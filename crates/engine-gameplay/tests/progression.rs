//! Inventory, quests and dialogue (pure).
use engine_gameplay::{
    dialogue::{self, Conversation, DialogueHost, Output, dlg_codes},
    inventory::{
        Capacity, Container, Equipment, EquipmentSlotDef, InventoryError, ItemDef, ItemTable,
        transfer,
    },
    quest::{
        NoContext, QuestContext, QuestDef, QuestError, QuestEvent, QuestLog, QuestState,
        QuestTable, validate_quests,
    },
};
use engine_graph::GraphDocument;
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc};

// ---------------------------------------------------------------- inventory

fn items() -> ItemTable {
    let def = |id: &str, stack: u32, weight: f32, tags: &[&str]| ItemDef {
        id: id.into(),
        stack,
        weight,
        tags: tags.iter().map(|t| (*t).to_owned()).collect(),
        ..ItemDef::default()
    };
    ItemTable::new(&[
        def("potion", 5, 0.5, &["consumable"]),
        def("sword", 1, 4.0, &["weapon"]),
        def("coin", 100, 0.0, &[]),
    ])
    .unwrap()
}

fn bag(slots: u32, max_weight: f32) -> Container {
    Container::new(Capacity { slots, max_weight })
}

fn layout(c: &Container) -> Vec<Option<(String, u32)>> {
    c.slots()
        .iter()
        .map(|s| s.as_ref().map(|s| (s.item.clone(), s.count)))
        .collect()
}

fn some(item: &str, count: u32) -> Option<(String, u32)> {
    Some((item.to_owned(), count))
}

#[test]
fn adding_stacks_into_existing_stacks_first_and_overflow_is_atomic() {
    let table = items();
    // (adds, expected layout of a 3-slot bag after them, error of the last add)
    type Case = (
        Vec<(&'static str, u32)>,
        Vec<Option<(String, u32)>>,
        Option<InventoryError>,
    );
    let cases: Vec<Case> = vec![
        (
            vec![("potion", 12)],
            vec![some("potion", 5), some("potion", 5), some("potion", 2)],
            None,
        ),
        (
            vec![("potion", 12), ("potion", 3)],
            vec![some("potion", 5), some("potion", 5), some("potion", 5)],
            None,
        ),
        (
            vec![("potion", 12), ("potion", 4)],
            vec![some("potion", 5), some("potion", 5), some("potion", 2)],
            Some(InventoryError::Full),
        ),
        (
            vec![("sword", 1), ("potion", 3), ("potion", 4)],
            vec![some("sword", 1), some("potion", 5), some("potion", 2)],
            None,
        ),
        (
            vec![("sword", 3), ("sword", 1)],
            vec![some("sword", 1); 3],
            Some(InventoryError::Full),
        ),
        (
            vec![("coin", 250)],
            vec![some("coin", 100), some("coin", 100), some("coin", 50)],
            None,
        ),
        (
            vec![("potion", 0)],
            vec![None, None, None],
            Some(InventoryError::InvalidCount),
        ),
        (
            vec![("ghost", 1)],
            vec![None, None, None],
            Some(InventoryError::UnknownItem),
        ),
    ];
    for (adds, expected, error) in cases {
        let mut container = bag(3, 0.0);
        let mut last = None;
        for (item, count) in &adds {
            let before = container.clone();
            last = container.add(&table, item, *count).err();
            if last.is_some() {
                assert_eq!(container, before, "a refused add changes nothing: {adds:?}");
            }
        }
        assert_eq!(layout(&container), expected, "{adds:?}");
        assert_eq!(last, error, "{adds:?}");
    }
}

#[test]
fn weight_limits_refuse_what_would_overload_the_container() {
    let table = items();
    let mut container = bag(8, 10.0);
    assert!(container.add(&table, "sword", 2).is_ok()); // 8
    assert_eq!(
        container.add(&table, "sword", 1),
        Err(InventoryError::Overweight)
    );
    assert!(container.add(&table, "potion", 4).is_ok()); // 10 exactly
    assert!((container.weight(&table) - 10.0).abs() < 1e-4);
    assert_eq!(
        container.add(&table, "potion", 1),
        Err(InventoryError::Overweight)
    );
    assert!(
        container.add(&table, "coin", 50).is_ok(),
        "weightless items always fit"
    );
    assert_eq!(container.count("potion"), 4);
    assert_eq!(container.totals()["coin"], 50);
}

#[test]
fn remove_move_and_split_are_atomic_and_deterministic() {
    let table = items();
    let mut c = bag(4, 0.0);
    c.add(&table, "potion", 8).unwrap(); // [5, 3]
    assert_eq!(c.remove("potion", 9), Err(InventoryError::NotEnough));
    assert_eq!(c.count("potion"), 8);
    c.remove("potion", 4).unwrap(); // highest slot first: [4]... slot1 3 -> 0, slot0 5 -> 4
    assert_eq!(layout(&c), [some("potion", 4), None, None, None]);
    assert_eq!(c.split(0, 4), Err(InventoryError::InvalidCount));
    assert_eq!(c.split(0, 1), Ok(1));
    assert_eq!(
        layout(&c),
        [some("potion", 3), some("potion", 1), None, None]
    );
    // Merge: slot 1 into slot 0 up to the stack size 5 (room 2, only 1 to move).
    c.move_stack(&table, 1, 0, None).unwrap();
    assert_eq!(layout(&c), [some("potion", 4), None, None, None]);
    // Partial move into an empty slot, swap of two different items.
    c.add(&table, "sword", 1).unwrap();
    c.move_stack(&table, 0, 3, Some(1)).unwrap();
    assert_eq!(
        c.move_stack(&table, 3, 1, Some(2)),
        Err(InventoryError::InvalidCount)
    );
    c.move_stack(&table, 1, 0, None).unwrap(); // sword (slot 1) onto potion (slot 0): swap
    assert_eq!(c.slot(0).unwrap().item, "sword");
    assert_eq!(
        c.move_stack(&table, 1, 0, Some(1)),
        Err(InventoryError::Mismatch)
    );
    assert_eq!(
        c.move_stack(&table, 9, 0, None),
        Err(InventoryError::BadSlot)
    );
    assert_eq!(
        c.move_stack(&table, 2, 0, None),
        Err(InventoryError::EmptySlot)
    );
    let mut full = bag(1, 0.0);
    full.add(&table, "sword", 1).unwrap();
    assert_eq!(full.split(0, 1), Err(InventoryError::InvalidCount));
    let mut two = bag(1, 0.0);
    two.add(&table, "potion", 3).unwrap();
    assert_eq!(two.split(0, 1), Err(InventoryError::Full));
}

#[test]
fn a_failed_transfer_leaves_both_containers_unchanged() {
    let table = items();
    let mut a = bag(3, 0.0);
    let mut b = bag(1, 3.0);
    a.add(&table, "potion", 6).unwrap();
    b.add(&table, "potion", 1).unwrap();
    let (a0, b0) = (a.clone(), b.clone());
    // 7 x 0.5 = 3.5 passes b's weight limit of 3 only if weight is ignored: it is checked first.
    assert_eq!(
        transfer(&table, &mut a, &mut b, "potion", 6),
        Err(InventoryError::Overweight)
    );
    assert_eq!((&a, &b), (&a0, &b0));
    // A destination with one slot has room for 4 more potions, not 6.
    let mut one_slot = bag(1, 0.0);
    one_slot.add(&table, "potion", 1).unwrap();
    let o0 = one_slot.clone();
    assert_eq!(
        transfer(&table, &mut a, &mut one_slot, "potion", 6),
        Err(InventoryError::Full)
    );
    assert_eq!((&a, &one_slot), (&a0, &o0));
    // a does not hold 7.
    assert_eq!(
        transfer(&table, &mut a, &mut b, "potion", 7),
        Err(InventoryError::NotEnough)
    );
    assert_eq!((&a, &b), (&a0, &b0));
    // A weight limit refuses on the destination side and nothing leaves the source.
    let mut light = bag(2, 0.5);
    let l0 = light.clone();
    assert_eq!(
        transfer(&table, &mut a, &mut light, "potion", 2),
        Err(InventoryError::Overweight)
    );
    assert_eq!((&a, &light), (&a0, &l0));
    // A transfer that fits moves everything.
    assert!(transfer(&table, &mut a, &mut b, "potion", 4).is_ok());
    assert_eq!((a.count("potion"), b.count("potion")), (2, 5));
}

#[test]
fn equipment_slots_check_tags_and_return_items() {
    let table = items();
    let mut c = bag(2, 0.0);
    let mut eq = Equipment::new(vec![EquipmentSlotDef {
        name: "main_hand".into(),
        tag: "weapon".into(),
    }])
    .unwrap();
    c.add(&table, "sword", 1).unwrap();
    c.add(&table, "potion", 1).unwrap();
    assert_eq!(
        eq.equip(&table, &mut c, 1, "main_hand"),
        Err(InventoryError::NotEquippable)
    );
    assert_eq!(
        eq.equip(&table, &mut c, 0, "off_hand"),
        Err(InventoryError::BadSlot)
    );
    assert_eq!(
        eq.equip(&table, &mut c, 0, "main_hand"),
        Ok("sword".to_owned())
    );
    assert_eq!(c.count("sword"), 0);
    assert_eq!(eq.worn()["main_hand"], "sword");
    c.add(&table, "sword", 1).unwrap();
    assert_eq!(
        eq.equip(&table, &mut c, 0, "main_hand"),
        Err(InventoryError::SlotOccupied)
    );
    // The bag is full (potion + the second sword): unequipping is refused and the sword stays on.
    assert_eq!(
        eq.unequip(&table, &mut c, "main_hand"),
        Err(InventoryError::Full)
    );
    assert!(eq.worn().contains_key("main_hand"));
    c.remove("sword", 1).unwrap();
    assert_eq!(
        eq.unequip(&table, &mut c, "main_hand"),
        Ok("sword".to_owned())
    );
    assert_eq!(c.count("sword"), 1);
    assert_eq!(
        eq.unequip(&table, &mut c, "main_hand"),
        Err(InventoryError::EmptySlot)
    );
}

#[test]
fn item_definitions_are_validated() {
    let bad = |edit: fn(&mut ItemDef)| {
        let mut def = ItemDef {
            id: "x".into(),
            ..ItemDef::default()
        };
        edit(&mut def);
        ItemTable::new(&[def]).is_err()
    };
    assert!(!bad(|_| {}));
    assert!(bad(|d| d.id = "bad id".into()));
    assert!(bad(|d| d.stack = 0));
    assert!(bad(|d| d.weight = f32::NAN));
    assert!(bad(|d| d.tags = vec!["a b".into()]));
    assert!(bad(|d| {
        d.properties.insert("k".into(), json!("x".repeat(2000)));
    }));
    let dup = ItemDef {
        id: "x".into(),
        ..ItemDef::default()
    };
    assert!(ItemTable::new(&[dup.clone(), dup]).is_err());
    assert!(
        Capacity {
            slots: 0,
            max_weight: 0.0
        }
        .validate()
        .is_err()
    );
    assert!(
        Capacity {
            slots: 16,
            max_weight: -1.0
        }
        .validate()
        .is_err()
    );
}

// -------------------------------------------------------------------- quests

fn quests() -> QuestTable {
    let defs: Vec<QuestDef> = serde_json::from_value(json!([
        {"id":"intro","stages":[{"id":"talk","objectives":[{"type":"flag","id":"met","flag":"met_elder"}]}]},
        {"id":"wolves","prerequisites":["intro"],
         "stages":[
            {"id":"hunt","objectives":[{"type":"counter","id":"kills","event":"enemy.killed",
                "filter":{"kind":"wolf","by.team":1},"target":3}]},
            {"id":"collect","objectives":[{"type":"has_item","id":"pelts","item":"pelt","count":2}]}],
         "rewards":{"items":[{"item":"coin","count":10}],"effect":"buff",
                    "event":{"name":"quest.reward","payload":{"big":true}}},
         "fail_on":[{"event":"player.died"}]}
    ]))
    .unwrap();
    let items: BTreeSet<&str> = ["pelt", "coin"].into();
    let effects: BTreeSet<&str> = ["buff"].into();
    validate_quests(&defs, &items, &effects).unwrap();
    QuestTable::new(&defs)
}

struct Bag(u32);
impl QuestContext for Bag {
    fn has_item(&self, item: &str, count: u32) -> bool {
        item == "pelt" && self.0 >= count
    }
}

fn kill(kind: &str, team: u32) -> Value {
    json!({"kind":kind,"by":{"team":team}})
}

#[test]
fn prerequisites_block_and_a_filtered_counter_advances_stages_to_completion() {
    let table = quests();
    let mut log = QuestLog::default();
    assert_eq!(
        log.start(&table, "wolves", &NoContext),
        Err(QuestError::Prerequisite("intro".into()))
    );
    assert_eq!(
        log.start(&table, "ghost", &NoContext),
        Err(QuestError::Unknown)
    );
    log.start(&table, "intro", &NoContext).unwrap();
    assert_eq!(log.state("intro"), QuestState::Active);
    let events = log.set_flag(&table, "met_elder", &NoContext);
    assert!(matches!(events.last(), Some(QuestEvent::Completed { quest, .. }) if quest == "intro"));
    log.start(&table, "wolves", &NoContext).unwrap();
    // Only wolves killed by team 1 count; a bear or a wolf of team 2 does not.
    assert!(
        log.on_event(&table, "enemy.killed", &kill("bear", 1), &NoContext)
            .is_empty()
    );
    assert!(
        log.on_event(&table, "enemy.killed", &kill("wolf", 2), &NoContext)
            .is_empty()
    );
    assert!(
        log.on_event(&table, "other.event", &kill("wolf", 1), &NoContext)
            .is_empty()
    );
    let progress = |events: &[QuestEvent]| {
        events
            .iter()
            .filter_map(|e| match e {
                QuestEvent::ObjectiveProgressed { value, target, .. } => Some((*value, *target)),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        progress(&log.on_event(&table, "enemy.killed", &kill("wolf", 1), &NoContext)),
        [(1, 3)]
    );
    assert_eq!(
        progress(&log.on_event(&table, "enemy.killed", &kill("wolf", 1), &NoContext)),
        [(2, 3)]
    );
    let third = log.on_event(&table, "enemy.killed", &kill("wolf", 1), &NoContext);
    assert_eq!(progress(&third), [(3, 3)]);
    assert!(third.contains(&QuestEvent::StageAdvanced {
        quest: "wolves".into(),
        stage: "collect".into()
    }));
    assert_eq!(log.stage(&table, "wolves").as_deref(), Some("collect"));
    assert_eq!(log.objectives(&table, "wolves"), [("pelts".into(), 0, 1)]);
    // The has-item stage completes when the inventory catches up, and rewards come once.
    assert!(log.refresh(&table, &Bag(1)).is_empty());
    let done = log.refresh(&table, &Bag(2));
    let rewards: Vec<_> = done
        .iter()
        .filter_map(|e| match e {
            QuestEvent::Completed { rewards, .. } => Some(rewards.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(rewards.len(), 1);
    assert_eq!(rewards[0].items[0].count, 10);
    assert_eq!(rewards[0].effect.as_deref(), Some("buff"));
    assert_eq!(rewards[0].event.as_ref().unwrap().name, "quest.reward");
    assert_eq!(log.state("wolves"), QuestState::Completed);
    assert!(log.refresh(&table, &Bag(9)).is_empty(), "no second reward");
    assert!(
        log.on_event(&table, "enemy.killed", &kill("wolf", 1), &NoContext)
            .is_empty()
    );
    assert_eq!(
        log.start(&table, "wolves", &NoContext),
        Err(QuestError::NotStartable(QuestState::Completed))
    );
}

#[test]
fn quests_fail_on_their_events_restart_and_can_be_abandoned() {
    let table = quests();
    let mut log = QuestLog::default();
    log.start(&table, "intro", &NoContext).unwrap();
    log.set_flag(&table, "met_elder", &NoContext);
    log.start(&table, "wolves", &NoContext).unwrap();
    log.on_event(&table, "enemy.killed", &kill("wolf", 1), &NoContext);
    let failed = log.on_event(&table, "player.died", &json!({}), &NoContext);
    assert_eq!(
        failed,
        [QuestEvent::Failed {
            quest: "wolves".into()
        }]
    );
    assert_eq!(log.state("wolves"), QuestState::Failed);
    assert!(
        log.on_event(&table, "enemy.killed", &kill("wolf", 1), &NoContext)
            .is_empty()
    );
    // A failed quest can be started again with fresh progress.
    log.start(&table, "wolves", &NoContext).unwrap();
    assert_eq!(log.objectives(&table, "wolves")[0].1, 0);
    assert!(log.abandon("wolves"));
    assert_eq!(log.state("wolves"), QuestState::Inactive);
    assert!(!log.abandon("wolves"));
    assert_eq!(log.states(), [("intro".into(), QuestState::Completed)]);
    // Starting with the items already in hand completes has-item objectives at once.
    let mut other = QuestLog::default();
    other.start(&table, "intro", &NoContext).unwrap();
    other.set_flag(&table, "met_elder", &NoContext);
    other.start(&table, "wolves", &Bag(5)).unwrap();
    for _ in 0..3 {
        other.on_event(&table, "enemy.killed", &kill("wolf", 1), &Bag(5));
    }
    assert_eq!(other.state("wolves"), QuestState::Completed);
    assert!(table.listened_events().contains("enemy.killed"));
    assert!(table.listened_events().contains("player.died"));
}

#[test]
fn quest_definitions_are_validated() {
    let items: BTreeSet<&str> = ["pelt"].into();
    let effects: BTreeSet<&str> = ["buff"].into();
    let check = |doc: Value| {
        let defs: Vec<QuestDef> = serde_json::from_value(doc).unwrap();
        validate_quests(&defs, &items, &effects)
    };
    let stage = json!([{"id":"s","objectives":[{"type":"flag","id":"o","flag":"f"}]}]);
    assert!(check(json!([{"id":"a","stages":stage}])).is_ok());
    assert!(check(json!([{"id":"a","stages":[]}])).is_err());
    assert!(check(json!([{"id":"a","stages":stage,"prerequisites":["ghost"]}])).is_err());
    assert!(
        check(json!([{"id":"a","stages":stage,"prerequisites":["b"]},
        {"id":"b","stages":stage,"prerequisites":["a"]}]))
        .is_err(),
        "prerequisite cycle"
    );
    assert!(check(json!([{"id":"a","stages":stage},{"id":"a","stages":stage}])).is_err());
    assert!(
        check(json!([{"id":"a","stages":[{"id":"s","objectives":[
        {"type":"counter","id":"c","event":"e","target":0}]}]}]))
        .is_err()
    );
    assert!(
        check(json!([{"id":"a","stages":[{"id":"s","objectives":[
        {"type":"has_item","id":"c","item":"nope","count":1}]}]}]))
        .is_err()
    );
    assert!(check(json!([{"id":"a","stages":stage,"rewards":{"effect":"nope"}}])).is_err());
    assert!(
        check(json!([{"id":"a","stages":stage,"rewards":{"items":[{"item":"pelt","count":0}]}}]))
            .is_err()
    );
    assert!(serde_json::from_value::<Vec<QuestDef>>(json!([{"id":"a","bogus":1}])).is_err());
}

// ------------------------------------------------------------------ dialogue

struct D {
    nodes: Vec<Value>,
    edges: Vec<Value>,
    variables: Vec<Value>,
}

impl D {
    fn new() -> Self {
        Self {
            nodes: vec![json!({"id":"start","node_type":"start"})],
            edges: vec![],
            variables: vec![],
        }
    }
    fn var(mut self, name: &str, ty: &str, default: Value) -> Self {
        let mut v = json!({"name":name,"type":ty});
        if !default.is_null() {
            v["default"] = default;
        }
        self.variables.push(v);
        self
    }
    fn node(mut self, id: &str, ty: &str, params: Value) -> Self {
        self.nodes
            .push(json!({"id":id,"node_type":ty,"params":params}));
        self
    }
    /// `from.next -> to.parent`
    fn next(mut self, from: &str, to: &str) -> Self {
        self.edges
            .push(json!({"from":{"node":from,"port":"next"},"to":{"node":to,"port":"parent"}}));
        self
    }
    fn option(
        mut self,
        choice: &str,
        option: &str,
        text: &str,
        conditions: Value,
        to: &str,
    ) -> Self {
        self = self.node(
            option,
            "option",
            json!({"text":text,"conditions":conditions}),
        );
        self.edges.push(
            json!({"from":{"node":choice,"port":"options"},"to":{"node":option,"port":"owner"}}),
        );
        self.next(option, to)
    }
    fn doc(self) -> GraphDocument {
        GraphDocument::from_value(json!({"version":1,"kind":"dialogue",
            "nodes":self.nodes,"edges":self.edges,"variables":self.variables}))
        .unwrap()
    }
}

fn village() -> D {
    D::new()
        .var("mood", "int", json!(0))
        .node(
            "hello",
            "line",
            json!({"speaker":"elder","text":"elder.hello"}),
        )
        .node("ask", "choice", json!({"prompt":"elder.prompt"}))
        .node("take", "start_quest", json!({"quest":"wolves"}))
        .node(
            "thanks",
            "line",
            json!({"speaker":"elder","text":"elder.thanks"}),
        )
        .node("reward", "give_item", json!({"item":"coin","count":5}))
        .node(
            "event",
            "emit_event",
            json!({"event":"elder.paid","payload":{"n":1}}),
        )
        .node("moody", "set_variable", json!({"key":"mood","value":2}))
        .node("bye", "end", json!({"reason":"goodbye"}))
        .node("back", "jump", json!({"target":"ask"}))
        .node(
            "rude",
            "line",
            json!({"speaker":"elder","text":"elder.rude"}),
        )
        .next("start", "hello")
        .next("hello", "ask")
        .option(
            "ask",
            "o_quest",
            "opt.quest",
            json!([{"type":"quest","quest":"wolves","state":"inactive"}]),
            "take",
        )
        .option(
            "ask",
            "o_turn_in",
            "opt.turn_in",
            json!([{"type":"quest","quest":"wolves","state":"completed"},
            {"type":"item","item":"pelt","count":2}]),
            "reward",
        )
        .option(
            "ask",
            "o_rude",
            "opt.rude",
            json!([{"type":"variable","key":"mood","operator":"lt","value":1}]),
            "moody",
        )
        .option("ask", "o_leave", "opt.leave", json!([]), "bye")
        .next("take", "thanks")
        .next("thanks", "bye_after_quest")
        .node("bye_after_quest", "end", json!({"reason":"accepted"}))
        .next("reward", "event")
        .next("event", "back")
        .next("moody", "rude")
        .next("rude", "back2")
        .node("back2", "jump", json!({"target":"ask"}))
}

#[derive(Default)]
struct Host {
    quests: std::collections::BTreeMap<String, QuestState>,
    pelts: u32,
    log: Vec<String>,
}

impl DialogueHost for Host {
    fn quest_state(&mut self, quest: &str) -> QuestState {
        self.quests
            .get(quest)
            .copied()
            .unwrap_or(QuestState::Inactive)
    }
    fn has_item(&mut self, item: &str, count: u32) -> bool {
        item == "pelt" && self.pelts >= count
    }
    fn emit_event(&mut self, name: &str, payload: &Value) {
        self.log.push(format!("event:{name}:{payload}"));
    }
    fn start_quest(&mut self, quest: &str) {
        self.log.push(format!("quest:{quest}"));
        self.quests.insert(quest.into(), QuestState::Active);
    }
    fn give_item(&mut self, item: &str, count: u32) {
        self.log.push(format!("give:{item}x{count}"));
    }
}

fn texts(output: &Output) -> Vec<String> {
    match output {
        Output::Choices { options, .. } => options.iter().map(|o| o.text.clone()).collect(),
        Output::Line { text, .. } => vec![text.clone()],
        Output::Ended { reason } => vec![format!("ended:{reason}")],
    }
}

#[test]
fn a_conversation_offers_conditional_choices_and_runs_side_effects() {
    let dialogue = Arc::new(dialogue::compile(&village().doc()).unwrap());
    assert_eq!(dialogue.node_count(), 17);
    assert!(dialogue.quests().contains("wolves") && dialogue.items().contains("coin"));
    let mut host = Host::default();
    let mut talk = Conversation::new(dialogue.clone());
    assert_eq!(talk.waiting_for(), "idle");
    assert_eq!(texts(&talk.begin(&mut host).unwrap()), ["elder.hello"]);
    assert!(talk.begin(&mut host).is_err(), "only once");
    // The quest is inactive: it can be taken; the turn-in is hidden; mood 0 < 1 allows rudeness.
    let choices = talk.advance(&mut host).unwrap();
    assert_eq!(texts(&choices), ["opt.quest", "opt.rude", "opt.leave"]);
    assert!(matches!(&choices, Output::Choices { prompt: Some(p), .. } if p == "elder.prompt"));
    assert_eq!(
        talk.select(9, &mut host),
        Err(dialogue::DialogueError::BadChoice)
    );
    assert_eq!(
        talk.advance(&mut host),
        Err(dialogue::DialogueError::WrongState)
    );
    // Rude: sets mood 2, shows the line, jumps back; the rude option is gone now.
    assert_eq!(texts(&talk.select(1, &mut host).unwrap()), ["elder.rude"]);
    assert_eq!(talk.variables()["mood"], 2);
    let again = talk.advance(&mut host).unwrap();
    assert_eq!(texts(&again), ["opt.quest", "opt.leave"]);
    // Taking the quest starts it, thanks the player and ends.
    assert_eq!(texts(&talk.select(0, &mut host).unwrap()), ["elder.thanks"]);
    assert_eq!(host.log, ["quest:wolves"]);
    assert_eq!(texts(&talk.advance(&mut host).unwrap()), ["ended:accepted"]);
    assert!(talk.is_over());
    assert_eq!(
        talk.select(0, &mut host),
        Err(dialogue::DialogueError::WrongState)
    );

    // With the quest completed and two pelts, the turn-in appears; it gives coins and
    // emits an event, then returns to the choices.
    let mut host = Host {
        pelts: 2,
        ..Host::default()
    };
    host.quests.insert("wolves".into(), QuestState::Completed);
    let mut talk = Conversation::new(dialogue.clone());
    talk.begin(&mut host).unwrap();
    let choices = talk.advance(&mut host).unwrap();
    assert_eq!(texts(&choices), ["opt.turn_in", "opt.rude", "opt.leave"]);
    let back = talk.select(0, &mut host).unwrap();
    assert_eq!(texts(&back), ["opt.turn_in", "opt.rude", "opt.leave"]);
    assert_eq!(host.log, ["give:coinx5", "event:elder.paid:{\"n\":1}"]);
    assert_eq!(
        texts(&talk.select(2, &mut host).unwrap()),
        ["ended:goodbye"]
    );

    // Two runs of the same script give the same outputs.
    let run = || {
        let mut host = Host::default();
        let mut talk = Conversation::new(dialogue.clone());
        let mut out = vec![talk.begin(&mut host).unwrap()];
        out.push(talk.advance(&mut host).unwrap());
        out.push(talk.select(1, &mut host).unwrap());
        out.push(talk.advance(&mut host).unwrap());
        (out, host.log)
    };
    assert_eq!(run(), run());
}

#[test]
fn a_choice_with_nothing_to_offer_ends_and_jump_loops_are_cut() {
    let hidden = D::new()
        .node("ask", "choice", json!({}))
        .next("start", "ask")
        .option(
            "ask",
            "o",
            "opt",
            json!([{"type":"item","item":"key","count":1}]),
            "stop",
        )
        .node("stop", "end", json!({}));
    let dialogue = Arc::new(dialogue::compile(&hidden.doc()).unwrap());
    let mut talk = Conversation::new(dialogue);
    assert_eq!(
        texts(&talk.begin(&mut Host::default()).unwrap()),
        ["ended:no_choices"]
    );
    let looping = D::new()
        .node("a", "set_variable", json!({"key":"n","value":1}))
        .node("b", "jump", json!({"target":"a"}))
        .var("n", "int", Value::Null)
        .next("start", "a")
        .next("a", "b");
    let dialogue = Arc::new(dialogue::compile(&looping.doc()).unwrap());
    let mut talk = Conversation::new(dialogue);
    assert_eq!(
        texts(&talk.begin(&mut Host::default()).unwrap()),
        ["ended:loop"]
    );
}

fn codes_of(doc: &GraphDocument) -> Vec<String> {
    match dialogue::compile(doc) {
        Ok(_) => Vec::new(),
        Err(d) => d.into_iter().map(|d| d.code).collect(),
    }
}

#[test]
fn every_dialogue_code_has_a_focused_negative_case() {
    let mut hit = BTreeSet::new();
    let mut expect = |code: &'static str, doc: GraphDocument| {
        let found = codes_of(&doc);
        assert!(
            found.iter().any(|c| c == code),
            "expected {code}, got {found:?}"
        );
        hit.insert(code);
    };
    let base = || {
        D::new()
            .node("l", "line", json!({"text":"k"}))
            .next("start", "l")
    };
    // a choice without options
    expect(
        dlg_codes::CHOICE_EMPTY,
        D::new()
            .node("c", "choice", json!({}))
            .next("start", "c")
            .doc(),
    );
    // too many options
    // The end node has a single parent port, so give every option its own end.
    let mut many = D::new().node("c", "choice", json!({})).next("start", "c");
    for i in 0..17 {
        many = many.node(&format!("e{i}"), "end", json!({})).option(
            "c",
            &format!("o{i}"),
            "t",
            json!([]),
            &format!("e{i}"),
        );
    }
    expect(dlg_codes::CHOICE_LIMIT, many.doc());
    // jump to a node that does not exist, and to the start node
    expect(
        dlg_codes::JUMP_TARGET,
        base()
            .node("j", "jump", json!({"target":"ghost"}))
            .next("l", "j")
            .doc(),
    );
    expect(
        dlg_codes::JUMP_TARGET,
        base()
            .node("j", "jump", json!({"target":"start"}))
            .next("l", "j")
            .doc(),
    );
    // unknown variable, wrong operand type, ordering on a string, unknown condition type
    let option_with = |conditions: Value| {
        D::new()
            .var("name", "string", Value::Null)
            .node("c", "choice", json!({}))
            .node("e", "end", json!({}))
            .next("start", "c")
            .option("c", "o", "t", conditions, "e")
            .doc()
    };
    expect(
        dlg_codes::CONDITION,
        option_with(json!([{"type":"variable","key":"ghost"}])),
    );
    expect(
        dlg_codes::CONDITION,
        option_with(json!([{"type":"variable","key":"name","operator":"eq","value":5}])),
    );
    expect(
        dlg_codes::CONDITION,
        option_with(json!([{"type":"variable","key":"name","operator":"lt","value":"a"}])),
    );
    expect(
        dlg_codes::CONDITION,
        option_with(json!([{"type":"weather"}])),
    );
    expect(
        dlg_codes::CONDITION,
        option_with(json!([{"type":"item","item":"x","count":0}])),
    );
    // set_variable value of the wrong type
    expect(
        dlg_codes::VALUE_TYPE,
        D::new()
            .var("n", "int", Value::Null)
            .node("s", "set_variable", json!({"key":"n","value":"text"}))
            .node("e", "end", json!({}))
            .next("start", "s")
            .next("s", "e")
            .doc(),
    );
    // event name and payload
    let emit = |params: Value| {
        D::new()
            .node("m", "emit_event", params)
            .node("e", "end", json!({}))
            .next("start", "m")
            .next("m", "e")
            .doc()
    };
    expect(dlg_codes::EVENT_NAME, emit(json!({"event":""})));
    expect(
        dlg_codes::EVENT_PAYLOAD,
        emit(json!({"event":"x","payload":[1]})),
    );
    // empty text keys
    expect(
        dlg_codes::TEXT,
        D::new()
            .node("l", "line", json!({"text":""}))
            .node("e", "end", json!({}))
            .next("start", "l")
            .next("l", "e")
            .doc(),
    );
    for code in dlg_codes::ALL {
        assert!(hit.contains(code), "no negative case for {code}");
    }
    // Graph-level rules still apply: a line whose next is not connected.
    let dangling = base().doc();
    let found = engine_graph::validate(
        &dangling,
        dialogue::shared_kind(),
        engine_graph::Strictness::Complete,
    );
    assert!(
        found
            .iter()
            .any(|d| d.code == engine_graph::codes::PORT_UNCONNECTED)
    );
    assert_eq!(dialogue::kind().name, "dialogue");
}
