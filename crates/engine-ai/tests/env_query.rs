//! Environment queries: generators, tests, scoring, selection and time slicing.
use engine_ai::{
    BbValue, Blackboard,
    env_query::{
        ActorSource, Item, Progress, QueryContext, QueryError, QueryHost, QueryResult, QueryRun,
        QueryTemplate,
    },
    perception::AffiliationFilter,
};
use engine_graph::{GraphVariable, PortType};
use serde_json::{Value, json};
use std::sync::Arc;

/// A world with a wall at x = 5 (blocks sight), no ground west of x = -1 (cannot be
/// projected or reached) and a fixed list of actors and smart object slots.
#[derive(Default)]
struct World {
    actors: Vec<Item>,
    slots: Vec<(Vec<String>, Item)>,
    los_calls: usize,
    path_calls: usize,
}

impl QueryHost for World {
    fn project(&mut self, p: [f32; 3], snap: f32) -> Option<[f32; 3]> {
        (p[0] >= -1.0 && snap > 0.0).then_some([p[0], 0.0, p[2]])
    }
    fn path_length(&mut self, from: [f32; 3], to: [f32; 3]) -> Option<f32> {
        self.path_calls += 1;
        if from[0] < -1.0 || to[0] < -1.0 {
            return None;
        }
        let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
        Some(1.5 * (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt())
    }
    fn line_of_sight(&mut self, from: [f32; 3], to: [f32; 3]) -> bool {
        self.los_calls += 1;
        !(from[0].min(to[0]) < 5.0 && from[0].max(to[0]) > 5.0)
    }
    fn actors(&mut self, _: ActorSource, _: AffiliationFilter, max: usize) -> Vec<Item> {
        self.actors.iter().copied().take(max).collect()
    }
    fn smart_objects(&mut self, tags: &[String], _: bool, max: usize) -> Vec<Item> {
        self.slots
            .iter()
            .filter(|(t, _)| tags.iter().all(|tag| t.contains(tag)))
            .map(|(_, item)| *item)
            .take(max)
            .collect()
    }
}

fn ctx() -> QueryContext {
    QueryContext {
        querier: [0.0, 0.0, 0.0],
        forward: [1.0, 0.0, 0.0],
        target: Some([20.0, 0.0, 0.0]),
        blackboard: Blackboard::default(),
    }
}

fn template(value: Value) -> Arc<QueryTemplate> {
    let mut doc = json!({"name":"q"});
    doc.as_object_mut()
        .unwrap()
        .extend(value.as_object().unwrap().clone());
    let template: QueryTemplate = serde_json::from_value(doc).expect("template parses");
    Arc::new(template)
}

fn run(value: Value) -> QueryResult {
    QueryRun::new(template(value), ctx())
        .expect("starts")
        .run(&mut World::default())
}

fn positions(result: &QueryResult) -> Vec<[f32; 3]> {
    result.items.iter().map(|s| s.item.position).collect()
}

fn near(a: [f32; 3], b: [f32; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4)
}

#[test]
fn circle_and_grid_generators_produce_ordered_points() {
    let all = run(json!({"generator":{"type":"circle","radius":10,"count":4},
        "select":{"top_n":16}}));
    assert_eq!(all.generated, 4);
    let p = positions(&all);
    assert!(near(p[0], [10.0, 0.0, 0.0]) && near(p[1], [0.0, 0.0, 10.0]));
    assert!(near(p[2], [-10.0, 0.0, 0.0]) && near(p[3], [0.0, 0.0, -10.0]));
    // Two rings of three, lifted 1.5 above the center of a target-centered circle.
    let rings = run(
        json!({"generator":{"type":"circle","center":"target","radius":6,
        "rings":2,"count":3,"height":1.5},"select":{"top_n":16}}),
    );
    assert_eq!(rings.generated, 6);
    let p = positions(&rings);
    assert!(near(p[0], [23.0, 1.5, 0.0]), "{:?}", p[0]);
    assert!(
        near(p[3], [26.0, 1.5, 0.0]),
        "outer ring starts after the inner"
    );
    // Grid: 5 x 5 points two apart, around a literal point.
    let grid = run(
        json!({"generator":{"type":"grid","center":[0,0,0],"half_extent":4,"spacing":2},
        "select":{"top_n":16}}),
    );
    assert_eq!(grid.generated, 25);
    let all = QueryRun::new(
        template(
            json!({"generator":{"type":"grid","half_extent":1,"spacing":1},"select":{"top_n":16}}),
        ),
        ctx(),
    )
    .unwrap()
    .run(&mut World::default());
    assert_eq!(all.items.len(), 9);
    assert!(near(all.items[0].item.position, [-1.0, 0.0, -1.0]));
    assert!(
        near(all.items[1].item.position, [-1.0, 0.0, 0.0]),
        "z runs fastest"
    );
    // Key references read the blackboard.
    let mut bb = Blackboard::from_variables(&[GraphVariable {
        name: "spot".into(),
        ty: PortType::Vec3,
        default: Some(json!([3.0, 0.0, 3.0])),
        description: String::new(),
    }])
    .unwrap();
    bb.take_changes();
    let context = QueryContext {
        blackboard: bb,
        ..ctx()
    };
    let keyed = QueryRun::new(
        template(
            json!({"generator":{"type":"circle","center":{"key":"spot"},"radius":1,"count":1}}),
        ),
        context,
    )
    .unwrap()
    .run(&mut World::default());
    assert!(near(keyed.items[0].item.position, [4.0, 0.0, 3.0]));
}

#[test]
fn distance_tests_filter_and_score_with_normalisation_and_weights() {
    let circle = json!({"type":"circle","radius":10,"count":4});
    // Filter by distance to the target at x = 20: only the (10, 0, 0) point is within 11.
    let filtered = run(json!({"generator":circle,"tests":[
        {"type":"distance","to":"target","max":11}]}));
    assert_eq!(filtered.items.len(), 1);
    assert!(near(filtered.items[0].item.position, [10.0, 0.0, 0.0]));
    assert_eq!((filtered.generated, filtered.rejected), (4, 3));
    // Score: closer to the target is better. Distances: 10, 22.4, 30, 22.4 -> best first.
    let ranked = run(json!({"generator":circle,"select":{"top_n":4},"tests":[
        {"type":"distance","to":"target","score":"lower_better"}]}));
    let order: Vec<u32> = ranked.items.iter().map(|s| s.index).collect();
    assert_eq!(
        (order[0], order[3]),
        (0, 2),
        "nearest first, farthest last: {order:?}"
    );
    assert_eq!(ranked.items[0].score, 1.0);
    assert_eq!(ranked.items[3].score, 0.0);
    // Two weighted score tests: near the target (weight 1) and far from the querier
    // (weight 3). Items: A (10,0,0) B (0,0,10) C (-10,0,0) D (0,0,-10).
    let weighted = run(json!({"generator":circle,"select":{"top_n":4},"tests":[
        {"type":"distance","to":"target","score":"lower_better","weight":1},
        {"type":"distance","to":"querier","score":"higher_better","weight":3}]}));
    // Distance to the querier is 10 for all, so that test is flat (normalised to 1);
    // the target test spans 10..30: A = (1*1 + 3*1)/4, C = (1*0 + 3*1)/4.
    let by_index = |i: u32| weighted.items.iter().find(|s| s.index == i).unwrap().score;
    assert!((by_index(0) - 1.0).abs() < 1e-6);
    assert!((by_index(2) - 0.75).abs() < 1e-6);
    assert!((by_index(1) - by_index(3)).abs() < 1e-6);
}

#[test]
fn dot_tests_select_by_facing() {
    let circle = json!({"type":"circle","radius":10,"count":8});
    // forward is +X: keep points at least 45 degrees in front.
    let front = run(json!({"generator":circle,"select":{"top_n":16},"tests":[
        {"type":"dot","direction":"forward","min":0.7}]}));
    let xs: Vec<f32> = front.items.iter().map(|s| s.item.position[0]).collect();
    assert_eq!(front.items.len(), 3);
    assert!(xs.iter().all(|x| *x > 6.9), "{xs:?}");
    // Best facing wins; behind-targets with `toward`: direction from the querier toward the target.
    let best = run(json!({"generator":circle,"tests":[
        {"type":"dot","direction":"target","score":"higher_better"}]}));
    assert!(near(best.items[0].item.position, [10.0, 0.0, 0.0]));
    let away = run(json!({"generator":circle,"tests":[
        {"type":"dot","direction":"target","score":"lower_better"}]}));
    assert!(near(away.items[0].item.position, [-10.0, 0.0, 0.0]));
}

#[test]
fn line_of_sight_reachability_and_path_length_use_the_host() {
    let circle = json!({"type":"circle","center":"target","radius":10,"count":4});
    // Points around the target (20, 0, 0): (30,0,0) (20,0,10) (10,0,0) (20,0,-10).
    // The wall at x = 5 blocks sight from the querier at x = 0 to none of them... but
    // blocks a viewer standing west of it: look from (0,0,0) toward x > 5 is blocked.
    let blocked = run(json!({"generator":circle,"select":{"top_n":16},"tests":[
        {"type":"line_of_sight"}]}));
    assert!(blocked.items.is_empty(), "every point lies behind the wall");
    let inverted = run(json!({"generator":circle,"select":{"top_n":16},"tests":[
        {"type":"line_of_sight","invert":true}]}));
    assert_eq!(inverted.items.len(), 4);
    // From a point east of the wall everything is visible.
    let clear = run(json!({"generator":circle,"select":{"top_n":16},"tests":[
        {"type":"line_of_sight","from":[8,0,0]}]}));
    assert_eq!(clear.items.len(), 4);
    // Reachable / path length: the west point (-10, 0, 0) cannot be reached.
    let around = json!({"type":"circle","radius":10,"count":4});
    let reachable = run(json!({"generator":around,"select":{"top_n":16},"tests":[
        {"type":"reachable"}]}));
    assert_eq!(reachable.items.len(), 3);
    let unreachable = run(json!({"generator":around,"select":{"top_n":16},"tests":[
        {"type":"reachable","invert":true}]}));
    assert_eq!(unreachable.items.len(), 1);
    assert!(near(unreachable.items[0].item.position, [-10.0, 0.0, 0.0]));
    let by_path = run(
        json!({"generator":{"type":"circle","center":"target","radius":10,"count":4},
        "select":{"top_n":16},"tests":[
        {"type":"path_length","from":"querier","max":35,"score":"lower_better"}]}),
    );
    assert!(by_path.items.len() == 3, "{:?}", positions(&by_path));
    assert!(
        near(by_path.items[0].item.position, [10.0, 0.0, 0.0]),
        "shortest path first"
    );
}

#[test]
fn cheap_tests_first_saves_the_expensive_ones() {
    let mut world = World::default();
    let tpl = template(
        json!({"generator":{"type":"circle","radius":10,"count":8},"tests":[
        {"type":"distance","to":"target","max":14},
        {"type":"line_of_sight","from":[8,0,0]},
        {"type":"path_length","score":"lower_better"}]}),
    );
    let result = QueryRun::new(tpl, ctx()).unwrap().run(&mut world);
    // Only the point within 14 of the target passes the distance test (the 45 degree points
    // are 14.7 away), so only it is line-checked and path-measured.
    assert_eq!(
        (result.items.len(), world.los_calls, world.path_calls),
        (1, 1, 1)
    );
}

#[test]
fn blackboard_gates_projection_and_selection() {
    let mut bb = Blackboard::from_variables(&[
        GraphVariable {
            name: "calm".into(),
            ty: PortType::Bool,
            default: Some(json!(false)),
            description: String::new(),
        },
        GraphVariable {
            name: "hp".into(),
            ty: PortType::Int,
            default: Some(json!(30)),
            description: String::new(),
        },
    ])
    .unwrap();
    bb.set("calm", BbValue::Bool(true)).unwrap();
    let with = |tests: Value| {
        let context = QueryContext {
            blackboard: bb.clone(),
            ..ctx()
        };
        QueryRun::new(
            template(json!({"generator":{"type":"circle","radius":3,"count":2},"tests":tests})),
            context,
        )
        .unwrap()
        .run(&mut World::default())
    };
    assert_eq!(
        with(json!([{"type":"blackboard","key":"calm","operator":"eq","value":true}]))
            .items
            .len(),
        1
    );
    assert!(
        with(json!([{"type":"blackboard","key":"calm","operator":"eq","value":false}]))
            .items
            .is_empty()
    );
    assert!(
        with(json!([{"type":"blackboard","key":"hp","operator":"gt","value":50}]))
            .items
            .is_empty()
    );
    assert_eq!(
        with(json!([{"type":"blackboard","key":"hp","operator":"ge","value":30}]))
            .items
            .len(),
        1
    );
    assert!(
        with(json!([{"type":"blackboard","key":"missing"}]))
            .items
            .is_empty()
    );
    // A closed gate costs no generation.
    assert_eq!(
        with(json!([{"type":"blackboard","key":"hp","operator":"lt","value":0}])).generated,
        0
    );
    // Projection drops points that cannot be snapped (west of x = 0) and moves the rest.
    let projected = run(json!({"generator":{"type":"circle","radius":10,"count":4},
        "project_to_navmesh":true,"select":{"top_n":16}}));
    assert_eq!(projected.items.len(), 3);
    assert_eq!(projected.rejected, 1);
    // top_n and min_score.
    let scored = |select: Value| {
        run(
            json!({"generator":{"type":"circle","radius":10,"count":4},"select":select,"tests":[
            {"type":"distance","to":"target","score":"lower_better"}]}),
        )
    };
    assert_eq!(scored(json!({"top_n":2})).items.len(), 2);
    assert_eq!(scored(json!({"top_n":16,"min_score":0.9})).items.len(), 1);
    // Without score tests every survivor scores 0 and generation order breaks the tie.
    let flat =
        run(json!({"generator":{"type":"circle","radius":10,"count":4},"select":{"top_n":2}}));
    let order: Vec<u32> = flat.items.iter().map(|s| s.index).collect();
    assert_eq!(order, [0, 1]);
    assert!(flat.items.iter().all(|s| s.score == 0.0));
}

#[test]
fn actors_and_smart_objects_generate_items_with_subjects() {
    let mut world = World {
        actors: vec![
            Item {
                position: [3.0, 0.0, 0.0],
                subject: Some(7),
                slot: None,
            },
            Item {
                position: [9.0, 0.0, 0.0],
                subject: Some(8),
                slot: None,
            },
        ],
        slots: vec![
            (
                vec!["sit".into()],
                Item {
                    position: [1.0, 0.0, 1.0],
                    subject: Some(40),
                    slot: Some(0),
                },
            ),
            (
                vec!["sit".into(), "wood".into()],
                Item {
                    position: [6.0, 0.0, 1.0],
                    subject: Some(41),
                    slot: Some(1),
                },
            ),
        ],
        ..World::default()
    };
    let nearest = QueryRun::new(
        template(
            json!({"generator":{"type":"actors","source":"perceived"},"tests":[
            {"type":"distance","score":"lower_better"}]}),
        ),
        ctx(),
    )
    .unwrap()
    .run(&mut world);
    assert_eq!(nearest.items[0].item.subject, Some(7));
    let seats = QueryRun::new(
        template(json!({"generator":{"type":"smart_objects","tags":["sit","wood"]},"select":{"top_n":4}})),
        ctx(),
    )
    .unwrap()
    .run(&mut world);
    assert_eq!(seats.items.len(), 1);
    assert_eq!(
        (seats.items[0].item.subject, seats.items[0].item.slot),
        (Some(41), Some(1))
    );
}

#[test]
fn a_query_spreads_over_steps_and_matches_the_unsliced_result() {
    let doc = json!({"generator":{"type":"grid","half_extent":8,"spacing":1},
        "project_to_navmesh":true,"select":{"top_n":3},"tests":[
        {"type":"distance","to":"target","max":30,"score":"lower_better"},
        {"type":"dot","direction":"forward","min":-0.5,"score":"higher_better","weight":0.5},
        {"type":"line_of_sight","from":[8,0,0]}]});
    let whole = QueryRun::new(template(doc.clone()), ctx())
        .unwrap()
        .run(&mut World::default());
    let mut sliced = QueryRun::new(template(doc), ctx()).unwrap();
    let mut host = World::default();
    let mut steps = 0;
    let result = loop {
        steps += 1;
        assert!(steps < 10_000);
        if let Progress::Done(result) = sliced.step(40, &mut host) {
            break result;
        }
    };
    assert!(
        steps > 5,
        "a budget of 40 units needs many steps, took {steps}"
    );
    assert_eq!(result.items, whole.items);
    assert_eq!(result.generated, 289);
    assert_eq!(result.steps as usize, steps);
    assert_eq!(result.cost, whole.cost);
}

#[test]
fn templates_are_validated_and_start_errors_are_reported() {
    let parse = |value: Value| serde_json::from_value::<QueryTemplate>(value);
    // Unknown fields and unknown types are rejected at parse time.
    assert!(parse(json!({"name":"q","generator":{"type":"circle","radius":1},"bogus":1})).is_err());
    assert!(parse(json!({"name":"q","generator":{"type":"blob"}})).is_err());
    assert!(parse(json!({"name":"q","generator":{"type":"circle","radius":1,"nope":1}})).is_err());
    // Bounds.
    let invalid = |value: Value| parse(value).unwrap().validate().is_err();
    let base =
        |generator: Value, tests: Value| json!({"name":"q","generator":generator,"tests":tests});
    let circle = json!({"type":"circle","radius":5});
    assert!(!invalid(base(circle.clone(), json!([]))));
    assert!(invalid(json!({"name":"bad name","generator":circle})));
    assert!(invalid(base(
        json!({"type":"circle","radius":0}),
        json!([])
    )));
    assert!(invalid(base(
        json!({"type":"circle","radius":5,"rings":100,"count":100}),
        json!([])
    )));
    assert!(invalid(base(
        json!({"type":"grid","half_extent":100,"spacing":1}),
        json!([])
    )));
    assert!(
        invalid(base(circle.clone(), json!([{"type":"distance"}]))),
        "a test that does nothing"
    );
    assert!(invalid(base(
        circle.clone(),
        json!([{"type":"distance","min":5,"max":1}])
    )));
    assert!(invalid(base(
        circle.clone(),
        json!([{"type":"dot","direction":"forward","min":2}])
    )));
    assert!(invalid(base(
        circle.clone(),
        json!([{"type":"distance","score":"lower_better","weight":-1}])
    )));
    assert!(invalid(base(
        circle.clone(),
        json!([{"type":"blackboard","key":"k","operator":"eq"}])
    )));
    let many: Vec<Value> = (0..9).map(|_| json!({"type":"reachable"})).collect();
    assert!(invalid(base(circle.clone(), Value::Array(many))));
    assert!(invalid(
        json!({"name":"q","generator":circle,"select":{"top_n":0}})
    ));
    assert!(invalid(
        json!({"name":"q","generator":circle,"select":{"top_n":99}})
    ));
    assert!(invalid(
        json!({"name":"q","generator":circle,"snap_distance":500})
    ));
    // Starting errors.
    let needs_target =
        template(json!({"generator":circle,"tests":[{"type":"distance","to":"target","max":1}]}));
    let no_target = QueryContext {
        target: None,
        ..ctx()
    };
    assert_eq!(
        QueryRun::new(needs_target, no_target).unwrap_err(),
        QueryError::MissingTarget
    );
    let needs_key =
        template(json!({"generator":{"type":"circle","center":{"key":"nope"},"radius":1}}));
    assert_eq!(
        QueryRun::new(needs_key, ctx()).unwrap_err(),
        QueryError::MissingKey("nope".into())
    );
    assert!(matches!(
        QueryRun::new(
            Arc::new(serde_json::from_value(json!({"name":"x y","generator":circle})).unwrap()),
            ctx()
        ),
        Err(QueryError::Invalid(_))
    ));
    // The template reports what it needs.
    let t = template(
        json!({"generator":circle,"project_to_navmesh":true,"tests":[
        {"type":"dot","direction":"target","min":0.5}]}),
    );
    assert!(t.needs_target() && t.needs_navmesh());
    assert!(!template(json!({"generator":circle})).needs_target());
}

#[test]
fn two_runs_give_identical_results() {
    let doc = json!({"generator":{"type":"circle","radius":9,"rings":3,"count":24},
        "select":{"top_n":5},"tests":[
        {"type":"distance","to":"target","score":"lower_better"},
        {"type":"dot","direction":"forward","score":"higher_better"}]});
    assert_eq!(run(doc.clone()), run(doc));
}
