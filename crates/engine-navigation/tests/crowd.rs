use engine_navigation::*;
use glam::Vec3;

fn arena(size: f32) -> NavMesh {
    let input = BakeInput {
        vertices: vec![
            [0., 0., 0.],
            [size, 0., 0.],
            [size, 0., size],
            [0., 0., size],
        ],
        triangles: vec![[0, 1, 2], [0, 2, 3]],
        obstacles: vec![],
        settings: BakeSettings {
            cell_size: 0.5,
            agent_radius: 0.,
            ..Default::default()
        },
    };
    bake(&input).unwrap()
}
fn query() -> QuerySettings {
    QuerySettings {
        max_distance: 0.5,
        max_expansions: 65_536,
        ..Default::default()
    }
}
struct Run {
    min_gap: f32,
    ticks: usize,
    arrived: Vec<Option<usize>>,
    positions: Vec<Vec3>,
    corrections: usize,
}
const DT: f32 = 0.05;
fn run(mesh: &NavMesh, crowd: &mut Crowd, goals: &[Vec3], max_ticks: usize) -> Run {
    let nav = NavContext {
        mesh,
        overlay: None,
        query: query(),
    };
    let ids: Vec<_> = crowd.agent_ids().collect();
    for (id, goal) in ids.iter().zip(goals) {
        assert_eq!(
            crowd.set_goal(*id, *goal, &nav).unwrap(),
            GoalOutcome::Reached
        );
    }
    let mut arrived = vec![None; ids.len()];
    let mut min_gap = f32::MAX;
    let mut corrections = 0;
    let mut ticks = 0;
    for tick in 1..=max_ticks {
        for e in crowd.step(DT, Some(&nav)).unwrap() {
            if let CrowdEvent::Arrived { agent } = e {
                arrived[agent as usize].get_or_insert(tick);
            }
        }
        corrections += crowd.stats().contact_corrections;
        ticks = tick;
        let p = crowd.positions();
        for i in 0..p.len() {
            for j in i + 1..p.len() {
                let ri = crowd.agent(p[i].0).unwrap().radius;
                let rj = crowd.agent(p[j].0).unwrap().radius;
                let gap = (p[i].1 - p[j].1).length() - (ri + rj);
                min_gap = min_gap.min(gap);
            }
        }
        if arrived.iter().all(Option::is_some) {
            break;
        }
    }
    Run {
        min_gap,
        ticks,
        arrived,
        positions: crowd.positions().into_iter().map(|(_, p)| p).collect(),
        corrections,
    }
}
fn add(crowd: &mut Crowd, p: Vec3) {
    crowd
        .add_agent(AgentDesc {
            position: p,
            radius: 0.4,
            max_speed: 2.,
            arrival_radius: 0.15,
        })
        .unwrap();
}
fn head_on() -> Run {
    let mesh = arena(30.);
    let mut crowd = Crowd::new(CrowdSettings::default()).unwrap();
    add(&mut crowd, Vec3::new(5., 0., 15.));
    add(&mut crowd, Vec3::new(25., 0., 15.));
    run(
        &mesh,
        &mut crowd,
        &[Vec3::new(25., 0., 15.), Vec3::new(5., 0., 15.)],
        1200,
    )
}
fn crossing() -> Run {
    let mesh = arena(50.);
    let mut crowd = Crowd::new(CrowdSettings::default()).unwrap();
    let mut goals = Vec::new();
    for lane in 0..8 {
        let o = 25. + (lane as f32 - 3.5) * 1.1;
        let d = (lane % 2) as f32 * 1.5;
        add(&mut crowd, Vec3::new(8. - d, 0., o));
        goals.push(Vec3::new(42., 0., o));
        add(&mut crowd, Vec3::new(42. + d, 0., o + 0.3));
        goals.push(Vec3::new(8., 0., o + 0.3));
        add(&mut crowd, Vec3::new(o + 0.6, 0., 8. - d));
        goals.push(Vec3::new(o + 0.6, 0., 42.));
        add(&mut crowd, Vec3::new(o - 0.2, 0., 42. + d));
        goals.push(Vec3::new(o - 0.2, 0., 8.));
    }
    run(&mesh, &mut crowd, &goals, 2400)
}
#[test]
fn head_on_swap_keeps_separation_and_arrives() {
    let r = head_on();
    println!(
        "head-on: min_gap={:.5} ticks={} corrections={} arrived={:?}",
        r.min_gap, r.ticks, r.corrections, r.arrived
    );
    assert!(r.min_gap >= -1e-3, "{}", r.min_gap);
    assert!(r.arrived.iter().all(Option::is_some));
    assert!(r.ticks <= 600);
}
#[test]
fn four_way_crossing_of_32_agents() {
    let r = crossing();
    println!(
        "crossing32: min_gap={:.5} ticks={} corrections={}",
        r.min_gap, r.ticks, r.corrections
    );
    assert_eq!(r.arrived.len(), 32);
    assert!(r.min_gap >= -1e-3, "{}", r.min_gap);
    assert!(r.arrived.iter().all(Option::is_some), "{:?}", r.arrived);
    assert!(r.ticks <= 1500);
}
#[test]
fn identical_runs_are_bitwise_equal() {
    let (a, b) = (crossing(), crossing());
    assert_eq!(a.ticks, b.ticks);
    for (p, q) in a.positions.iter().zip(&b.positions) {
        assert_eq!(
            p.to_array().map(f32::to_bits),
            q.to_array().map(f32::to_bits)
        );
    }
}
#[test]
fn formation_members_settle_on_slots() {
    let mesh = arena(30.);
    let nav = NavContext {
        mesh: &mesh,
        overlay: None,
        query: query(),
    };
    let mut crowd = Crowd::new(CrowdSettings::default()).unwrap();
    for i in 0..9 {
        crowd
            .add_agent(AgentDesc {
                position: Vec3::new(3. + (i % 3) as f32, 0., 3. + (i / 3) as f32),
                radius: 0.4,
                max_speed: 2.,
                arrival_radius: 0.05,
            })
            .unwrap();
    }
    let slots: Vec<[f32; 2]> = (0..9)
        .map(|i| [(i % 3) as f32 * 1.5 - 1.5, (i / 3) as f32 * 1.5])
        .collect();
    crowd
        .set_formation(
            1,
            FormationDesc {
                slots,
                anchor: FormationAnchor::Fixed([20., 0., 20.]),
                yaw: 0.7,
                follow_heading: false,
            },
        )
        .unwrap();
    let ids: Vec<_> = crowd.agent_ids().collect();
    crowd.assign_formation(1, &ids).unwrap();
    let again = crowd.formation_members(1).unwrap().to_vec();
    crowd.assign_formation(1, &ids).unwrap();
    assert_eq!(again, crowd.formation_members(1).unwrap());
    let mut min_gap = f32::MAX;
    for _ in 0..800 {
        crowd.step(DT, Some(&nav)).unwrap();
        let p = crowd.positions();
        for i in 0..p.len() {
            for j in i + 1..p.len() {
                min_gap = min_gap.min((p[i].1 - p[j].1).length() - 0.8);
            }
        }
    }
    let mut worst = 0f32;
    for &(agent, slot) in crowd.formation_members(1).unwrap() {
        let target = Vec3::from(crowd.slot_position(1, slot).unwrap());
        worst = worst.max((crowd.agent(agent).unwrap().position - target).length());
    }
    println!("formation: worst slot error={worst:.5} min_gap={min_gap:.5}");
    assert!(worst < 0.06, "{worst}");
    assert!(min_gap >= -1e-3);
}
#[test]
fn leader_formation_follows_leader() {
    let mut crowd = Crowd::new(CrowdSettings::default()).unwrap();
    add(&mut crowd, Vec3::new(0., 0., 0.));
    for i in 0..4 {
        add(&mut crowd, Vec3::new(i as f32 - 2., 0., -3.));
    }
    crowd
        .set_formation(
            7,
            FormationDesc {
                slots: vec![[-1., -1.5], [1., -1.5], [-2., -3.], [2., -3.]],
                anchor: FormationAnchor::Leader(0),
                yaw: 0.,
                follow_heading: true,
            },
        )
        .unwrap();
    crowd.assign_formation(7, &[0, 1, 2, 3, 4]).unwrap();
    assert_eq!(crowd.formation_members(7).unwrap().len(), 4);
    crowd.set_path(0, vec![Vec3::new(0., 0., 30.)]).unwrap();
    for _ in 0..400 {
        crowd.step(DT, None).unwrap();
    }
    assert!(crowd.agent(0).unwrap().arrived);
    for &(agent, slot) in crowd.formation_members(7).unwrap() {
        let target = Vec3::from(crowd.slot_position(7, slot).unwrap());
        assert!((crowd.agent(agent).unwrap().position - target).length() < 0.3);
    }
}
#[test]
fn static_segment_is_not_crossed() {
    let mut crowd = Crowd::new(CrowdSettings::default()).unwrap();
    add(&mut crowd, Vec3::new(0., 0., 0.));
    crowd.set_segment(1, [4., -3.], [4., 3.]).unwrap();
    crowd.set_path(0, vec![Vec3::new(8., 0., 0.)]).unwrap();
    let mut max_x = f32::MIN;
    for _ in 0..600 {
        crowd.step(DT, None).unwrap();
        max_x = max_x.max(crowd.agent(0).unwrap().position.x);
    }
    assert!(max_x <= 4. - 0.4 + 1e-3, "{max_x}");
}
#[test]
fn limits_are_enforced() {
    let mut crowd = Crowd::new(CrowdSettings {
        max_agents: 2,
        ..Default::default()
    })
    .unwrap();
    add(&mut crowd, Vec3::ZERO);
    add(&mut crowd, Vec3::X);
    assert!(crowd.add_agent(AgentDesc::default()).is_err());
    assert!(crowd.step(0., None).is_err());
    assert!(
        Crowd::new(CrowdSettings {
            max_neighbors: 1000,
            ..Default::default()
        })
        .is_err()
    );
}
