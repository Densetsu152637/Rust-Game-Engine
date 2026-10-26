use engine_gameplay::ability::{
    AbilityBehaviour, AbilityRuntime, AbilitySystem, ActivationRequest, ActorInit, ApplyFailure,
    AttributeValue, BehaviourContext, BehaviourRegistry, BehaviourStatus, CancelReason,
    Definitions, FailReason, GameplayEvent, Library, Outcome, TagContainer, TagQuery,
};
use engine_net_predict::{
    KeyResolution, KeyValidator, KeyValidatorConfig, LedgerConfig, PredictionLedger,
};
use serde_json::{Value, json};
use std::sync::Arc;

fn infinite(name: &str, attribute: &str, op: &str, value: f32) -> Value {
    json!({"name":name,"policy":"infinite","modifiers":[
        {"attribute":attribute,"op":op,"magnitude":{"scalable":{"value":value}}}]})
}

fn add(attribute: &str, value: f32) -> Value {
    json!([{"attribute":attribute,"magnitude":{"scalable":{"value":value}}}])
}

fn library() -> Arc<Library> {
    let stacking = |kind: &str, limit: u32, extra: Value| {
        let mut s = json!({"kind":kind,"limit":limit});
        for (k, v) in extra.as_object().unwrap() {
            s[k] = v.clone();
        }
        s
    };
    let effects = vec![
        infinite("Add10", "Stat", "add", 10.0),
        infinite("Add5", "Stat", "add", 5.0),
        infinite("Mul1_5", "Stat", "multiply", 1.5),
        infinite("Mul2", "Stat", "multiply", 2.0),
        infinite("Div2", "Stat", "divide", 2.0),
        infinite("Div4", "Stat", "divide", 4.0),
        infinite("Over7", "Stat", "override", 7.0),
        infinite("Over9", "Stat", "override", 9.0),
        infinite("MaxBuff", "MaxHealth", "add", 50.0),
        infinite("Haste", "Speed", "multiply", 5.0),
        json!({"name":"Damage","modifiers":add("Health",-25.0)}),
        json!({"name":"BigDamage","modifiers":add("Health",-150.0)}),
        json!({"name":"Heal","modifiers":add("Health",30.0)}),
        json!({"name":"BigHeal","modifiers":add("Health",500.0)}),
        json!({"name":"Slow","policy":"duration","duration":2.0,"modifiers":add("Speed",-2.0)}),
        json!({"name":"Mark","policy":"infinite","granted_tags":["Status.Marked"],"modifiers":add("Stat",1.0)}),
        json!({"name":"Tick","policy":"duration","duration":5.0,"period":1.0,"modifiers":add("Counter",1.0)}),
        json!({"name":"TickNow","policy":"duration","duration":5.0,"period":1.0,
            "execute_on_application":true,"modifiers":add("Counter",1.0)}),
        json!({"name":"TickForever","policy":"infinite","period":1.0,"modifiers":add("Counter",1.0)}),
        json!({"name":"Regen","policy":"infinite","period":1.0,"inhibition":"reset_period",
            "ongoing":{"required":["State.Alive"]},"modifiers":add("Counter",1.0)}),
        json!({"name":"StackTarget","policy":"infinite","modifiers":add("Stat",2.0),
            "stacking":stacking("by_target",3,json!({}))}),
        json!({"name":"StackSource","policy":"infinite","modifiers":add("Stat",1.0),
            "stacking":stacking("by_source",2,json!({}))}),
        json!({"name":"StackNone","policy":"infinite","modifiers":add("Stat",1.0)}),
        json!({"name":"Refresh","policy":"duration","duration":2.0,"modifiers":add("Power",1.0),
            "stacking":stacking("by_target",5,json!({}))}),
        json!({"name":"NoRefresh","policy":"duration","duration":2.0,"modifiers":add("Power",1.0),
            "stacking":stacking("by_target",5,json!({"duration_refresh":"never_refresh"}))}),
        json!({"name":"PeriodReset","policy":"duration","duration":10.0,"period":1.0,
            "modifiers":add("Counter",1.0),"stacking":stacking("by_target",2,json!({}))}),
        json!({"name":"ExpireAll","policy":"duration","duration":1.0,"modifiers":add("Power",1.0),
            "stacking":stacking("by_target",3,json!({}))}),
        json!({"name":"ExpireOne","policy":"duration","duration":1.0,"modifiers":add("Power",1.0),
            "stacking":stacking("by_target",3,json!({"expiration":"remove_single_stack"}))}),
        json!({"name":"ExpireRefresh","policy":"duration","duration":1.0,"modifiers":add("Power",1.0),
            "stacking":stacking("by_target",3,json!({"expiration":"refresh_duration"}))}),
        json!({"name":"Deny","policy":"infinite","modifiers":add("Power",1.0),
            "stacking":stacking("by_target",1,json!({"deny_overflow":true}))}),
        json!({"name":"Burn","modifiers":add("Health",-10.0),"application":{"blocked":["State.Immune"]}}),
        json!({"name":"NeedsWet","modifiers":add("Health",-5.0),"application":{"required":["State.Wet"]}}),
        json!({"name":"ManaCost","modifiers":add("Mana",-20.0)}),
        json!({"name":"BigCost","modifiers":add("Mana",-200.0)}),
        json!({"name":"Cooldown","policy":"duration","duration":1.0,"granted_tags":["Cooldown.Strike"]}),
        json!({"name":"Snapshot","policy":"infinite","modifiers":[{"attribute":"Power",
            "magnitude":{"attribute_based":{"attribute":"Strength","coefficient":2.0}}}]}),
        json!({"name":"Live","policy":"infinite","modifiers":[{"attribute":"Power",
            "magnitude":{"attribute_based":{"attribute":"Strength","coefficient":2.0,"capture":"live"}}}]}),
        json!({"name":"StrikeDamage","modifiers":[{"attribute":"Health",
            "magnitude":{"attribute_based":{"attribute":"Strength","coefficient":-1.0}}}]}),
        json!({"name":"Debuff","policy":"infinite","asset_tags":["Debuff.Slow"],"modifiers":add("Speed",-1.0)}),
        json!({"name":"Cleanse","remove_effects_with_tags":["Debuff"]}),
    ];
    let abilities = json!([
        {"name":"Strike","tags":["Ability.Attack.Melee"],"cost":"ManaCost","cooldown":"Cooldown",
            "steps":[{"apply_effect":{"effect":"Damage","to":"target"}}]},
        {"name":"Channel","tags":["Ability.Channel"],"activation_owned_tags":["State.Channeling"],
            "steps":[{"wait":{"seconds":5.0}}]},
        {"name":"Dodge","cancel_abilities_with_tags":["Ability.Channel"]},
        {"name":"Guard","block_abilities_with_tags":["Ability.Attack"],"steps":[{"wait":{"seconds":1.0}}]},
        {"name":"Gated","owner":{"required":["Status.Ready"],"blocked":["State.Stunned"]}},
        {"name":"Expensive","cost":"BigCost"},
        {"name":"Dash","native":"dash","cost":"ManaCost"},
        {"name":"Spawned","native":"dash","instancing":"per_execution"},
        {"name":"Reaction","triggers":["Event.Hit"],"steps":[{"emit_event":{"event":"reacted","payload":{"n":1}}}]},
        {"name":"Cleanser","steps":[{"apply_effect":{"effect":"Cleanse"}}]},
        {"name":"Stacker","steps":[{"apply_effect":{"effect":"StackTarget"}}]},
        {"name":"Manual","commit":"manual","cost":"ManaCost",
            "steps":[{"wait":{"seconds":0.5}},"commit",{"apply_effect":{"effect":"Damage","to":"target"}}]}
    ]);
    let doc = json!({"version":1,
        "attribute_sets":[{"name":"Vitals","attributes":[
            {"name":"MaxHealth","base":100},
            {"name":"Health","base":100,"min":0,"max":"MaxHealth"},
            {"name":"Mana","base":100,"min":0},
            {"name":"Strength","base":10},
            {"name":"Power","base":0},
            {"name":"Counter","base":0},
            {"name":"Speed","base":4,"min":0,"max":10},
            {"name":"Stat","base":100}]}],
        "effects":effects,
        "abilities":abilities});
    Definitions::from_json(&serde_json::to_vec(&doc).unwrap()).unwrap()
}

const ABILITIES: [&str; 12] = [
    "Strike",
    "Channel",
    "Dodge",
    "Guard",
    "Gated",
    "Expensive",
    "Dash",
    "Spawned",
    "Reaction",
    "Cleanser",
    "Stacker",
    "Manual",
];

fn init(attributes: &[(&str, f32)], tags: &[&str]) -> ActorInit {
    ActorInit {
        sets: vec!["Vitals".into()],
        attributes: attributes.iter().map(|(k, v)| ((*k).into(), *v)).collect(),
        abilities: ABILITIES.iter().map(|a| ((*a).into(), 1)).collect(),
        tags: tags.iter().map(|t| (*t).into()).collect(),
    }
}

fn system() -> AbilitySystem {
    let mut s = AbilitySystem::new(library());
    s.add_actor(1, &init(&[], &[])).unwrap();
    s.add_actor(2, &init(&[], &[])).unwrap();
    s
}

/// Host world for native behaviours.
#[derive(Default)]
struct Host {
    log: Vec<String>,
    factories: u32,
}

struct Dash {
    ticks: u32,
}
impl AbilityBehaviour<Host> for Dash {
    fn activate(&mut self, cx: &mut BehaviourContext<'_, Host>) -> BehaviourStatus {
        self.ticks = 0;
        cx.host.log.push(format!("activate {}", cx.instance.0));
        BehaviourStatus::Running
    }
    fn tick(&mut self, cx: &mut BehaviourContext<'_, Host>) -> BehaviourStatus {
        self.ticks += 1;
        if self.ticks == 3 {
            cx.host.log.push(format!("done {}", cx.instance.0));
            BehaviourStatus::Ended
        } else {
            BehaviourStatus::Running
        }
    }
    fn end(&mut self, cx: &mut BehaviourContext<'_, Host>, cancelled: bool) {
        cx.host
            .log
            .push(format!("end {} {cancelled}", cx.instance.0));
    }
}
fn dash(host: &mut Host, _: u64) -> Result<Box<dyn AbilityBehaviour<Host>>, String> {
    host.factories += 1;
    Ok(Box::new(Dash { ticks: 0 }))
}

fn runtime() -> AbilityRuntime<Host> {
    let mut registry = BehaviourRegistry::default();
    registry.register("dash", dash).unwrap();
    AbilityRuntime::new(system(), registry)
}

fn value(s: &AbilitySystem, actor: u64, name: &str) -> AttributeValue {
    s.attribute(actor, name).unwrap()
}

fn ticks(s: &mut AbilitySystem, n: usize, dt: f32) {
    for _ in 0..n {
        s.tick_effects(dt);
    }
}

#[test]
fn tags_match_hierarchically_and_count() {
    let mut tags = TagContainer::new();
    tags.add("State.Debuff.Stunned");
    tags.add("State.Debuff.Stunned");
    tags.add("Team.Red");
    assert!(tags.has("State", false));
    assert!(tags.has("State.Debuff", false));
    assert!(!tags.has("State", true));
    assert!(tags.has("State.Debuff.Stunned", true));
    assert!(
        !tags.has("State.Debuff.Stun", false),
        "segments match whole"
    );
    assert!(!tags.has("Team.RedX", false));
    assert_eq!(tags.count("State.Debuff.Stunned"), 2);
    tags.remove("State.Debuff.Stunned");
    assert!(tags.has("State", false), "one count left");
    tags.remove("State.Debuff.Stunned");
    assert!(!tags.has("State", false));
    let query = TagQuery {
        all: vec!["Team".into()],
        any: vec!["Team.Blue".into(), "Team.Red".into()],
        none: vec!["State".into()],
        exact: false,
    };
    assert!(query.matches(&tags));
    tags.add("State.Dead");
    assert!(!query.matches(&tags));
    let exact = TagQuery {
        all: vec!["Team".into()],
        exact: true,
        ..TagQuery::default()
    };
    assert!(!exact.matches(&tags));
}

#[test]
fn aggregation_order_add_multiply_divide_override() {
    let table: [(&[&str], f32); 10] = [
        (&["Add10", "Add5"], 115.0),
        (&["Mul1_5", "Mul2"], 250.0),
        (&["Div2"], 50.0),
        (&["Div2", "Div4"], 20.0),
        (&["Add10", "Add5", "Mul1_5", "Mul2"], 287.5),
        (&["Add10", "Add5", "Mul1_5", "Mul2", "Div2"], 143.75),
        (&["Mul2", "Add10"], 220.0),
        (&["Add10", "Over7", "Mul2"], 7.0),
        (&["Over7", "Over9"], 9.0),
        (&["Over9", "Over7"], 7.0),
    ];
    for (effects, expected) in table {
        let mut s = system();
        for e in effects {
            s.apply_effect(1, 1, e, 1, None).unwrap();
        }
        let v = value(&s, 1, "Stat");
        assert_eq!(v.current, expected, "{effects:?}");
        assert_eq!(v.base, 100.0, "duration modifiers leave the base alone");
    }
    // Removing the latest override falls back to the earlier one.
    let mut s = system();
    s.apply_effect(1, 1, "Over7", 1, None).unwrap();
    let nine = s.apply_effect(1, 1, "Over9", 1, None).unwrap().unwrap();
    assert!(s.remove_effect(1, nine));
    assert_eq!(value(&s, 1, "Stat").current, 7.0);
}

#[test]
fn clamping_by_value_and_by_attribute() {
    let mut s = system();
    s.apply_effect(1, 1, "BigDamage", 1, None).unwrap();
    assert_eq!(
        value(&s, 1, "Health"),
        AttributeValue {
            base: 0.0,
            current: 0.0
        }
    );
    s.apply_effect(1, 1, "BigHeal", 1, None).unwrap();
    assert_eq!(value(&s, 1, "Health").current, 100.0, "max is MaxHealth");
    let buff = s.apply_effect(1, 1, "MaxBuff", 1, None).unwrap().unwrap();
    assert_eq!(value(&s, 1, "MaxHealth").current, 150.0);
    s.apply_effect(1, 1, "Heal", 1, None).unwrap();
    assert_eq!(
        value(&s, 1, "Health"),
        AttributeValue {
            base: 130.0,
            current: 130.0
        }
    );
    s.remove_effect(1, buff);
    assert_eq!(
        value(&s, 1, "Health").current,
        100.0,
        "current re-clamps to the new max"
    );
    s.apply_effect(1, 1, "Haste", 1, None).unwrap();
    assert_eq!(value(&s, 1, "Speed").current, 10.0, "4 x 5 clamps to 10");
}

#[test]
fn instant_duration_and_infinite_effects() {
    let mut s = system();
    assert_eq!(s.apply_effect(1, 1, "Damage", 1, None), Ok(None));
    assert_eq!(
        value(&s, 1, "Health"),
        AttributeValue {
            base: 75.0,
            current: 75.0
        }
    );
    let slow = s.apply_effect(1, 1, "Slow", 1, None).unwrap().unwrap();
    assert_eq!(
        value(&s, 1, "Speed"),
        AttributeValue {
            base: 4.0,
            current: 2.0
        }
    );
    let mark = s.apply_effect(1, 1, "Mark", 1, None).unwrap().unwrap();
    assert!(s.has_tag(1, "Status.Marked", true));
    ticks(&mut s, 19, 0.1);
    assert_eq!(value(&s, 1, "Speed").current, 2.0, "still slowed at 1.9 s");
    s.drain_events();
    ticks(&mut s, 1, 0.1);
    assert_eq!(
        value(&s, 1, "Speed").current,
        4.0,
        "expired at exactly 2.0 s"
    );
    let events = s.drain_events();
    assert!(
        matches!(&events[0], GameplayEvent::EffectRemoved { handle, reason, .. }
        if *handle == slow && reason.as_str() == "expired")
    );
    assert!(
        matches!(&events[1], GameplayEvent::AttributeChanged { attribute, old, new, .. }
        if attribute == "Speed" && *old == 2.0 && *new == 4.0)
    );
    ticks(&mut s, 100, 0.1);
    assert_eq!(value(&s, 1, "Stat").current, 101.0, "infinite stays");
    assert!(s.remove_effect(1, mark));
    assert!(!s.has_tag(1, "Status", false));
    assert_eq!(value(&s, 1, "Stat").current, 100.0);
}

/// Executions of a periodic effect after `n` ticks of `dt`.
fn periodic(effect: &str, n: usize, dt: f32) -> f32 {
    let mut s = system();
    s.apply_effect(1, 1, effect, 1, None).unwrap();
    ticks(&mut s, n, dt);
    value(&s, 1, "Counter").base
}

#[test]
fn periodic_executions_are_exact_at_both_rates() {
    let sixtieth = 1.0f32 / 60.0;
    for (dt, per_second) in [(sixtieth, 60usize), (0.1, 10)] {
        // Duration 5 s, period 1 s: executions at 1..=5 (the last coincides with expiry).
        assert_eq!(periodic("Tick", per_second - 1, dt), 0.0, "dt {dt}");
        assert_eq!(periodic("Tick", per_second, dt), 1.0, "dt {dt}");
        assert_eq!(periodic("Tick", per_second * 5 - 1, dt), 4.0, "dt {dt}");
        assert_eq!(periodic("Tick", per_second * 5, dt), 5.0, "dt {dt}");
        assert_eq!(periodic("Tick", per_second * 12, dt), 5.0, "dt {dt}");
        // Execute on application adds the execution at t = 0.
        assert_eq!(periodic("TickNow", 0, dt), 1.0, "dt {dt}");
        assert_eq!(periodic("TickNow", per_second * 5, dt), 6.0, "dt {dt}");
        assert_eq!(periodic("TickNow", per_second * 12, dt), 6.0, "dt {dt}");
        // Infinite: one per second over 7.5 s and 60 s.
        assert_eq!(
            periodic("TickForever", per_second * 15 / 2, dt),
            7.0,
            "dt {dt}"
        );
        assert_eq!(
            periodic("TickForever", per_second * 60, dt),
            60.0,
            "dt {dt}"
        );
    }
}

#[test]
fn inhibited_periodic_effects_skip_and_reset_their_period() {
    let mut s = AbilitySystem::new(library());
    s.add_actor(1, &init(&[], &["State.Alive"])).unwrap();
    s.apply_effect(1, 1, "Regen", 1, None).unwrap();
    ticks(&mut s, 10, 0.1);
    assert_eq!(value(&s, 1, "Counter").base, 1.0);
    s.remove_loose_tag(1, "State.Alive");
    ticks(&mut s, 10, 0.1);
    assert_eq!(value(&s, 1, "Counter").base, 1.0, "inhibited");
    assert!(s.effects(1)[0].inhibited);
    s.add_loose_tag(1, "State.Alive").unwrap();
    ticks(&mut s, 9, 0.1);
    assert_eq!(
        value(&s, 1, "Counter").base,
        1.0,
        "period restarted on resume"
    );
    ticks(&mut s, 1, 0.1);
    assert_eq!(value(&s, 1, "Counter").base, 2.0);
}

#[test]
fn stacking_policies() {
    // By target: one stack whoever applies it, capped at 3.
    let mut s = system();
    for source in [1, 1, 2, 2, 2] {
        s.apply_effect(source, 1, "StackTarget", 1, None).unwrap();
    }
    let effects = s.effects(1);
    assert_eq!((effects.len(), effects[0].stacks), (1, 3));
    assert_eq!(value(&s, 1, "Stat").current, 106.0);
    // By source: one stack per source, capped at 2.
    let mut s = system();
    for source in [1, 1, 1, 2] {
        s.apply_effect(source, 1, "StackSource", 1, None).unwrap();
    }
    let stacks: Vec<u32> = s.effects(1).iter().map(|e| e.stacks).collect();
    assert_eq!(stacks, [2, 1]);
    assert_eq!(value(&s, 1, "Stat").current, 103.0);
    // None: separate effects.
    let mut s = system();
    for _ in 0..3 {
        s.apply_effect(1, 1, "StackNone", 1, None).unwrap();
    }
    assert_eq!(s.effects(1).len(), 3);
    assert_eq!(value(&s, 1, "Stat").current, 103.0);
    // Deny overflow.
    let mut s = system();
    s.apply_effect(1, 1, "Deny", 1, None).unwrap();
    assert_eq!(
        s.apply_effect(1, 1, "Deny", 1, None),
        Err(ApplyFailure::StackOverflow)
    );
}

#[test]
fn stack_duration_refresh_period_reset_and_expiry() {
    // Refresh on success: reapplied at 1.5 s, expires at 3.5 s.
    let mut s = system();
    s.apply_effect(1, 1, "Refresh", 1, None).unwrap();
    ticks(&mut s, 15, 0.1);
    s.apply_effect(1, 1, "Refresh", 1, None).unwrap();
    ticks(&mut s, 19, 0.1);
    assert_eq!(s.effects(1)[0].stacks, 2);
    ticks(&mut s, 1, 0.1);
    assert!(s.effects(1).is_empty(), "expired at 3.5 s");
    // Never refresh: expires at 2.0 s with both stacks.
    let mut s = system();
    s.apply_effect(1, 1, "NoRefresh", 1, None).unwrap();
    ticks(&mut s, 15, 0.1);
    s.apply_effect(1, 1, "NoRefresh", 1, None).unwrap();
    ticks(&mut s, 4, 0.1);
    assert_eq!(value(&s, 1, "Power").current, 2.0);
    ticks(&mut s, 1, 0.1);
    assert!(s.effects(1).is_empty(), "expired at 2.0 s");
    // Period reset: reapplied at 0.5 s, next execution at 1.5 s with 2 stacks.
    let mut s = system();
    s.apply_effect(1, 1, "PeriodReset", 1, None).unwrap();
    ticks(&mut s, 5, 0.1);
    s.apply_effect(1, 1, "PeriodReset", 1, None).unwrap();
    ticks(&mut s, 9, 0.1);
    assert_eq!(value(&s, 1, "Counter").base, 0.0, "no execution at 1.0 s");
    ticks(&mut s, 1, 0.1);
    assert_eq!(value(&s, 1, "Counter").base, 2.0, "1.5 s, two stacks");
    // Expiration policies with three stacks of a 1 s effect.
    for (effect, expected) in [
        ("ExpireAll", [0, 0, 0]),
        ("ExpireOne", [2, 1, 0]),
        ("ExpireRefresh", [3, 3, 3]),
    ] {
        let mut s = system();
        for _ in 0..3 {
            s.apply_effect(1, 1, effect, 1, None).unwrap();
        }
        let mut seen = [0u32; 3];
        for slot in &mut seen {
            ticks(&mut s, 10, 0.1);
            *slot = s.effects(1).first().map_or(0, |e| e.stacks);
        }
        assert_eq!(seen, expected, "{effect}");
        assert_eq!(
            value(&s, 1, "Power").current,
            expected[2] as f32,
            "{effect}"
        );
    }
}

#[test]
fn tag_requirements_block_and_allow() {
    let mut s = system();
    s.add_loose_tag(1, "State.Immune.Fire").unwrap();
    assert_eq!(
        s.apply_effect(2, 1, "Burn", 1, None),
        Err(ApplyFailure::ApplicationTags),
        "blocked by a descendant tag"
    );
    s.remove_loose_tag(1, "State.Immune.Fire");
    assert_eq!(s.apply_effect(2, 1, "Burn", 1, None), Ok(None));
    assert_eq!(value(&s, 1, "Health").current, 90.0);
    assert_eq!(
        s.apply_effect(2, 1, "NeedsWet", 1, None),
        Err(ApplyFailure::ApplicationTags)
    );
    s.add_loose_tag(1, "State.Wet").unwrap();
    assert_eq!(s.apply_effect(2, 1, "NeedsWet", 1, None), Ok(None));
    assert_eq!(value(&s, 1, "Health").current, 85.0);

    let mut rt = runtime();
    let mut host = Host::default();
    let gated = || ActivationRequest::new(1, "Gated");
    assert_eq!(
        rt.try_activate(&mut host, gated()),
        Err(FailReason::OwnerTags)
    );
    rt.system.add_loose_tag(1, "Status.Ready").unwrap();
    assert!(rt.try_activate(&mut host, gated()).is_ok());
    rt.system.add_loose_tag(1, "State.Stunned.Hard").unwrap();
    assert_eq!(
        rt.try_activate(&mut host, gated()),
        Err(FailReason::OwnerTags)
    );
}

type ActorState = (Vec<(String, AttributeValue)>, TagContainer, usize);

fn state(s: &AbilitySystem, actor: u64) -> ActorState {
    (
        s.attributes(actor),
        s.tags(actor).unwrap().clone(),
        s.effects(actor).len(),
    )
}

#[test]
fn a_failed_cost_leaves_everything_unchanged() {
    let mut rt = runtime();
    let mut host = Host::default();
    rt.system.set_base(1, "Mana", 10.0);
    let before = (state(&rt.system, 1), state(&rt.system, 2));
    rt.system.drain_events();
    let strike = ActivationRequest::new(1, "Strike").with_target(2);
    assert_eq!(rt.try_activate(&mut host, strike), Err(FailReason::Cost));
    assert_eq!(
        rt.try_activate(&mut host, ActivationRequest::new(1, "Expensive")),
        Err(FailReason::Cost)
    );
    assert_eq!((state(&rt.system, 1), state(&rt.system, 2)), before);
    let events = rt.system.drain_events();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| matches!(
        e,
        GameplayEvent::AbilityFailed {
            reason: FailReason::Cost,
            ..
        }
    )));
}

#[test]
fn cooldown_blocks_until_the_exact_expiry_tick() {
    for (dt, n) in [(0.1f32, 10usize), (1.0 / 60.0, 60)] {
        let mut rt = runtime();
        let mut host = Host::default();
        let strike = || ActivationRequest::new(1, "Strike").with_target(2);
        rt.try_activate(&mut host, strike()).unwrap();
        assert_eq!(value(&rt.system, 2, "Health").current, 75.0);
        assert!(rt.system.has_tag(1, "Cooldown.Strike", true));
        for tick in 1..n {
            rt.step(&mut host, dt);
            assert_eq!(
                rt.try_activate(&mut host, strike()),
                Err(FailReason::Cooldown),
                "dt {dt} tick {tick}"
            );
        }
        rt.step(&mut host, dt);
        assert!(
            rt.try_activate(&mut host, strike()).is_ok(),
            "dt {dt} tick {n}"
        );
        assert_eq!(value(&rt.system, 1, "Mana").current, 60.0, "two costs");
        assert_eq!(value(&rt.system, 2, "Health").current, 50.0);
    }
}

#[test]
fn abilities_cancel_and_block_by_tag() {
    let mut rt = runtime();
    let mut host = Host::default();
    let channel = rt
        .try_activate(&mut host, ActivationRequest::new(1, "Channel"))
        .unwrap();
    assert!(rt.system.has_tag(1, "State.Channeling", true));
    rt.system.drain_events();
    rt.try_activate(&mut host, ActivationRequest::new(1, "Dodge"))
        .unwrap();
    assert_eq!(
        rt.system.outcome(channel),
        Some(Outcome::Cancelled(CancelReason::CancelledByTags))
    );
    assert!(!rt.system.has_tag(1, "State.Channeling", true));
    let events = rt.system.drain_events();
    assert!(
        matches!(&events[0], GameplayEvent::AbilityCancelled { ability, .. } if ability == "Channel")
    );
    assert!(
        matches!(&events[1], GameplayEvent::AbilityActivated { ability, .. } if ability == "Dodge")
    );

    let guard = rt
        .try_activate(&mut host, ActivationRequest::new(1, "Guard"))
        .unwrap();
    let strike = || ActivationRequest::new(1, "Strike").with_target(2);
    assert_eq!(
        rt.try_activate(&mut host, strike()),
        Err(FailReason::Blocked)
    );
    assert_eq!(
        rt.try_activate(&mut host, ActivationRequest::new(1, "Guard")),
        Err(FailReason::AlreadyActive)
    );
    for _ in 0..10 {
        rt.step(&mut host, 0.1);
    }
    assert_eq!(rt.system.outcome(guard), Some(Outcome::Ended));
    assert!(rt.try_activate(&mut host, strike()).is_ok());
}

#[test]
fn attribute_based_magnitudes_snapshot_and_live() {
    for (effect, after) in [("Snapshot", 20.0), ("Live", 40.0)] {
        let mut s = system();
        s.apply_effect(1, 2, effect, 1, None).unwrap();
        assert_eq!(
            value(&s, 2, "Power").current,
            20.0,
            "{effect}: 2 x Strength 10"
        );
        s.set_base(1, "Strength", 20.0);
        s.tick_effects(0.1);
        assert_eq!(value(&s, 2, "Power").current, after, "{effect}");
    }
    let mut s = system();
    s.set_base(1, "Strength", 30.0);
    s.apply_effect(1, 2, "StrikeDamage", 1, None).unwrap();
    assert_eq!(value(&s, 2, "Health").base, 70.0);
}

#[test]
fn native_behaviours_triggers_and_instancing() {
    let mut rt = runtime();
    let mut host = Host::default();
    let first = rt
        .try_activate(&mut host, ActivationRequest::new(1, "Dash"))
        .unwrap();
    for _ in 0..3 {
        rt.step(&mut host, 0.1);
    }
    assert_eq!(rt.system.outcome(first), Some(Outcome::Ended));
    let second = rt
        .try_activate(&mut host, ActivationRequest::new(1, "Dash"))
        .unwrap();
    rt.system.cancel_instance(second);
    rt.step(&mut host, 0.1);
    assert_eq!(host.factories, 1, "per-actor behaviour is reused");
    assert_eq!(
        host.log,
        [
            format!("activate {}", first.0),
            format!("done {}", first.0),
            format!("end {} false", first.0),
            format!("activate {}", second.0),
            format!("end {} true", second.0),
        ]
    );
    let a = rt
        .try_activate(&mut host, ActivationRequest::new(1, "Spawned"))
        .unwrap();
    let b = rt
        .try_activate(&mut host, ActivationRequest::new(1, "Spawned"))
        .unwrap();
    assert_eq!(host.factories, 3, "per-execution builds one per activation");
    assert_eq!(rt.system.instances(1).len(), 2);
    for _ in 0..3 {
        rt.step(&mut host, 0.1);
    }
    assert_eq!(rt.system.outcome(a), Some(Outcome::Ended));
    assert_eq!(rt.system.outcome(b), Some(Outcome::Ended));

    rt.system.drain_events();
    let results = rt.send_event(&mut host, 1, "Event.Hit", Some(2), None);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "Reaction");
    let events = rt.system.drain_events();
    assert!(events.iter().any(|e| matches!(e,
        GameplayEvent::Custom { event, payload, .. } if event == "reacted" && payload["n"] == 1)));
}

#[test]
fn manual_commit_runs_at_its_step() {
    let mut rt = runtime();
    let mut host = Host::default();
    rt.try_activate(
        &mut host,
        ActivationRequest::new(1, "Manual").with_target(2),
    )
    .unwrap();
    assert_eq!(
        value(&rt.system, 1, "Mana").current,
        100.0,
        "not committed yet"
    );
    for _ in 0..5 {
        rt.step(&mut host, 0.1);
    }
    assert_eq!(value(&rt.system, 1, "Mana").current, 80.0);
    assert_eq!(value(&rt.system, 2, "Health").current, 75.0);
}

// ------------------------------------------------------------------ prediction

fn both(s: &AbilitySystem) -> (ActorState, ActorState) {
    (state(s, 1), state(s, 2))
}

#[test]
fn predicted_activation_confirmed_by_the_server_applies_exactly_once() {
    let mut client = runtime();
    let mut server = runtime();
    let mut host = Host::default();
    let mut ledger = PredictionLedger::new(LedgerConfig::default()).unwrap();
    let mut validator = KeyValidator::new(KeyValidatorConfig::default()).unwrap();

    let key = ledger.begin(0, None).unwrap();
    let request = ActivationRequest::new(1, "Strike").with_target(2);
    client
        .try_activate(&mut host, request.clone().with_key(key))
        .unwrap();
    // Predicted: costs and damage are stand-ins on current values; the cooldown is tagged.
    assert_eq!(
        value(&client.system, 1, "Mana"),
        AttributeValue {
            base: 100.0,
            current: 80.0
        }
    );
    assert_eq!(
        value(&client.system, 2, "Health"),
        AttributeValue {
            base: 100.0,
            current: 75.0
        }
    );
    assert!(client.system.effects(1).iter().all(|e| e.predicted));
    assert!(client.system.has_tag(1, "Cooldown.Strike", true));
    // A second predicted activation is refused by the predicted cooldown.
    assert_eq!(
        client.try_activate(&mut host, request.clone()),
        Err(FailReason::Cooldown)
    );

    validator.admit(key, None).unwrap();
    let accepted = server.try_activate(&mut host, request).is_ok();
    let verdicts = validator.resolve(key, accepted).unwrap();
    for verdict in verdicts {
        assert_eq!(
            ledger.apply(verdict, &mut client.system),
            KeyResolution::Committed(1)
        );
    }
    assert_eq!(
        value(&client.system, 1, "Mana"),
        AttributeValue {
            base: 80.0,
            current: 80.0
        }
    );
    assert_eq!(
        value(&client.system, 2, "Health"),
        AttributeValue {
            base: 75.0,
            current: 75.0
        }
    );
    let effects = client.system.effects(1);
    assert_eq!(effects.len(), 1, "only the cooldown remains");
    assert!(!effects[0].predicted);
    assert_eq!(client.system.pending_predictions(), 0);
    assert_eq!(
        client.system.attributes(1),
        server.system.attributes(1),
        "client matches the authoritative result"
    );
    assert_eq!(client.system.attributes(2), server.system.attributes(2));
    // The committed cooldown expires normally.
    for _ in 0..10 {
        client.step(&mut host, 0.1);
    }
    assert!(!client.system.has_tag(1, "Cooldown", false));
}

/// Applies a predicted Strike plus a predicted cleanse and a predicted stack on top of
/// authoritative state.
fn predict_everything(rt: &mut AbilityRuntime<Host>, ledger: &mut PredictionLedger) -> u32 {
    let mut host = Host::default();
    rt.system.apply_effect(1, 1, "Debuff", 1, None).unwrap();
    rt.system
        .apply_effect(1, 1, "StackTarget", 1, None)
        .unwrap();
    rt.system.add_loose_tag(2, "State.Wet").unwrap();
    let key = ledger.begin(0, None).unwrap();
    rt.try_activate(
        &mut host,
        ActivationRequest::new(1, "Strike")
            .with_target(2)
            .with_key(key),
    )
    .unwrap();
    rt.try_activate(
        &mut host,
        ActivationRequest::new(1, "Cleanser").with_key(key),
    )
    .unwrap();
    rt.try_activate(
        &mut host,
        ActivationRequest::new(1, "Stacker").with_key(key),
    )
    .unwrap();
    rt.system
        .apply_effect(1, 2, "NeedsWet", 1, Some(key))
        .unwrap();
    key.get()
}

#[test]
fn rejected_and_expired_predictions_restore_attributes_and_tags_exactly() {
    for expire in [false, true] {
        let mut rt = runtime();
        let config = LedgerConfig {
            timeout_ticks: 5,
            ..LedgerConfig::default()
        };
        let mut ledger = PredictionLedger::new(config).unwrap();
        // Reference: the authoritative part alone.
        let mut reference = runtime();
        reference
            .system
            .apply_effect(1, 1, "Debuff", 1, None)
            .unwrap();
        reference
            .system
            .apply_effect(1, 1, "StackTarget", 1, None)
            .unwrap();
        reference.system.add_loose_tag(2, "State.Wet").unwrap();
        let before = both(&reference.system);

        predict_everything(&mut rt, &mut ledger);
        assert_ne!(both(&rt.system), before);
        assert_eq!(
            value(&rt.system, 1, "Speed").current,
            4.0,
            "debuff cleansed"
        );
        assert_eq!(
            rt.system
                .effects(1)
                .iter()
                .find(|e| e.effect == "StackTarget")
                .unwrap()
                .stacks,
            2
        );
        if expire {
            assert_eq!(ledger.expire(4, &mut rt.system), 0);
            assert_eq!(ledger.expire(5, &mut rt.system), 1);
        } else {
            let key = engine_net_predict::PredictionKey::new(1).unwrap();
            assert_eq!(ledger.reject(key, &mut rt.system), KeyResolution::Undone(1));
        }
        assert_eq!(both(&rt.system), before, "expire {expire}");
        assert_eq!(
            value(&rt.system, 1, "Speed").current,
            3.0,
            "debuff restored"
        );
        assert_eq!(rt.system.pending_predictions(), 0);
        assert!(rt.system.instances(1).is_empty());
    }
}

#[test]
fn dependent_keys_undo_and_commit_with_their_parent() {
    // Reject the parent: the child is undone too, newest first.
    let mut rt = runtime();
    let mut host = Host::default();
    let mut ledger = PredictionLedger::new(LedgerConfig::default()).unwrap();
    let before = both(&rt.system);
    let dash = ledger.begin(0, None).unwrap();
    rt.try_activate(
        &mut host,
        ActivationRequest::new(1, "Strike")
            .with_target(2)
            .with_key(dash),
    )
    .unwrap();
    let hit = ledger.begin(0, Some(dash)).unwrap();
    rt.system
        .apply_effect(1, 2, "Damage", 1, Some(hit))
        .unwrap();
    assert_eq!(value(&rt.system, 2, "Health").current, 50.0);
    rt.system.drain_events();
    assert_eq!(
        ledger.reject(dash, &mut rt.system),
        KeyResolution::Undone(2)
    );
    assert_eq!(both(&rt.system), before);
    let removed: Vec<String> = rt
        .system
        .drain_events()
        .into_iter()
        .filter_map(|e| match e {
            GameplayEvent::EffectRemoved { effect, .. } => Some(effect),
            _ => None,
        })
        .collect();
    assert_eq!(
        removed,
        ["Damage", "Damage", "Cooldown", "ManaCost"],
        "child first"
    );

    // Confirm the child first: it waits for the parent, then both commit.
    let mut rt = runtime();
    let dash = ledger.begin(1, None).unwrap();
    rt.try_activate(
        &mut host,
        ActivationRequest::new(1, "Strike")
            .with_target(2)
            .with_key(dash),
    )
    .unwrap();
    let hit = ledger.begin(1, Some(dash)).unwrap();
    rt.system
        .apply_effect(1, 2, "Damage", 1, Some(hit))
        .unwrap();
    assert_eq!(ledger.confirm(hit, &mut rt.system), KeyResolution::Deferred);
    assert_eq!(value(&rt.system, 2, "Health").base, 100.0);
    assert_eq!(
        ledger.confirm(dash, &mut rt.system),
        KeyResolution::Committed(2)
    );
    assert_eq!(
        value(&rt.system, 2, "Health"),
        AttributeValue {
            base: 50.0,
            current: 50.0
        }
    );
    assert_eq!(value(&rt.system, 1, "Mana").base, 80.0);
    assert_eq!(rt.system.pending_predictions(), 0);
}

// ----------------------------------------------------------------- determinism

fn scenario() -> Vec<GameplayEvent> {
    let mut rt = runtime();
    let mut host = Host::default();
    let mut ledger = PredictionLedger::new(LedgerConfig::default()).unwrap();
    rt.system.apply_effect(2, 1, "TickNow", 1, None).unwrap();
    rt.system.apply_effect(2, 1, "ExpireOne", 1, None).unwrap();
    rt.system.apply_effect(2, 1, "ExpireOne", 1, None).unwrap();
    let mut trace = Vec::new();
    for tick in 0..120u64 {
        if tick % 15 == 0 {
            let _ = rt.try_activate(
                &mut host,
                ActivationRequest::new(1, "Strike").with_target(2),
            );
        }
        if tick == 30 {
            let key = ledger.begin(tick, None).unwrap();
            let _ = rt.try_activate(
                &mut host,
                ActivationRequest::new(2, "Strike")
                    .with_target(1)
                    .with_key(key),
            );
        }
        if tick == 40 {
            ledger.reject(
                engine_net_predict::PredictionKey::new(1).unwrap(),
                &mut rt.system,
            );
        }
        rt.step(&mut host, 1.0 / 60.0);
        trace.extend(rt.system.drain_events());
    }
    trace
}

#[test]
fn two_runs_produce_identical_event_traces() {
    let a = scenario();
    let b = scenario();
    assert!(a.len() > 30, "a rich trace: {}", a.len());
    assert_eq!(a, b);
}

#[test]
fn invalid_definitions_are_rejected_with_reasons() {
    let bad = [
        (json!({"version":2}), "version"),
        (
            json!({"version":1,"attribute_sets":[{"name":"S","attributes":[
            {"name":"A","base":1,"max":"B"},{"name":"B","base":1}]}]}),
            "earlier attribute",
        ),
        (
            json!({"version":1,"effects":[{"name":"E","policy":"duration"}]}),
            "duration > 0",
        ),
        (
            json!({"version":1,"effects":[{"name":"E","granted_tags":["X"]}]}),
            "instant effects",
        ),
        (
            json!({"version":1,"effects":[{"name":"E","policy":"infinite","granted_tags":["bad tag"]}]}),
            "invalid tag",
        ),
        (
            json!({"version":1,"abilities":[{"name":"A","cost":"Missing"}]}),
            "unknown effect",
        ),
        (
            json!({"version":1,"effects":[{"name":"C","policy":"infinite"}],
            "abilities":[{"name":"A","cooldown":"C"}]}),
            "cooldown effect",
        ),
        (
            json!({"version":1,"abilities":[{"name":"A","commit":"manual"}]}),
            "commit step",
        ),
    ];
    for (doc, needle) in bad {
        let err = Definitions::from_json(&serde_json::to_vec(&doc).unwrap()).unwrap_err();
        assert!(err.contains(needle), "{err} should mention {needle}");
    }
}
