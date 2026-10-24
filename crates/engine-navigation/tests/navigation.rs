use engine_navigation::*;
use glam::Vec3;
#[test]
fn weighted_grid_detours_around_expensive_and_blocked_cells() {
    let mut costs = vec![Some(1); 15];
    costs[7] = None;
    costs[6] = Some(100);
    let grid = Grid::new(5, 3, costs, false).unwrap();
    let result = grid
        .find_path([0, 1], [4, 1], SearchBudget::default())
        .unwrap();
    let PathResult::Reached { nodes, cost } = result else {
        panic!("expected path");
    };
    assert_eq!(nodes.first(), Some(&[0, 1]));
    assert_eq!(nodes.last(), Some(&[4, 1]));
    assert!(!nodes.contains(&[2, 1]));
    assert!(!nodes.contains(&[1, 1]));
    assert_eq!(cost, 6);
    assert_eq!(
        grid.find_path([0, 1], [4, 1], SearchBudget::default())
            .unwrap(),
        PathResult::Reached { nodes, cost }
    );
    assert_eq!(
        grid.find_path([0, 1], [4, 1], SearchBudget { max_expansions: 0 })
            .unwrap(),
        PathResult::BudgetExceeded
    );
}
#[test]
fn diagonals_cannot_cut_blocked_corners() {
    let mut grid = Grid::new(2, 2, vec![Some(1), None, None, Some(1)], true).unwrap();
    assert_eq!(
        grid.find_path([0, 0], [1, 1], SearchBudget::default())
            .unwrap(),
        PathResult::Unreachable
    );
    grid.set_cost([1, 0], Some(1)).unwrap();
    let PathResult::Reached { nodes, cost } = grid
        .find_path([0, 0], [1, 1], SearchBudget::default())
        .unwrap()
    else {
        panic!();
    };
    assert_eq!(nodes, vec![[0, 0], [1, 0], [1, 1]]);
    assert_eq!(cost, 2);
    assert!(
        grid.find_path([2, 0], [1, 1], SearchBudget::default())
            .is_err()
    );
    assert!(grid.set_cost([0, 0], Some(0)).is_err());
}
#[test]
fn graph_finds_cheapest_directed_route_and_distinguishes_unreachable() {
    let graph = Graph::new(vec![vec![(2, 10), (1, 1)], vec![(2, 2)], vec![]]).unwrap();
    assert_eq!(
        graph.find_path(0, 2, SearchBudget::default()).unwrap(),
        PathResult::Reached {
            nodes: vec![0, 1, 2],
            cost: 3
        }
    );
    assert_eq!(
        graph.find_path(2, 0, SearchBudget::default()).unwrap(),
        PathResult::Unreachable
    );
    assert!(Graph::new(vec![vec![(4, 1)]]).is_err());
    assert!(Grid::new(u32::MAX, u32::MAX, vec![], false).is_err());
}
#[test]
fn follower_consumes_distance_across_multiple_segments_without_overshoot() {
    let mut agent = Agent::new(Vec3::ZERO).unwrap();
    agent
        .set_path(vec![Vec3::ZERO, Vec3::X, Vec3::new(1., 2., 0.)])
        .unwrap();
    assert_eq!(agent.advance(1., 2.).unwrap(), Vec3::new(1., 1., 0.));
    assert!(!agent.arrived());
    assert_eq!(agent.advance(1., 2.).unwrap(), Vec3::new(1., 2., 0.));
    assert!(agent.arrived());
    assert!(agent.advance(-1., 2.).is_err());
    assert!(agent.set_path(vec![Vec3::NAN]).is_err());
}
