use engine_animation::{graph::*, *};
use engine_graph::GraphDocument;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
fn document(ik: bool) -> GraphDocument {
    let mut nodes = vec![
        json!({"id":"foreign","node_type":"retarget_clip","params":{"clip":"foreign_clip","source_rig":"source","target_rig":"target","mapping":{"joints":[{"source":0,"target":0},{"source":1,"target":1}]}}}),
        json!({"id":"blend","node_type":"blend","params":{"parameter":"blend"}}),
        json!({"id":"out","node_type":"output"}),
    ];
    let mut edges = vec![edge("foreign", "blend", "a"), edge("foreign", "blend", "b")];
    let mut variables = vec![json!({"name":"blend","type":"float","default":0.5})];
    if ik {
        nodes.push(json!({"id":"ik","node_type":"ik_two_bone","params":{"root":0,"mid":1,"end":2,"target":"target_point","pole":"pole","weight":"ik_weight"}}));
        edges.extend([edge("blend", "ik", "input"), edge("ik", "out", "pose")]);
        variables.extend([
            json!({"name":"target_point","type":"vec3","default":[2,2,0]}),
            json!({"name":"pole","type":"vec3","default":[0,3,0]}),
            json!({"name":"ik_weight","type":"float","default":1}),
        ]);
    } else {
        edges.push(edge("blend", "out", "pose"));
    }
    GraphDocument::from_value(
        json!({"version":1,"kind":"animation","nodes":nodes,"edges":edges,"variables":variables}),
    )
    .unwrap()
}
fn edge(a: &str, b: &str, port: &str) -> Value {
    json!({"from":{"node":a,"port":"pose"},"to":{"node":b,"port":port}})
}
fn rig(lengths: &[f32]) -> Arc<Skeleton> {
    let mut rest = vec![LocalPose::default()];
    rest.extend(lengths.iter().map(|length| LocalPose {
        translation: Vec3::X * length,
        ..Default::default()
    }));
    Arc::new(
        Skeleton::new(
            (0..rest.len()).map(|i| i.checked_sub(1)).collect(),
            vec![Mat4::IDENTITY; rest.len()],
            rest,
        )
        .unwrap(),
    )
}
fn clip() -> Arc<Clip> {
    Arc::new(
        Clip::new(
            1.,
            2,
            vec![Track {
                joint: 0,
                translation: None,
                rotation: Some(
                    RotationTrack::new(
                        vec![0., 1.],
                        vec![
                            Quat::IDENTITY,
                            Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
                        ],
                        Interpolation::Linear,
                    )
                    .unwrap(),
                ),
                scale: None,
            }],
            vec![ClipEvent {
                time: 0.25,
                name: "foot".into(),
            }],
        )
        .unwrap(),
    )
}
#[test]
fn source_sized_clip_becomes_target_pose_and_shared_branch_events_emit_once() {
    let graph = Arc::new(Graph::compile(&document(false)).unwrap());
    let target = rig(&[2., 2.]);
    let source = rig(&[1.]);
    let clips = BTreeMap::from([("foreign_clip".into(), clip())]);
    let dependencies: Vec<_> = graph
        .asset_dependencies()
        .into_iter()
        .map(|d| (d.id.to_owned(), d.kind))
        .collect();
    assert_eq!(
        dependencies,
        vec![
            ("foreign_clip".into(), GraphAssetKind::Animation),
            ("source".into(), GraphAssetKind::Model),
            ("target".into(), GraphAssetKind::Model)
        ]
    );
    assert!(
        GraphPlayer::with_skeleton(
            graph.clone(),
            target.clone(),
            clips.clone(),
            &BTreeMap::new()
        )
        .is_err()
    );
    let rigs = GraphRigs::new(
        "target".into(),
        target.clone(),
        BTreeMap::from([("source".into(), source)]),
    )
    .unwrap();
    graph.validate_retarget_bindings(&rigs, &clips).unwrap();
    let mut player = GraphPlayer::with_rigs(graph, rigs, clips, &BTreeMap::new()).unwrap();
    let events = player.update(0.25).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].clip, "foreign_clip");
    assert_eq!(player.pose().len(), 3);
    let end = target.global_matrices(player.pose()).unwrap()[2]
        .w_axis
        .truncate();
    let expected = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2 * 0.25) * (Vec3::X * 4.);
    assert!(end.distance(expected) < 3e-4);
}
#[test]
fn retarget_output_blends_and_runs_downstream_ik_in_target_space() {
    let graph = Arc::new(Graph::compile(&document(true)).unwrap());
    let target = rig(&[2., 2.]);
    let rigs = GraphRigs::new(
        "target".into(),
        target.clone(),
        BTreeMap::from([("source".into(), rig(&[1.]))]),
    )
    .unwrap();
    let mut player = GraphPlayer::with_rigs(
        graph,
        rigs,
        BTreeMap::from([("foreign_clip".into(), clip())]),
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(player.update(0.25).unwrap().len(), 1);
    let end = target.global_matrices(player.pose()).unwrap()[2]
        .w_axis
        .truncate();
    assert!(end.distance(Vec3::new(2., 2., 0.)) < 3e-4);
}
#[test]
fn explicit_rig_labels_clip_counts_and_map_shape_are_validated_before_evaluation() {
    let graph = Arc::new(Graph::compile(&document(false)).unwrap());
    let target = rig(&[2., 2.]);
    let clips = BTreeMap::from([("foreign_clip".into(), clip())]);
    let missing = GraphRigs::new("target".into(), target.clone(), BTreeMap::new()).unwrap();
    assert!(
        GraphPlayer::with_rigs(graph.clone(), missing, clips.clone(), &BTreeMap::new()).is_err()
    );
    let wrong = GraphRigs::new(
        "other".into(),
        target.clone(),
        BTreeMap::from([("source".into(), rig(&[1.]))]),
    )
    .unwrap();
    assert!(graph.validate_retarget_bindings(&wrong, &clips).is_err());
    let wrong_count = GraphRigs::new(
        "target".into(),
        target,
        BTreeMap::from([("source".into(), rig(&[1., 1.]))]),
    )
    .unwrap();
    assert!(
        graph
            .validate_retarget_bindings(&wrong_count, &clips)
            .is_err()
    );
    let mut bad = document(false);
    bad.nodes[0].params.get_mut("mapping").unwrap()["joints"][0]["basis"] = json!([0, 0, 0, 0]);
    assert_eq!(
        Graph::compile(&bad).unwrap_err()[0].code,
        "animation.graph.invalid"
    );
}
