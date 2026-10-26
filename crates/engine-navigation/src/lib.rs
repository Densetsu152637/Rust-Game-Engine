//! Bounded deterministic grid, graph and baked polygon navigation.
mod crowd;
mod navmesh;
use anyhow::{Result, ensure};
pub use crowd::*;
use glam::Vec3;
pub use navmesh::*;
use std::{cmp::Reverse, collections::BinaryHeap};
#[derive(Clone, Copy, Debug)]
pub struct SearchBudget {
    pub max_expansions: usize,
}
impl Default for SearchBudget {
    fn default() -> Self {
        Self {
            max_expansions: 100_000,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathResult<T> {
    Reached { nodes: Vec<T>, cost: u64 },
    Unreachable,
    BudgetExceeded,
}
fn search(
    count: usize,
    start: usize,
    goal: usize,
    budget: SearchBudget,
    neighbors: impl Fn(usize) -> Vec<(usize, u32)>,
    heuristic: impl Fn(usize) -> u64,
) -> PathResult<usize> {
    let mut distances = vec![u64::MAX; count];
    let mut previous = vec![None; count];
    let mut queue = BinaryHeap::new();
    distances[start] = 0;
    queue.push(Reverse((heuristic(start), 0_u64, start)));
    let mut expanded = 0;
    while let Some(Reverse((_, cost, node))) = queue.pop() {
        if cost != distances[node] {
            continue;
        }
        if node == goal {
            let mut path = vec![goal];
            let mut cursor = goal;
            while let Some(parent) = previous[cursor] {
                path.push(parent);
                cursor = parent;
            }
            path.reverse();
            return PathResult::Reached { nodes: path, cost };
        }
        if expanded >= budget.max_expansions {
            return PathResult::BudgetExceeded;
        }
        expanded += 1;
        for (next, step) in neighbors(node) {
            let candidate = cost + u64::from(step);
            if candidate < distances[next] {
                distances[next] = candidate;
                previous[next] = Some(node);
                queue.push(Reverse((candidate + heuristic(next), candidate, next)));
            }
        }
    }
    PathResult::Unreachable
}
#[derive(Clone, Debug)]
pub struct Grid {
    width: u32,
    height: u32,
    costs: Vec<Option<u32>>,
    diagonal: bool,
}
impl Grid {
    /// None is blocked; Some(cost) is the positive cost to enter a cell.
    pub fn new(width: u32, height: u32, costs: Vec<Option<u32>>, diagonal: bool) -> Result<Self> {
        let count = u64::from(width) * u64::from(height);
        ensure!(
            width > 0 && height > 0 && count <= 1_000_000 && costs.len() as u64 == count,
            "invalid grid dimensions"
        );
        ensure!(
            costs.iter().all(|c| c.is_none_or(|v| v > 0)),
            "traversal cost must be positive"
        );
        Ok(Self {
            width,
            height,
            costs,
            diagonal,
        })
    }
    fn index(&self, p: [u32; 2]) -> Result<usize> {
        ensure!(p[0] < self.width && p[1] < self.height, "cell outside grid");
        Ok((p[1] * self.width + p[0]) as usize)
    }
    fn point(&self, i: usize) -> [u32; 2] {
        [i as u32 % self.width, i as u32 / self.width]
    }
    pub fn set_cost(&mut self, p: [u32; 2], cost: Option<u32>) -> Result<()> {
        ensure!(
            cost.is_none_or(|v| v > 0),
            "traversal cost must be positive"
        );
        let i = self.index(p)?;
        self.costs[i] = cost;
        Ok(())
    }
    pub fn find_path(
        &self,
        start: [u32; 2],
        goal: [u32; 2],
        budget: SearchBudget,
    ) -> Result<PathResult<[u32; 2]>> {
        let a = self.index(start)?;
        let b = self.index(goal)?;
        if self.costs[a].is_none() || self.costs[b].is_none() {
            return Ok(PathResult::Unreachable);
        }
        let minimum = self.costs.iter().flatten().copied().min().unwrap_or(1) as u64;
        let result = search(
            self.costs.len(),
            a,
            b,
            budget,
            |i| self.neighbors(i),
            |i| {
                let p = self.point(i);
                let dx = u64::from(p[0].abs_diff(goal[0]));
                let dy = u64::from(p[1].abs_diff(goal[1]));
                (if self.diagonal { dx.max(dy) } else { dx + dy }) * minimum
            },
        );
        Ok(match result {
            PathResult::Reached { nodes, cost } => PathResult::Reached {
                nodes: nodes.into_iter().map(|i| self.point(i)).collect(),
                cost,
            },
            PathResult::Unreachable => PathResult::Unreachable,
            PathResult::BudgetExceeded => PathResult::BudgetExceeded,
        })
    }
    fn neighbors(&self, i: usize) -> Vec<(usize, u32)> {
        let [x, y] = self.point(i);
        let mut result = Vec::with_capacity(8);
        for dy in -1_i64..=1 {
            for dx in -1_i64..=1 {
                if (dx == 0 && dy == 0) || (!self.diagonal && dx != 0 && dy != 0) {
                    continue;
                }
                let nx = i64::from(x) + dx;
                let ny = i64::from(y) + dy;
                if nx < 0 || ny < 0 || nx >= i64::from(self.width) || ny >= i64::from(self.height) {
                    continue;
                }
                if dx != 0 && dy != 0 {
                    let horizontal = (y * self.width + nx as u32) as usize;
                    let vertical = (ny as u32 * self.width + x) as usize;
                    if self.costs[horizontal].is_none() || self.costs[vertical].is_none() {
                        continue;
                    }
                }
                let next = (ny as u32 * self.width + nx as u32) as usize;
                if let Some(cost) = self.costs[next] {
                    result.push((next, cost));
                }
            }
        }
        result
    }
}
#[derive(Clone, Debug)]
pub struct Graph {
    edges: Vec<Vec<(usize, u32)>>,
}
impl Graph {
    pub fn new(mut edges: Vec<Vec<(usize, u32)>>) -> Result<Self> {
        ensure!(
            !edges.is_empty()
                && edges.len() <= 1_000_000
                && edges.iter().map(Vec::len).sum::<usize>() <= 8_000_000,
            "graph exceeds limits"
        );
        ensure!(
            edges
                .iter()
                .flatten()
                .all(|(i, c)| *i < edges.len() && *c > 0),
            "invalid edge"
        );
        for neighbors in &mut edges {
            neighbors.sort_unstable();
            neighbors.dedup();
        }
        Ok(Self { edges })
    }
    /// Dijkstra is A* with zero heuristic; arbitrary graph costs need no spatial assumption.
    pub fn find_path(
        &self,
        start: usize,
        goal: usize,
        budget: SearchBudget,
    ) -> Result<PathResult<usize>> {
        ensure!(
            start < self.edges.len() && goal < self.edges.len(),
            "invalid node"
        );
        Ok(search(
            self.edges.len(),
            start,
            goal,
            budget,
            |i| self.edges[i].clone(),
            |_| 0,
        ))
    }
}
#[derive(Clone, Debug)]
pub struct Agent {
    position: Vec3,
    path: Vec<Vec3>,
    next: usize,
}
impl Agent {
    pub fn new(position: Vec3) -> Result<Self> {
        ensure!(
            position.is_finite() && position.abs().max_element() <= 1e8,
            "invalid position"
        );
        Ok(Self {
            position,
            path: Vec::new(),
            next: 0,
        })
    }
    pub fn set_path(&mut self, path: Vec<Vec3>) -> Result<()> {
        ensure!(
            path.len() <= 1_000_000
                && path
                    .iter()
                    .all(|p| p.is_finite() && p.abs().max_element() <= 1e8),
            "invalid path"
        );
        self.path = path;
        self.next = 0;
        Ok(())
    }
    pub fn position(&self) -> Vec3 {
        self.position
    }
    /// Teleports the follower without touching its path (used by steering integrators).
    pub fn set_position(&mut self, position: Vec3) -> Result<()> {
        ensure!(
            position.is_finite() && position.abs().max_element() <= 1e8,
            "invalid position"
        );
        self.position = position;
        Ok(())
    }
    pub fn path(&self) -> &[Vec3] {
        &self.path
    }
    /// Index of the waypoint currently being approached; `path().len()` when arrived.
    pub fn next_index(&self) -> usize {
        self.next
    }
    /// The waypoint currently being approached, if any.
    pub fn target(&self) -> Option<Vec3> {
        self.path.get(self.next).copied()
    }
    /// Marks the current waypoint as reached without moving.
    pub fn skip_waypoint(&mut self) {
        self.next = (self.next + 1).min(self.path.len());
    }
    pub fn arrived(&self) -> bool {
        self.next == self.path.len()
    }
    pub fn advance(&mut self, dt: f32, speed: f32) -> Result<Vec3> {
        ensure!(
            dt.is_finite()
                && (0.0..=60.).contains(&dt)
                && speed.is_finite()
                && (0.0..=1e6).contains(&speed),
            "invalid movement"
        );
        let mut remaining = dt * speed;
        while self.next < self.path.len() {
            let delta = self.path[self.next] - self.position;
            let length = delta.length();
            ensure!(length.is_finite(), "path distance overflow");
            if length <= remaining {
                self.position = self.path[self.next];
                self.next += 1;
                remaining -= length;
            } else {
                self.position += delta * (remaining / length);
                break;
            }
        }
        Ok(self.position)
    }
}
