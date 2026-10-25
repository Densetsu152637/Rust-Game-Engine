//! Behaviour tests for the graph model, validation, executor and the built-in node library.
use engine_pcg::{
    Aabb, Data, Diagnostic, EditOp, FlatSurface, FnSurface, Graph, GraphBuilder, InstanceKind,
    Limits, NodeDefinition, NodeError, Outputs, ParamSpec, PortRef, PortSpec, PortType, Registry,
    RunSettings, Surface, apply_edit, run, validate,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn registry() -> Registry {
    Registry::builtin()
}

fn settings(seed: u64, half: f32) -> RunSettings {
    RunSettings::new(seed, Aabb::new([-half, -10.0, -half], [half, 10.0, half]))
}

fn codes(diagnostics: &[Diagnostic]) -> Vec<&str> {
    diagnostics.iter().map(|d| d.code.as_str()).collect()
}

fn entries(assets: &[(&str, f64)]) -> Value {
    Value::Array(
        assets
            .iter()
            .map(|(a, w)| json!({"asset": a, "kind": "mesh", "weight": w}))
            .collect(),
    )
}

fn points_of(graph: &Graph, surface: &dyn Surface, s: &RunSettings, node: &str) -> Vec<glam::Vec3> {
    let result = run(graph, &registry(), surface, s).expect("graph runs");
    match &result.outputs[node]["points"] {
        Data::Points(p) => p.iter().map(|p| p.position).collect(),
        other => panic!("not points: {other:?}"),
    }
}

#[test]
fn grid_sampler_places_the_exact_lattice() {
    let g = GraphBuilder::new(&registry(), 1)
        .node("grid", "grid_sampler", [("spacing", json!(2.0))])
        .build()
        .unwrap();
    let pts = points_of(&g, &FlatSurface::new(3.0), &settings(1, 10.0), "grid");
    assert_eq!(pts.len(), 100); // 20 / 2 squared
    assert!(pts.iter().all(|p| p.y == 3.0));
    // Cell centres: first point is half a spacing from the corner.
    assert!((pts[0].x + 9.0).abs() < 1e-4 && (pts[0].z + 9.0).abs() < 1e-4);
}

#[test]
fn random_sampler_respects_count_bounds_and_seed() {
    let g = GraphBuilder::new(&registry(), 5)
        .node("r", "random_sampler", [("count", json!(400))])
        .build()
        .unwrap();
    let a = points_of(&g, &FlatSurface::new(0.0), &settings(5, 8.0), "r");
    let b = points_of(&g, &FlatSurface::new(0.0), &settings(5, 8.0), "r");
    let c = points_of(&g, &FlatSurface::new(0.0), &settings(6, 8.0), "r");
    assert_eq!(a.len(), 400);
    assert_eq!(a, b, "same seed, same points");
    assert_ne!(a, c, "different seed, different points");
    assert!(a.iter().all(|p| p.x.abs() <= 8.0 && p.z.abs() <= 8.0));
    // Uniformity: the mean of 400 uniform samples over [-8, 8] has sigma 4.62/sqrt(400) = 0.23.
    let mean_x: f32 = a.iter().map(|p| p.x).sum::<f32>() / 400.0;
    assert!(mean_x.abs() < 1.0, "mean x {mean_x}");
}

#[test]
fn poisson_sampler_keeps_minimum_spacing_and_fills_the_area() {
    let g = GraphBuilder::new(&registry(), 9)
        .node("p", "poisson_sampler", [("min_distance", json!(2.0))])
        .build()
        .unwrap();
    let pts = points_of(&g, &FlatSurface::new(0.0), &settings(9, 25.0), "p");
    let mut closest = f32::MAX;
    for (i, a) in pts.iter().enumerate() {
        for b in &pts[i + 1..] {
            closest = closest.min(a.distance(*b));
        }
    }
    assert!(closest >= 2.0 - 1e-3, "closest pair {closest}");
    // Bridson packs roughly 0.6..0.8 points per r^2 of area: 2500 / 4 * 0.5 = 312 is a floor.
    assert!(pts.len() > 312, "{} points", pts.len());
}

#[test]
fn height_and_slope_filters_select_by_ground() {
    // Ramp rising 1 unit per unit of x: slope 45 degrees everywhere, height = x.
    let ramp = FnSurface::new(
        Aabb::new([-10.0, -50.0, -10.0], [10.0, 50.0, 10.0]),
        |x, _| x,
    );
    let g = GraphBuilder::new(&registry(), 2)
        .node("grid", "grid_sampler", [("spacing", json!(1.0))])
        .node(
            "h",
            "height_filter",
            [("min", json!(0.0)), ("max", json!(5.0))],
        )
        .node(
            "s",
            "slope_filter",
            [("min_degrees", json!(40.0)), ("max_degrees", json!(50.0))],
        )
        .node(
            "steep",
            "slope_filter",
            [("min_degrees", json!(60.0)), ("max_degrees", json!(90.0))],
        )
        .connect("grid.points", "h.points")
        .connect("h.points", "s.points")
        .connect("grid.points", "steep.points")
        .build()
        .unwrap();
    let s = settings(2, 10.0);
    let kept = points_of(&g, &ramp, &s, "s");
    assert!(!kept.is_empty());
    assert!(kept.iter().all(|p| (0.0..=5.0).contains(&p.y)));
    // x in (0, 5] with unit grid centres at .5: 0.5 ..= 4.5 -> 5 columns of 20 rows.
    assert_eq!(kept.len(), 100);
    assert!(points_of(&g, &ramp, &s, "steep").is_empty());
}

#[test]
fn bounds_filter_inverts() {
    let g = GraphBuilder::new(&registry(), 3)
        .node("grid", "grid_sampler", [("spacing", json!(1.0))])
        .node(
            "inside",
            "bounds_filter",
            [
                ("min", json!([0.0, -5.0, 0.0])),
                ("max", json!([5.0, 5.0, 5.0])),
            ],
        )
        .node(
            "outside",
            "bounds_filter",
            [
                ("min", json!([0.0, -5.0, 0.0])),
                ("max", json!([5.0, 5.0, 5.0])),
                ("invert", json!(true)),
            ],
        )
        .connect("grid.points", "inside.points")
        .connect("grid.points", "outside.points")
        .build()
        .unwrap();
    let s = settings(3, 10.0);
    let surface = FlatSurface::new(0.0);
    assert_eq!(points_of(&g, &surface, &s, "inside").len(), 25);
    assert_eq!(points_of(&g, &surface, &s, "outside").len(), 400 - 25);
}

#[test]
fn density_filter_follows_the_noise_field() {
    let g = GraphBuilder::new(&registry(), 4)
        .node("grid", "grid_sampler", [("spacing", json!(0.5))])
        .node(
            "noise",
            "noise_field",
            [("frequency", json!(0.1)), ("fractal", json!("fbm"))],
        )
        .node("keep", "density_filter", [("threshold", json!(0.5))])
        .connect("grid.points", "keep.points")
        .connect("noise.field", "keep.field")
        .build()
        .unwrap();
    let s = settings(4, 10.0);
    let kept = points_of(&g, &FlatSurface::new(0.0), &s, "keep");
    // Noise is centred on 0.5: roughly half of 1600 points pass (tolerance +-25%).
    assert!((600..1000).contains(&kept.len()), "{} kept", kept.len());
    // Offsetting the domain changes which points pass.
    let mut shifted = s.clone();
    shifted.domain_offset = [1000.0, 0.0, 1000.0];
    let other = points_of(&g, &FlatSurface::new(0.0), &shifted, "keep");
    assert_ne!(kept, other);
}

#[test]
fn noise_field_samples_absolute_coordinates() {
    // Shifting the origin by the offset must be equivalent to shifting the sample position.
    let g = GraphBuilder::new(&registry(), 12)
        .node("n", "noise_field", [("frequency", json!(0.07))])
        .node("grid", "grid_sampler", [("spacing", json!(1.0))])
        .node("d", "density_filter", [("threshold", json!(-1.0))])
        .connect("grid.points", "d.points")
        .connect("n.field", "d.field")
        .build()
        .unwrap();
    let surface = FlatSurface::new(0.0);
    let base = settings(12, 5.0);
    let first = run(&g, &registry(), &surface, &base).unwrap();
    let mut moved = base.clone();
    moved.bounds = Aabb::new([-5.0 + 3.0, -10.0, -5.0], [5.0 + 3.0, 10.0, 5.0]);
    moved.domain_offset = [-3.0, 0.0, 0.0];
    let second = run(&g, &registry(), &surface, &moved).unwrap();
    let density = |r: &engine_pcg::RunResult| match &r.outputs["d"]["points"] {
        Data::Points(p) => p.iter().map(|p| p.density).collect::<Vec<_>>(),
        _ => unreachable!(),
    };
    for (a, b) in density(&first).iter().zip(density(&second)) {
        assert!((a - b).abs() < 1e-5, "{a} vs {b}");
    }
}

#[test]
fn self_prune_enforces_spacing_and_prefers_higher_density() {
    let r = registry();
    let mut g = GraphBuilder::new(&r, 6)
        .node("grid", "grid_sampler", [("spacing", json!(1.0))])
        .node("prune", "self_prune", [("min_distance", json!(2.5))])
        .connect("grid.points", "prune.points")
        .build()
        .unwrap();
    let surface = FlatSurface::new(0.0);
    let s = settings(6, 10.0);
    let kept = points_of(&g, &surface, &s, "prune");
    for (i, a) in kept.iter().enumerate() {
        for b in &kept[i + 1..] {
            assert!(a.distance(*b) >= 2.5, "{a} {b}");
        }
    }
    assert!(kept.len() < 400 && kept.len() > 20);
    // Density preference: height-derived density keeps the tallest point of a pair.
    let hills = FnSurface::new(
        Aabb::new([-10.0, -50.0, -10.0], [10.0, 50.0, 10.0]),
        |x, _| x,
    );
    g.nodes.retain(|n| n.id != "grid");
    g.edges.clear();
    apply_edit(
        &mut g,
        &r,
        EditOp::AddNode {
            id: "grid".into(),
            kind: "grid_sampler".into(),
            params: BTreeMap::from([("spacing".to_owned(), json!(1.0))]),
        },
    )
    .unwrap();
    for op in [
        EditOp::AddNode {
            id: "h".into(),
            kind: "surface_field".into(),
            params: BTreeMap::from([
                ("low".to_owned(), json!(-10.0)),
                ("high".to_owned(), json!(10.0)),
            ]),
        },
        EditOp::AddNode {
            id: "d".into(),
            kind: "density_filter".into(),
            params: BTreeMap::from([("threshold".to_owned(), json!(0.0))]),
        },
        EditOp::Connect {
            from: PortRef::new("grid", "points"),
            to: PortRef::new("d", "points"),
        },
        EditOp::Connect {
            from: PortRef::new("h", "field"),
            to: PortRef::new("d", "field"),
        },
        EditOp::Connect {
            from: PortRef::new("d", "points"),
            to: PortRef::new("prune", "points"),
        },
    ] {
        apply_edit(&mut g, &r, op).unwrap();
    }
    let kept = points_of(&g, &hills, &s, "prune");
    // The highest-density (largest x) column survives: the first kept point has max x.
    let max_x = kept.iter().map(|p| p.x).fold(f32::MIN, f32::max);
    assert!((max_x - 9.5).abs() < 1e-4, "max x {max_x}");
}

#[test]
fn merge_and_difference_combine_point_sets() {
    let g = GraphBuilder::new(&registry(), 7)
        .node("a", "grid_sampler", [("spacing", json!(2.0))])
        .node("b", "grid_sampler", [("spacing", json!(2.0))])
        .node("m", "merge", [("dedupe_distance", json!(0.5))])
        .node("m_all", "merge", [])
        .node("diff", "difference", [("radius", json!(0.5))])
        .connect("a.points", "m.a")
        .connect("b.points", "m.b")
        .connect("a.points", "m_all.a")
        .connect("b.points", "m_all.b")
        .connect("a.points", "diff.a")
        .connect("b.points", "diff.b")
        .build()
        .unwrap();
    let surface = FlatSurface::new(0.0);
    let s = settings(7, 10.0);
    assert_eq!(
        points_of(&g, &surface, &s, "m").len(),
        100,
        "identical sets dedupe"
    );
    assert_eq!(points_of(&g, &surface, &s, "m_all").len(), 200);
    assert_eq!(points_of(&g, &surface, &s, "diff").len(), 0);
}

#[test]
fn spawner_follows_entry_weights() {
    let g = GraphBuilder::new(&registry(), 8)
        .node("grid", "grid_sampler", [("spacing", json!(0.5))])
        .node(
            "spawn",
            "spawner",
            [("entries", entries(&[("oak", 3.0), ("pine", 1.0)]))],
        )
        .connect("grid.points", "spawn.points")
        .build()
        .unwrap();
    let result = run(&g, &registry(), &FlatSurface::new(0.0), &settings(8, 25.0)).unwrap();
    assert_eq!(result.instances.len(), 10_000);
    let oak = result.instances.iter().filter(|i| i.asset == "oak").count() as f64;
    let share = oak / 10_000.0;
    // Binomial sigma = sqrt(0.75 * 0.25 / 10000) = 0.0043; tolerance is 6 sigma.
    assert!((share - 0.75).abs() < 0.026, "oak share {share}");
    assert!(
        result
            .instances
            .iter()
            .all(|i| i.kind == InstanceKind::Mesh)
    );
    assert_eq!(result.nodes.last().unwrap().id, "spawn");
}

#[test]
fn transform_points_applies_scale_yaw_and_alignment() {
    let ramp = FnSurface::new(
        Aabb::new([-10.0, -50.0, -10.0], [10.0, 50.0, 10.0]),
        |x, _| x,
    );
    let g = GraphBuilder::new(&registry(), 10)
        .node("grid", "grid_sampler", [("spacing", json!(2.0))])
        .node(
            "t",
            "transform_points",
            [
                ("scale_min", json!(0.5)),
                ("scale_max", json!(2.0)),
                ("yaw_max_degrees", json!(360.0)),
                ("align_to_normal", json!(true)),
                ("offset", json!([0.0, 1.0, 0.0])),
            ],
        )
        .node("spawn", "spawner", [("entries", entries(&[("rock", 1.0)]))])
        .connect("grid.points", "t.points")
        .connect("t.points", "spawn.points")
        .build()
        .unwrap();
    let result = run(&g, &registry(), &ramp, &settings(10, 10.0)).unwrap();
    for i in &result.instances {
        assert!((0.5..=2.0).contains(&i.scale[0]), "{:?}", i.scale);
        assert_eq!(i.scale[0], i.scale[1], "uniform scale");
        // Offset of +1 above the ramp surface (height = x).
        assert!((i.position[1] - (i.position[0] + 1.0)).abs() < 1e-3);
        // Up axis rotated onto the ramp normal: 45 degrees from vertical.
        let q = glam::Quat::from_array(i.rotation);
        let up = q * glam::Vec3::Y;
        assert!((up.angle_between(glam::Vec3::Y).to_degrees() - 45.0).abs() < 0.5);
    }
}

#[test]
fn execution_is_deterministic_and_seed_sensitive() {
    let build = |seed| {
        GraphBuilder::new(&registry(), seed)
            .node("p", "poisson_sampler", [("min_distance", json!(1.5))])
            .node("n", "noise_field", [])
            .node("d", "density_filter", [("mode", json!("probabilistic"))])
            .node(
                "t",
                "transform_points",
                [("yaw_max_degrees", json!(360.0)), ("scale_max", json!(1.5))],
            )
            .node(
                "s",
                "spawner",
                [("entries", entries(&[("a", 1.0), ("b", 2.0)]))],
            )
            .connect("p.points", "d.points")
            .connect("n.field", "d.field")
            .connect("d.points", "t.points")
            .connect("t.points", "s.points")
            .build()
            .unwrap()
    };
    let surface = FlatSurface::new(0.0);
    let s = settings(0, 15.0);
    let mut s11 = s.clone();
    s11.seed = 11;
    let a = run(&build(11), &registry(), &surface, &s11)
        .unwrap()
        .instances;
    let b = run(&build(11), &registry(), &surface, &s11)
        .unwrap()
        .instances;
    let mut s12 = s.clone();
    s12.seed = 12;
    let c = run(&build(11), &registry(), &surface, &s12)
        .unwrap()
        .instances;
    assert!(!a.is_empty());
    assert_eq!(a, b);
    assert_ne!(a, c);
}

#[test]
fn validation_reports_precise_diagnostics() {
    let r = registry();
    let mut g = Graph::new(0);
    let add = |g: &mut Graph, id: &str, kind: &str| {
        apply_edit(
            g,
            &r,
            EditOp::AddNode {
                id: id.into(),
                kind: kind.into(),
                params: BTreeMap::new(),
            },
        )
    };
    add(&mut g, "grid", "grid_sampler").unwrap();
    add(&mut g, "noise", "noise_field").unwrap();
    add(&mut g, "filter", "density_filter").unwrap();
    assert_eq!(
        codes(&validate(&g, &r)),
        ["pcg.input.missing", "pcg.input.missing"]
    );

    // Type mismatch is rejected by the editor and reported when the file is hand-written.
    let bad = apply_edit(
        &mut g,
        &r,
        EditOp::Connect {
            from: PortRef::new("noise", "field"),
            to: PortRef::new("filter", "points"),
        },
    )
    .unwrap_err();
    assert_eq!(bad.code, "pcg.edge.type_mismatch");
    g.edges.push(engine_pcg::Edge {
        from: PortRef::new("noise", "field"),
        to: PortRef::new("filter", "points"),
    });
    let found = validate(&g, &r);
    let mismatch = found
        .iter()
        .find(|d| d.code == "pcg.edge.type_mismatch")
        .unwrap();
    assert_eq!(mismatch.node.as_deref(), Some("filter"));
    assert_eq!(mismatch.port.as_deref(), Some("points"));

    let unknown_kind = json!({"nodes": [{"id": "x", "kind": "nope"}]}).to_string();
    let g2 = Graph::from_json(&unknown_kind).unwrap();
    assert_eq!(codes(&validate(&g2, &r)), ["pcg.node.unknown_kind"]);

    let bad_param = json!({"nodes": [{"id": "x", "kind": "random_sampler", "params": {"count": -1, "colour": 3}}]})
        .to_string();
    let found = validate(&Graph::from_json(&bad_param).unwrap(), &r);
    assert_eq!(codes(&found), ["pcg.param.unknown", "pcg.param.invalid"]);

    let spawner_missing = json!({"nodes": [{"id": "s", "kind": "spawner"}]}).to_string();
    let found = validate(&Graph::from_json(&spawner_missing).unwrap(), &r);
    assert!(codes(&found).contains(&"pcg.param.required"));

    let dup = json!({"nodes": [{"id": "a", "kind": "grid_sampler"}, {"id": "a", "kind": "grid_sampler"}, {"id": "bad id", "kind": "grid_sampler"}]})
        .to_string();
    let found = validate(&Graph::from_json(&dup).unwrap(), &r);
    assert!(codes(&found).contains(&"pcg.node.duplicate_id"));
    assert!(codes(&found).contains(&"pcg.node.invalid_id"));
}

#[test]
fn cycles_and_double_inputs_are_rejected() {
    let r = registry();
    let text = json!({
        "nodes": [
            {"id": "a", "kind": "self_prune"},
            {"id": "b", "kind": "self_prune"},
            {"id": "src", "kind": "grid_sampler"},
        ],
        "edges": [
            {"from": {"node": "a", "port": "points"}, "to": {"node": "b", "port": "points"}},
            {"from": {"node": "b", "port": "points"}, "to": {"node": "a", "port": "points"}},
            {"from": {"node": "src", "port": "points"}, "to": {"node": "a", "port": "points"}},
        ]
    })
    .to_string();
    let found = validate(&Graph::from_json(&text).unwrap(), &r);
    let cycle = found
        .iter()
        .find(|d| d.code == "pcg.graph.cycle")
        .expect("cycle");
    assert!(cycle.message.contains('a') && cycle.message.contains('b'));
    assert!(codes(&found).contains(&"pcg.input.multiple"));

    // The editor refuses an edge that would close a loop.
    let mut g = Graph::new(0);
    for id in ["a", "b"] {
        apply_edit(
            &mut g,
            &r,
            EditOp::AddNode {
                id: id.into(),
                kind: "self_prune".into(),
                params: BTreeMap::new(),
            },
        )
        .unwrap();
    }
    apply_edit(
        &mut g,
        &r,
        EditOp::Connect {
            from: PortRef::new("a", "points"),
            to: PortRef::new("b", "points"),
        },
    )
    .unwrap();
    let err = apply_edit(
        &mut g,
        &r,
        EditOp::Connect {
            from: PortRef::new("b", "points"),
            to: PortRef::new("a", "points"),
        },
    )
    .unwrap_err();
    assert_eq!(err.code, "pcg.graph.cycle");
    assert_eq!(g.edges.len(), 1, "graph unchanged after a refused edit");
}

#[test]
fn run_rejects_invalid_graphs_with_all_diagnostics() {
    let g = Graph::from_json(r#"{"nodes":[{"id":"f","kind":"height_filter"}]}"#).unwrap();
    let err = run(&g, &registry(), &FlatSurface::new(0.0), &settings(0, 5.0)).unwrap_err();
    assert_eq!(codes(&err.diagnostics), ["pcg.input.missing"]);
    assert!(err.to_string().contains("pcg.input.missing [f.points]"));
}

#[test]
fn limits_bound_every_stage() {
    let r = registry();
    let g = GraphBuilder::new(&r, 1)
        .node("grid", "grid_sampler", [("spacing", json!(0.1))])
        .build()
        .unwrap();
    let mut s = settings(1, 50.0);
    s.limits = Limits {
        max_points: 1000,
        max_instances: 1000,
    };
    let err = run(&g, &r, &FlatSurface::new(0.0), &s).unwrap_err();
    assert_eq!(err.diagnostics[0].code, "pcg.limit.points");

    let random = GraphBuilder::new(&r, 1)
        .node("r", "random_sampler", [("count", json!(5000))])
        .build()
        .unwrap();
    assert_eq!(
        run(&random, &r, &FlatSurface::new(0.0), &s)
            .unwrap_err()
            .diagnostics[0]
            .code,
        "pcg.limit.points"
    );

    let spawn = GraphBuilder::new(&r, 1)
        .node("r", "random_sampler", [("count", json!(800))])
        .node("s", "spawner", [("entries", entries(&[("a", 1.0)]))])
        .connect("r.points", "s.points")
        .build()
        .unwrap();
    s.limits = Limits {
        max_points: 1000,
        max_instances: 500,
    };
    assert_eq!(
        run(&spawn, &r, &FlatSurface::new(0.0), &s)
            .unwrap_err()
            .diagnostics[0]
            .code,
        "pcg.limit.instances"
    );

    let tiny = GraphBuilder::new(&r, 1)
        .node("p", "poisson_sampler", [("min_distance", json!(0.01))])
        .build()
        .unwrap();
    let mut huge = settings(1, 5000.0);
    huge.limits = Limits::default();
    assert_eq!(
        run(&tiny, &r, &FlatSurface::new(0.0), &huge)
            .unwrap_err()
            .diagnostics[0]
            .code,
        "pcg.limit.points"
    );
}

#[test]
fn surface_bounds_clip_generation() {
    let g = GraphBuilder::new(&registry(), 1)
        .node("grid", "grid_sampler", [("spacing", json!(1.0))])
        .build()
        .unwrap();
    let mut surface = FlatSurface::new(0.0);
    surface.bounds = Aabb::new([0.0, -1.0, 0.0], [4.0, 1.0, 4.0]);
    let pts = points_of(&g, &surface, &settings(1, 50.0), "grid");
    assert_eq!(pts.len(), 16);
}

#[test]
fn json_round_trip_and_versions() {
    let r = registry();
    let g = GraphBuilder::new(&r, 99)
        .node("grid", "grid_sampler", [("spacing", json!(3.0))])
        .node("s", "spawner", [("entries", entries(&[("a", 1.0)]))])
        .connect("grid.points", "s.points")
        .build()
        .unwrap();
    let text = g.to_json();
    assert_eq!(Graph::from_json(&text).unwrap(), g);
    assert_eq!(
        Graph::from_json(r#"{"version": 2}"#).unwrap_err().code,
        "pcg.graph.version"
    );
    assert_eq!(
        Graph::from_json(r#"{"nodes": [], "bogus": 1}"#)
            .unwrap_err()
            .code,
        "pcg.graph.parse"
    );
}

#[test]
fn edit_operations_round_trip() {
    let r = registry();
    let mut g = Graph::new(0);
    let ops: Vec<EditOp> = serde_json::from_value(json!([
        {"op": "add_node", "id": "a", "kind": "grid_sampler"},
        {"op": "add_node", "id": "b", "kind": "height_filter"},
        {"op": "connect", "from": {"node": "a", "port": "points"}, "to": {"node": "b", "port": "points"}},
        {"op": "set_parameter", "node": "b", "name": "max", "value": 3.5},
        {"op": "set_seed", "seed": 77}
    ]))
    .unwrap();
    for op in ops {
        apply_edit(&mut g, &r, op).unwrap();
    }
    assert_eq!(g.seed, 77);
    assert_eq!(g.node("b").unwrap().params["max"], json!(3.5));
    let err = apply_edit(
        &mut g,
        &r,
        EditOp::SetParameter {
            node: "b".into(),
            name: "max".into(),
            value: json!("high"),
        },
    )
    .unwrap_err();
    assert_eq!(err.code, "pcg.param.invalid");
    apply_edit(
        &mut g,
        &r,
        EditOp::ClearParameter {
            node: "b".into(),
            name: "max".into(),
        },
    )
    .unwrap();
    assert!(g.node("b").unwrap().params.is_empty());
    apply_edit(
        &mut g,
        &r,
        EditOp::Disconnect {
            from: PortRef::new("a", "points"),
            to: PortRef::new("b", "points"),
        },
    )
    .unwrap();
    assert!(g.edges.is_empty());
    apply_edit(&mut g, &r, EditOp::RemoveNode { id: "a".into() }).unwrap();
    assert_eq!(
        apply_edit(&mut g, &r, EditOp::RemoveNode { id: "a".into() })
            .unwrap_err()
            .code,
        "pcg.edit.unknown_node"
    );
    assert_eq!(
        apply_edit(
            &mut g,
            &r,
            EditOp::AddNode {
                id: "z".into(),
                kind: "warp".into(),
                params: BTreeMap::new()
            }
        )
        .unwrap_err()
        .code,
        "pcg.node.unknown_kind"
    );
}

#[test]
fn custom_node_kinds_register_and_run() {
    let mut r = registry();
    r.register(
        NodeDefinition {
            kind: "ring".into(),
            title: "Ring".into(),
            doc: "Points on a circle.".into(),
            inputs: vec![],
            outputs: vec![PortSpec::required("points", PortType::Points)],
            params: vec![
                ParamSpec::int("count", 8, 1, 1024),
                ParamSpec::float("radius", 5.0, 0.0, 1000.0),
            ],
        },
        |ctx| {
            let count = ctx.params.int("count") as usize;
            let radius = ctx.params.float("radius") as f32;
            let points = (0..count)
                .map(|i| {
                    let a = i as f32 / count as f32 * std::f32::consts::TAU;
                    engine_pcg::Point::new(glam::Vec3::new(a.cos() * radius, 0.0, a.sin() * radius))
                })
                .collect();
            let mut out: Outputs = BTreeMap::new();
            out.insert("points".into(), Data::points(points));
            Ok(out)
        },
    )
    .unwrap();
    assert!(
        r.register(r.definition("ring").unwrap().clone(), |_| Err(
            NodeError::failed("x")
        ))
        .is_err()
    );
    let g = GraphBuilder::new(&r, 0)
        .node("ring", "ring", [("count", json!(12))])
        .build()
        .unwrap();
    let result = run(&g, &r, &FlatSurface::new(0.0), &settings(0, 10.0)).unwrap();
    assert_eq!(result.nodes[0].points, 12);
    // A kind the registry does not know is a validation error, not a panic.
    assert_eq!(codes(&validate(&g, &registry())), ["pcg.node.unknown_kind"]);
}

#[test]
fn layer_fields_read_the_surface() {
    let surface = FnSurface::new(
        Aabb::new([-10.0, -5.0, -10.0], [10.0, 5.0, 10.0]),
        |_, _| 0.0,
    )
    .with_layer("grass", |x, _| if x > 0.0 { 1.0 } else { 0.0 });
    let g = GraphBuilder::new(&registry(), 1)
        .node("grid", "grid_sampler", [("spacing", json!(1.0))])
        .node(
            "f",
            "surface_field",
            [("source", json!("layer")), ("layer", json!("grass"))],
        )
        .node("d", "density_filter", [("threshold", json!(0.5))])
        .connect("grid.points", "d.points")
        .connect("f.field", "d.field")
        .build()
        .unwrap();
    let kept = points_of(&g, &surface, &settings(1, 10.0), "d");
    assert_eq!(kept.len(), 200);
    assert!(kept.iter().all(|p| p.x > 0.0));
}
