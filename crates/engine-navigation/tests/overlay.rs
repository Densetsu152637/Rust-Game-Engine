use engine_navigation::*;
use glam::Vec3;

fn floor(input: &mut BakeInput, min: [f32; 3], max: [f32; 3]) {
    let n = input.vertices.len() as u32;
    input
        .vertices
        .extend([min, [max[0], max[1], min[2]], max, [min[0], min[1], max[2]]]);
    input
        .triangles
        .extend([[n, n + 1, n + 2], [n, n + 2, n + 3]]);
}
fn input(radius: f32) -> BakeInput {
    BakeInput {
        vertices: vec![],
        triangles: vec![],
        obstacles: vec![],
        settings: BakeSettings {
            cell_size: 0.5,
            agent_radius: radius,
            ..Default::default()
        },
    }
}
fn query() -> QuerySettings {
    QuerySettings {
        max_distance: 0.5,
        max_expansions: 65_536,
        ..Default::default()
    }
}
fn points(route: &NavRoute) -> Vec<[f32; 3]> {
    match &route.path {
        NavPath::Reached { points, .. } => points.clone(),
        other => panic!("{other:?}"),
    }
}
fn open_mesh() -> NavMesh {
    let mut i = input(0.3);
    floor(&mut i, [0., 0., 0.], [20., 0., 20.]);
    bake(&i).unwrap()
}
/// Largest depth any sampled path point has inside `[lo, hi]` on XZ (negative: outside).
fn max_penetration(path: &[[f32; 3]], lo: [f32; 2], hi: [f32; 2]) -> f32 {
    let mut worst = f32::MIN;
    for pair in path.windows(2) {
        for i in 0..=200 {
            let t = i as f32 / 200.;
            let x = pair[0][0] * (1. - t) + pair[1][0] * t;
            let z = pair[0][2] * (1. - t) + pair[1][2] * t;
            let depth = (x - lo[0]).min(hi[0] - x).min((z - lo[1]).min(hi[1] - z));
            worst = worst.max(depth);
        }
    }
    worst
}

#[test]
fn carved_box_is_avoided_and_removal_restores_the_path() {
    let mesh = open_mesh();
    let (a, b) = ([2., 0., 10.], [18., 0., 10.]);
    let original = points(&mesh.find_route(a, b, query(), None).unwrap());
    assert_eq!(original.len(), 2);
    let mut overlay = NavOverlay::new(&mesh);
    assert_eq!(overlay.margin(), 0.3);
    let covered = overlay
        .set_obstacle(
            &mesh,
            1,
            DynamicShape::Box {
                min: [8., 0., 6.],
                max: [12., 2., 14.],
            },
        )
        .unwrap();
    assert!(covered > 0 && overlay.blocked_polygons() == covered);
    let path = points(&mesh.find_route(a, b, query(), Some(&overlay)).unwrap());
    assert!(path.len() > 2);
    // Footprint inflated by the agent radius (0.3): [7.7, 5.7]..[12.3, 14.3].
    let depth = max_penetration(&path, [7.7, 5.7], [12.3, 14.3]);
    println!("carve box: max penetration into inflated footprint = {depth:.6}");
    assert!(depth <= 1e-3, "{depth}");
    assert!(overlay.remove_obstacle(1));
    assert_eq!(overlay.blocked_polygons(), 0);
    let restored = points(&mesh.find_route(a, b, query(), Some(&overlay)).unwrap());
    assert_eq!(restored, original);
}

#[test]
fn carved_cylinder_overlapping_obstacles_and_full_block() {
    let mesh = open_mesh();
    let mut overlay = NavOverlay::new(&mesh);
    overlay
        .set_obstacle(
            &mesh,
            5,
            DynamicShape::Cylinder {
                base: [10., 0., 10.],
                radius: 2.,
                height: 2.,
            },
        )
        .unwrap();
    let path = points(
        &mesh
            .find_route([2., 0., 10.], [18., 0., 10.], query(), Some(&overlay))
            .unwrap(),
    );
    for pair in path.windows(2) {
        for i in 0..=100 {
            let t = i as f32 / 100.;
            let x = pair[0][0] * (1. - t) + pair[1][0] * t - 10.;
            let z = pair[0][2] * (1. - t) + pair[1][2] * t - 10.;
            assert!(x.hypot(z) >= 2.3 - 1e-3, "{x} {z}");
        }
    }
    // Two overlapping boxes keep the area blocked until both are removed.
    let boxed = |min: f32| DynamicShape::Box {
        min: [min, 0., 0.],
        max: [min + 3., 2., 20.],
    };
    overlay.set_obstacle(&mesh, 1, boxed(8.)).unwrap();
    overlay.set_obstacle(&mesh, 2, boxed(9.)).unwrap();
    let wall = mesh
        .find_route([2., 0., 10.], [18., 0., 10.], query(), Some(&overlay))
        .unwrap();
    assert_eq!(wall.path, NavPath::Unreachable);
    overlay.remove_obstacle(1);
    let still = mesh
        .find_route([2., 0., 10.], [18., 0., 10.], query(), Some(&overlay))
        .unwrap();
    assert_eq!(still.path, NavPath::Unreachable);
    overlay.remove_obstacle(2);
    overlay.remove_obstacle(5);
    assert_eq!(overlay.blocked_polygons(), 0);
    assert!(
        overlay
            .set_obstacle(
                &mesh,
                9,
                DynamicShape::Box {
                    min: [1.; 3],
                    max: [0.; 3]
                }
            )
            .is_err()
    );
}

fn islands() -> NavMesh {
    let mut i = input(0.);
    floor(&mut i, [0., 0., 0.], [5., 0., 5.]);
    floor(&mut i, [10., 0., 0.], [15., 0., 5.]);
    bake(&i).unwrap()
}
#[test]
fn off_mesh_link_connects_islands_with_stated_cost() {
    let mesh = islands();
    let (a, b) = ([1.25, 0., 2.25], [14.75, 0., 2.25]);
    let plain = mesh.find_route(a, b, query(), None).unwrap();
    assert_eq!(plain.path, NavPath::Unreachable);
    let mut overlay = NavOverlay::new(&mesh);
    let desc = OffMeshLinkDesc {
        start: [4.75, 0., 2.25],
        end: [10.25, 0., 2.25],
        cost: 3.,
        ..Default::default()
    };
    overlay.set_link(&mesh, 4, desc).unwrap();
    let route = mesh.find_route(a, b, query(), Some(&overlay)).unwrap();
    let pts = points(&route);
    // 7 hops to the link start cell, link cost 3, 9 hops from the link end cell.
    assert_eq!(route.cost, 19.);
    assert_eq!(route.links.len(), 1);
    let l = route.links[0];
    assert_eq!(
        (l.link, l.forward, l.exit_index),
        (4, true, l.entry_index + 1)
    );
    assert_eq!(pts[l.entry_index], [4.75, 0., 2.25]);
    assert_eq!(pts[l.exit_index], [10.25, 0., 2.25]);
    // Reverse direction works while bidirectional, and flags the link as backward.
    let back = mesh.find_route(b, a, query(), Some(&overlay)).unwrap();
    assert_eq!(back.cost, 19.);
    assert!(!back.links[0].forward);
    // One-way and disabled links.
    overlay
        .set_link(
            &mesh,
            4,
            OffMeshLinkDesc {
                bidirectional: false,
                ..desc
            },
        )
        .unwrap();
    assert_eq!(
        mesh.find_route(b, a, query(), Some(&overlay)).unwrap().path,
        NavPath::Unreachable
    );
    assert_eq!(
        mesh.find_route(a, b, query(), Some(&overlay)).unwrap().cost,
        19.
    );
    overlay.set_link_enabled(4, false).unwrap();
    assert_eq!(
        mesh.find_route(a, b, query(), Some(&overlay)).unwrap().path,
        NavPath::Unreachable
    );
    overlay.set_link_enabled(4, true).unwrap();
    assert!(overlay.remove_link(4));
    assert_eq!(
        mesh.find_route(a, b, query(), Some(&overlay)).unwrap().path,
        NavPath::Unreachable
    );
    assert!(
        overlay
            .set_link(
                &mesh,
                1,
                OffMeshLinkDesc {
                    start: [50., 0., 50.],
                    ..desc
                }
            )
            .is_err()
    );
}

#[test]
fn crowd_agent_reports_link_entry_and_exit() {
    let mesh = islands();
    let mut overlay = NavOverlay::new(&mesh);
    overlay
        .set_link(
            &mesh,
            2,
            OffMeshLinkDesc {
                start: [4.75, 0., 2.25],
                end: [10.25, 0., 2.25],
                cost: 3.,
                ..Default::default()
            },
        )
        .unwrap();
    let nav = NavContext {
        mesh: &mesh,
        overlay: Some(&overlay),
        query: query(),
    };
    let mut crowd = Crowd::new(CrowdSettings::default()).unwrap();
    let id = crowd
        .add_agent(AgentDesc {
            position: Vec3::new(1.25, 0., 2.25),
            radius: 0.3,
            max_speed: 2.,
            arrival_radius: 0.1,
        })
        .unwrap();
    assert_eq!(
        crowd
            .set_goal(id, Vec3::new(14.75, 0., 2.25), &nav)
            .unwrap(),
        GoalOutcome::Reached
    );
    let mut events = Vec::new();
    for _ in 0..600 {
        events.extend(crowd.step(0.05, Some(&nav)).unwrap());
    }
    assert_eq!(
        events,
        vec![
            CrowdEvent::LinkEntered { agent: id, link: 2 },
            CrowdEvent::LinkExited { agent: id, link: 2 },
            CrowdEvent::Arrived { agent: id },
        ]
    );
}

#[test]
fn new_obstacle_invalidates_and_repaths_crowd_agents() {
    let mesh = open_mesh();
    let mut overlay = NavOverlay::new(&mesh);
    let mut crowd = Crowd::new(CrowdSettings::default()).unwrap();
    let id = crowd
        .add_agent(AgentDesc {
            position: Vec3::new(2., 0., 10.),
            radius: 0.3,
            max_speed: 2.,
            arrival_radius: 0.1,
        })
        .unwrap();
    let goal = Vec3::new(18., 0., 10.);
    {
        let nav = NavContext {
            mesh: &mesh,
            overlay: Some(&overlay),
            query: query(),
        };
        crowd.set_goal(id, goal, &nav).unwrap();
        for _ in 0..20 {
            assert!(crowd.step(0.05, Some(&nav)).unwrap().is_empty());
        }
        assert!(crowd.invalidated_agents(&nav).is_empty());
    }
    overlay
        .set_obstacle(
            &mesh,
            1,
            DynamicShape::Box {
                min: [8., 0., 6.],
                max: [12., 2., 14.],
            },
        )
        .unwrap();
    let nav = NavContext {
        mesh: &mesh,
        overlay: Some(&overlay),
        query: query(),
    };
    assert_eq!(crowd.invalidated_agents(&nav), vec![id]);
    let mut events = crowd.step(0.05, Some(&nav)).unwrap();
    assert_eq!(events, vec![CrowdEvent::Repathed { agent: id }]);
    assert!(crowd.invalidated_agents(&nav).is_empty());
    let mut worst = f32::MIN;
    for _ in 0..800 {
        events.extend(crowd.step(0.05, Some(&nav)).unwrap());
        let p = crowd.agent(id).unwrap().position;
        worst = worst.max((p.x - 8.).min(12. - p.x).min((p.z - 6.).min(14. - p.z)));
    }
    assert!(worst <= 0.0, "agent entered the obstacle: {worst}");
    assert!(events.contains(&CrowdEvent::Arrived { agent: id }));
}
