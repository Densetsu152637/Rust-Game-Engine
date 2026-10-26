use engine_animation::{graph::*, *};
use engine_graph::GraphDocument;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
fn doc(nodes: Value, edges: Value, variables: Value) -> GraphDocument {
    GraphDocument::from_value(
        json!({"version":1,"kind":"animation","nodes":nodes,"edges":edges,"variables":variables}),
    )
    .unwrap()
}
fn edge(source: &str, target: &str, port: &str) -> Value {
    json!({"from":{"node":source,"port":"pose"},"to":{"node":target,"port":port}})
}
fn clip(value: f32) -> Arc<Clip> {
    Arc::new(
        Clip::new(
            1.,
            1,
            vec![Track {
                joint: 0,
                translation: Some(
                    VectorTrack::new(
                        vec![0., 1.],
                        vec![Vec3::X * value, Vec3::X * (value + 1.)],
                        Interpolation::Linear,
                    )
                    .unwrap(),
                ),
                rotation: None,
                scale: None,
            }],
            vec![ClipEvent {
                time: 0.25,
                name: format!("event{value}"),
            }],
        )
        .unwrap(),
    )
}
fn new_player(document: &GraphDocument) -> GraphPlayer {
    GraphPlayer::new(
        Arc::new(Graph::compile(document).unwrap()),
        vec![LocalPose::default()],
        BTreeMap::from([
            ("a".into(), clip(0.)),
            ("b".into(), clip(10.)),
            ("c".into(), clip(20.)),
        ]),
    )
    .unwrap()
}
fn approx(a: f32, b: f32) {
    assert!((a - b).abs() < 1e-4, "{a} != {b}");
}
#[test]
fn spaces_interpolate_boundaries_clamp_and_reject_degenerate_authored_data() {
    let space = BlendSpace1D::new(vec![-f32::MAX, 0., f32::MAX]).unwrap();
    assert_eq!(space.weights(-f32::MAX).unwrap(), vec![1., 0., 0.]);
    assert_eq!(space.weights(f32::MAX).unwrap(), vec![0., 0., 1.]);
    assert_eq!(space.weights(0.).unwrap(), vec![0., 1., 0.]);
    assert!(space.weights(f32::NAN).is_err());
    assert!(BlendSpace1D::new(vec![0., 0.]).is_err());
    assert!(BlendSpace1D::new(vec![1., 0.]).is_err());
    let space = BlendSpace2D::new(vec![[0., 0.], [1., 0.], [0., 1.]], vec![[0, 1, 2]]).unwrap();
    assert_eq!(space.weights([0., 0.]).unwrap(), vec![1., 0., 0.]);
    assert_eq!(space.weights([0.25, 0.5]).unwrap(), vec![0.25, 0.25, 0.5]);
    assert_eq!(space.weights([2., 2.]).unwrap(), vec![0., 0.5, 0.5]);
    assert_eq!(space.weights([-2., 0.]).unwrap(), vec![1., 0., 0.]);
    assert!(BlendSpace2D::new(vec![[0., 0.], [1., 0.], [2., 0.]], vec![[0, 1, 2]]).is_err());
    assert!(BlendSpace2D::new(vec![[0., 0.], [0., 0.], [1., 1.]], vec![[0, 1, 2]]).is_err());
    assert!(BlendSpace2D::new(vec![[0., 0.], [1., 0.], [0., 1.]], vec![[0, 1, 3]]).is_err());
    assert!(
        BlendSpace2D::new(
            vec![[0., 0.], [1., 0.], [0., 1.]],
            vec![[0, 1, 2], [2, 1, 0]]
        )
        .is_err()
    );
}
#[test]
fn graph_blend_spaces_drive_real_pose_and_only_contributing_clip_events() {
    let mut document = doc(
        json!([
            {"id":"a","node_type":"clip","params":{"clip":"a"}},
            {"id":"b","node_type":"clip","params":{"clip":"b"}},
            {"id":"c","node_type":"clip","params":{"clip":"c"}},
            {"id":"mix","node_type":"blend2d","params":{"x":"x","y":"y","positions":[[0,0],[1,0],[0,1]],"triangles":[[0,1,2]]}},
            {"id":"out","node_type":"output"}
        ]),
        json!([
            edge("a", "mix", "samples"),
            edge("b", "mix", "samples"),
            edge("c", "mix", "samples"),
            edge("mix", "out", "pose")
        ]),
        json!([{"name":"x","type":"float","default":0.25},{"name":"y","type":"float","default":0.5}]),
    );
    let mut player = new_player(&document);
    approx(player.pose()[0].translation.x, 12.5);
    let events = player.update(0.25).unwrap();
    assert_eq!(events.len(), 3);
    approx(player.pose()[0].translation.x, 12.75);
    player
        .set_parameters(&BTreeMap::from([
            ("x".into(), json!(1.)),
            ("y".into(), json!(0.)),
        ]))
        .unwrap();
    player.update(0.).unwrap();
    approx(player.pose()[0].translation.x, 10.25);
    let defaults = player.parameters().clone();
    assert!(
        player
            .set_parameters(&BTreeMap::from([("x".into(), json!(true))]))
            .is_err()
    );
    assert_eq!(player.parameters(), &defaults);
    assert!(
        player
            .set_parameters(&BTreeMap::from([("missing".into(), json!(1.))]))
            .is_err()
    );
    document.nodes.retain(|n| n.id.as_str() != "c");
    document.edges.retain(|e| e.from.node.as_str() != "c");
    document.nodes[2].node_type = "blend1d".into();
    document.nodes[2].params =
        serde_json::from_value(json!({"parameter":"x","positions":[0,2]})).unwrap();
    let mut one = new_player(&document);
    approx(one.pose()[0].translation.x, 1.25);
    one.set_parameters(&BTreeMap::from([("x".into(), json!(0.))]))
        .unwrap();
    let events = one.update(0.25).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].clip, "a");
}
#[test]
fn machine_prioritizes_authored_transitions_and_interrupts_without_pose_jumps() {
    let document = doc(
        json!([
            {"id":"a","node_type":"clip","params":{"clip":"a","speed":0}},
            {"id":"b","node_type":"clip","params":{"clip":"b","speed":0}},
            {"id":"c","node_type":"clip","params":{"clip":"c","speed":0}},
            {"id":"machine","node_type":"state_machine","params":{"states":["idle","walk","run"],"initial":"idle","transitions":[
                {"from":"idle","to":"walk","parameter":"go","op":"eq","value":true,"duration":1},
                {"from":"idle","to":"run","parameter":"go","op":"eq","value":true,"duration":0},
                {"from":"walk","to":"idle","parameter":"go","op":"eq","value":false,"duration":1}]}},
            {"id":"out","node_type":"output"}
        ]),
        json!([
            edge("a", "machine", "states"),
            edge("b", "machine", "states"),
            edge("c", "machine", "states"),
            edge("machine", "out", "pose")
        ]),
        json!([{"name":"go","type":"bool","default":false}]),
    );
    let mut player = new_player(&document);
    approx(player.pose()[0].translation.x, 0.);
    player
        .set_parameters(&BTreeMap::from([("go".into(), json!(true))]))
        .unwrap();
    player.update(0.).unwrap();
    approx(player.pose()[0].translation.x, 0.);
    player.update(0.25).unwrap();
    approx(player.pose()[0].translation.x, 2.5);
    player.set_parameters(&BTreeMap::new()).unwrap();
    player.update(0.).unwrap();
    approx(player.pose()[0].translation.x, 2.5);
    player.update(0.25).unwrap();
    approx(player.pose()[0].translation.x, 1.875);
    player.update(1.).unwrap();
    approx(player.pose()[0].translation.x, 0.);
}
#[test]
fn compiler_and_shared_validator_reject_bad_topology_and_semantics() {
    let mut document = doc(
        json!([{"id":"a","node_type":"clip","params":{"clip":"a"}},{"id":"out","node_type":"output"}]),
        json!([edge("a", "out", "pose")]),
        json!([]),
    );
    assert!(Graph::compile(&document).is_ok());
    document.nodes.push(document.nodes[0].clone());
    assert!(
        !engine_graph::validate(&document, &kind(), engine_graph::Strictness::Complete).is_empty()
    );
    document.nodes.pop();
    document.nodes[0].params.insert("clip".into(), json!(""));
    let errors = engine_graph::validate(&document, &kind(), engine_graph::Strictness::Complete);
    assert_eq!(errors[0].code, "animation.graph.invalid");
    document.nodes[0].params.insert("clip".into(), json!("a"));
    document.edges.clear();
    assert!(Graph::compile(&document).is_err());
    document
        .edges
        .push(serde_json::from_value(edge("a", "out", "pose")).unwrap());
    document.nodes.push(
        serde_json::from_value(json!({"id":"dead","node_type":"clip","params":{"clip":"b"}}))
            .unwrap(),
    );
    assert!(
        Graph::compile(&document).unwrap_err()[0]
            .message
            .contains("contribute")
    );
}
#[test]
fn invalid_clock_and_event_overflow_leave_pose_and_time_unchanged() {
    let document = doc(
        json!([{"id":"a","node_type":"clip","params":{"clip":"a"}},{"id":"out","node_type":"output"}]),
        json!([edge("a", "out", "pose")]),
        json!([]),
    );
    let tiny = Arc::new(
        Clip::new(
            0.001,
            1,
            vec![],
            vec![ClipEvent {
                time: 0.,
                name: "loop".into(),
            }],
        )
        .unwrap(),
    );
    let mut player = GraphPlayer::new(
        Arc::new(Graph::compile(&document).unwrap()),
        vec![LocalPose::default()],
        BTreeMap::from([("a".into(), tiny)]),
    )
    .unwrap();
    assert!(player.update(60.).is_err());
    assert_eq!(player.pose(), &[LocalPose::default()]);
    assert!(player.update(f32::NAN).is_err());
    assert_eq!(player.update(0.001).unwrap().len(), 1);
}
#[test]
fn shared_pose_sources_blend_once_and_cycles_are_rejected() {
    let mut document = doc(
        json!([
            {"id":"a","node_type":"clip","params":{"clip":"a"}},
            {"id":"mix","node_type":"blend","params":{"parameter":"weight"}},
            {"id":"out","node_type":"output"}
        ]),
        json!([
            edge("a", "mix", "a"),
            edge("a", "mix", "b"),
            edge("mix", "out", "pose")
        ]),
        json!([{"name":"weight","type":"float","default":0.5}]),
    );
    let mut player = new_player(&document);
    assert_eq!(player.update(0.25).unwrap().len(), 1);
    approx(player.pose()[0].translation.x, 0.25);
    document.edges[0] = serde_json::from_value(edge("mix", "mix", "a")).unwrap();
    assert!(
        Graph::compile(&document)
            .unwrap_err()
            .iter()
            .any(|d| d.code == "graph.cycle")
    );
}
#[test]
fn malformed_machine_conditions_and_huge_float_parameters_are_diagnostics() {
    let mut document = doc(
        json!([
            {"id":"a","node_type":"clip","params":{"clip":"a"}},
            {"id":"b","node_type":"clip","params":{"clip":"b"}},
            {"id":"machine","node_type":"state_machine","params":{"states":["idle","walk"],"initial":"idle","transitions":[{"from":"idle","to":"walk","parameter":"go","op":"gt","value":true,"duration":1}]}},
            {"id":"out","node_type":"output"}
        ]),
        json!([
            edge("a", "machine", "states"),
            edge("b", "machine", "states"),
            edge("machine", "out", "pose")
        ]),
        json!([{"name":"go","type":"bool","default":false}]),
    );
    assert_eq!(
        Graph::compile(&document).unwrap_err()[0].code,
        "animation.graph.invalid"
    );
    document.nodes[2].params.get_mut("transitions").unwrap()[0]["op"] = json!("eq");
    assert!(Graph::compile(&document).is_ok());
    document.variables.push(
        serde_json::from_value(json!({"name":"huge","type":"float","default":1e100})).unwrap(),
    );
    assert_eq!(
        Graph::compile(&document).unwrap_err()[0].path,
        "/variables/huge"
    );
}
#[test]
fn ik_graph_requires_an_explicit_rig_and_rejects_bad_controls_atomically() {
    let document = doc(
        json!([
            {"id":"clip","node_type":"clip","params":{"clip":"rest"}},
            {"id":"ik","node_type":"ik_two_bone","params":{"root":0,"mid":1,"end":2,"target":"target","pole":"pole","weight":"weight"}},
            {"id":"out","node_type":"output"}
        ]),
        json!([edge("clip", "ik", "input"), edge("ik", "out", "pose")]),
        json!([
            {"name":"target","type":"vec3","default":[1,1,0]},
            {"name":"pole","type":"vec3","default":[0,2,0]},
            {"name":"weight","type":"float","default":0}
        ]),
    );
    let graph = Arc::new(Graph::compile(&document).unwrap());
    assert!(graph.requires_skeleton());
    let rest = vec![
        LocalPose::default(),
        LocalPose {
            translation: Vec3::X,
            ..Default::default()
        },
        LocalPose {
            translation: Vec3::X,
            ..Default::default()
        },
    ];
    let skeleton = Arc::new(
        Skeleton::new(
            vec![None, Some(0), Some(1)],
            vec![Mat4::IDENTITY; 3],
            rest.clone(),
        )
        .unwrap(),
    );
    let clip = Arc::new(
        Clip::new(
            1.,
            3,
            vec![Track {
                joint: 0,
                translation: Some(
                    VectorTrack::new(
                        vec![0., 1.],
                        vec![Vec3::ZERO, Vec3::X],
                        Interpolation::Linear,
                    )
                    .unwrap(),
                ),
                rotation: None,
                scale: None,
            }],
            vec![],
        )
        .unwrap(),
    );
    let clips = BTreeMap::from([("rest".into(), clip)]);
    assert!(GraphPlayer::new(graph.clone(), rest, clips.clone()).is_err());
    let mut player =
        GraphPlayer::with_skeleton(graph, skeleton.clone(), clips, &BTreeMap::new()).unwrap();
    player
        .set_parameters(&BTreeMap::from([("weight".into(), json!(1.))]))
        .unwrap();
    player.update(0.).unwrap();
    let globals = skeleton.global_matrices(player.pose()).unwrap();
    assert!(
        globals[2]
            .transform_point3(Vec3::ZERO)
            .distance(Vec3::new(1., 1., 0.))
            < 3e-4
    );
    let previous = player.parameters().clone();
    for overrides in [
        BTreeMap::from([("weight".into(), json!(2.))]),
        BTreeMap::from([("target".into(), json!([1, 2]))]),
        BTreeMap::from([("target".into(), json!([1, 2, null]))]),
        BTreeMap::from([("target".into(), json!([1e100, 0, 0]))]),
    ] {
        assert!(player.set_parameters(&overrides).is_err());
        assert_eq!(player.parameters(), &previous);
    }
    player.set_parameters(&BTreeMap::new()).unwrap();
    player.update(0.1).unwrap();
    let before = player.pose().to_vec();
    player
        .set_parameters(&BTreeMap::from([
            ("weight".into(), json!(1.)),
            ("target".into(), json!([1, 0, 0])),
            ("pole".into(), json!([2, 0, 0])),
        ]))
        .unwrap();
    assert!(player.update(0.1).is_err());
    assert_eq!(player.pose(), before);
    player.set_parameters(&BTreeMap::new()).unwrap();
    player.update(0.1).unwrap();
    assert!((player.pose()[0].translation.x - 0.2).abs() < 1e-5);
}
#[test]
fn ik_construction_evaluates_effective_initial_controls_not_unused_defaults() {
    let document = doc(
        json!([
            {"id":"clip","node_type":"clip","params":{"clip":"rest"}},
            {"id":"ik","node_type":"ik_two_bone","params":{"root":0,"mid":1,"end":2,"target":"target","pole":"pole","weight":"weight"}},
            {"id":"out","node_type":"output"}
        ]),
        json!([edge("clip", "ik", "input"), edge("ik", "out", "pose")]),
        json!([
            {"name":"target","type":"vec3"},{"name":"pole","type":"vec3"},{"name":"weight","type":"float","default":1}
        ]),
    );
    let graph = Arc::new(Graph::compile(&document).unwrap());
    let skeleton = Arc::new(
        Skeleton::new(
            vec![None, Some(0), Some(1)],
            vec![Mat4::IDENTITY; 3],
            vec![
                LocalPose::default(),
                LocalPose {
                    translation: Vec3::X,
                    ..Default::default()
                },
                LocalPose {
                    translation: Vec3::X,
                    ..Default::default()
                },
            ],
        )
        .unwrap(),
    );
    let clips = BTreeMap::from([(
        "rest".into(),
        Arc::new(Clip::new(1., 3, vec![], vec![]).unwrap()),
    )]);
    assert!(
        GraphPlayer::with_skeleton(
            graph.clone(),
            skeleton.clone(),
            clips.clone(),
            &BTreeMap::new()
        )
        .is_err()
    );
    let mut parameters = BTreeMap::from([
        ("target".into(), json!([1, 1, 0])),
        ("pole".into(), json!([0, 2, 0])),
    ]);
    let player =
        GraphPlayer::with_skeleton(graph.clone(), skeleton.clone(), clips.clone(), &parameters)
            .unwrap();
    assert!(
        skeleton.global_matrices(player.pose()).unwrap()[2]
            .transform_point3(Vec3::ZERO)
            .distance(Vec3::new(1., 1., 0.))
            < 3e-4
    );
    parameters.insert("pole".into(), json!([2, 2, 0]));
    assert!(GraphPlayer::with_skeleton(graph, skeleton, clips, &parameters).is_err());
}
#[test]
fn flat_graph_initial_overrides_are_applied_before_pose_validation() {
    let document = doc(
        json!([
            {"id":"a","node_type":"clip","params":{"clip":"a"}},
            {"id":"b","node_type":"clip","params":{"clip":"b"}},
            {"id":"mix","node_type":"blend","params":{"parameter":"weight"}},
            {"id":"out","node_type":"output"}
        ]),
        json!([
            edge("a", "mix", "a"),
            edge("b", "mix", "b"),
            edge("mix", "out", "pose")
        ]),
        json!([{"name":"weight","type":"float","default":0.5}]),
    );
    let graph = Arc::new(Graph::compile(&document).unwrap());
    let clips = [("a", Vec3::ONE), ("b", Vec3::NEG_ONE)]
        .into_iter()
        .map(|(id, scale)| {
            (
                id.to_owned(),
                Arc::new(
                    Clip::new(
                        1.,
                        1,
                        vec![Track {
                            joint: 0,
                            translation: None,
                            rotation: None,
                            scale: Some(
                                VectorTrack::new(vec![0.], vec![scale], Interpolation::Linear)
                                    .unwrap(),
                            ),
                        }],
                        vec![],
                    )
                    .unwrap(),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert!(GraphPlayer::new(graph.clone(), vec![LocalPose::default()], clips.clone()).is_err());
    let player = GraphPlayer::with_parameters(
        graph,
        vec![LocalPose::default()],
        clips,
        &BTreeMap::from([("weight".into(), json!(0.))]),
    )
    .unwrap();
    assert_eq!(player.pose()[0].scale, Vec3::ONE);
}
