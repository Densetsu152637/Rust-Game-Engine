//! Conservative layered heightfield bake into convex polygons with shared portals.
use anyhow::{Result, ensure};
use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};
use std::{
    cmp::Reverse,
    collections::{BTreeMap, BinaryHeap},
};

mod bake;
mod funnel;
mod overlay;
pub use bake::bake;
pub use overlay::*;

pub const MAX_POLYGONS: usize = 65_536;
const MAX_WORK: usize = 4_000_000;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BakeSettings {
    pub cell_size: f32,
    pub agent_radius: f32,
    pub agent_height: f32,
    pub max_slope_degrees: f32,
    pub max_step: f32,
}
impl Default for BakeSettings {
    fn default() -> Self {
        Self {
            cell_size: 0.25,
            agent_radius: 0.25,
            agent_height: 1.8,
            max_slope_degrees: 45.,
            max_step: 0.3,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Obstacle {
    pub min: [f32; 3],
    pub max: [f32; 3],
}
/// Explicit walkable surfaces, plus solid boxes (including ceiling/headroom blockers).
/// Winding does not matter. No overlapping floors within a layer are permitted.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BakeInput {
    pub vertices: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
    #[serde(default)]
    pub obstacles: Vec<Obstacle>,
    #[serde(default)]
    pub settings: BakeSettings,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QuerySettings {
    pub max_distance: f32,
    pub max_polygons: usize,
    pub max_expansions: usize,
    pub max_points: usize,
}
impl Default for QuerySettings {
    fn default() -> Self {
        Self {
            max_distance: 2.,
            max_polygons: MAX_POLYGONS,
            max_expansions: 16_384,
            max_points: 4096,
        }
    }
}
impl QuerySettings {
    pub fn validate(self) -> Result<()> {
        ensure!(
            self.max_distance.is_finite()
                && (0.0..=1000.).contains(&self.max_distance)
                && self.max_polygons > 0
                && self.max_polygons <= MAX_POLYGONS
                && self.max_expansions <= MAX_POLYGONS
                && self.max_points >= 2
                && self.max_points <= 65_536,
            "invalid navigation query limits"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum NavPath {
    Reached {
        points: Vec<[f32; 3]>,
        polygons: usize,
    },
    Unreachable,
    BudgetExceeded,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq)]
pub struct NavLocation {
    pub polygon: usize,
    pub position: [f32; 3],
}
#[derive(Clone, Debug)]
struct Polygon {
    cell: [i32; 2],
    plane: [f32; 3],
    /// Lowest overhead surface over this cell, including partial ceilings.
    ceiling: f64,
    links: Vec<usize>,
}
impl Polygon {
    fn height(&self, p: Vec2) -> f32 {
        self.plane[0] * p.x + self.plane[1] * p.y + self.plane[2]
    }
    fn point(&self, p: Vec2) -> [f32; 3] {
        [p.x, self.height(p), p.y]
    }
}
/// Immutable baked topology. Deserialization is deliberately unavailable: all topology
/// is constructed by validation and baking, never trusted from an asset payload.
#[derive(Clone, Debug)]
pub struct NavMesh {
    polygons: Vec<Polygon>,
    settings: BakeSettings,
}
impl NavMesh {
    pub fn polygon_count(&self) -> usize {
        self.polygons.len()
    }
    pub fn settings(&self) -> &BakeSettings {
        &self.settings
    }
    pub fn nearest(&self, position: [f32; 3], query: QuerySettings) -> Result<Option<NavLocation>> {
        self.nearest_filtered(position, query, |_| false)
    }
    /// `nearest` that ignores polygons for which `skip` returns true (dynamic carving).
    fn nearest_filtered(
        &self,
        position: [f32; 3],
        query: QuerySettings,
        skip: impl Fn(usize) -> bool,
    ) -> Result<Option<NavLocation>> {
        query.validate()?;
        ensure!(valid_point(position), "invalid navigation position");
        ensure!(
            self.polygons.len() <= query.max_polygons,
            "nearest polygon budget exceeded"
        );
        let p = Vec3::from(position);
        let mut best = None;
        let mut distance = query.max_distance * query.max_distance;
        for (index, polygon) in self.polygons.iter().enumerate() {
            if skip(index) {
                continue;
            }
            let min =
                Vec2::new(polygon.cell[0] as f32, polygon.cell[1] as f32) * self.settings.cell_size;
            let size = self.settings.cell_size;
            let corners = [
                min,
                min + Vec2::new(size, 0.),
                min + Vec2::splat(size),
                min + Vec2::new(0., size),
            ]
            .map(|v| Vec3::from(polygon.point(v)));
            let a = closest_triangle(p, corners[0], corners[1], corners[2]);
            let b = closest_triangle(p, corners[0], corners[2], corners[3]);
            let candidate = if a.distance_squared(p) <= b.distance_squared(p) {
                a
            } else {
                b
            };
            let d = candidate.distance_squared(p);
            if d <= distance {
                distance = d;
                best = Some(NavLocation {
                    polygon: index,
                    position: candidate.to_array(),
                });
            }
        }
        Ok(best)
    }
    pub fn find_path(
        &self,
        start: [f32; 3],
        goal: [f32; 3],
        query: QuerySettings,
    ) -> Result<NavPath> {
        query.validate()?;
        ensure!(
            valid_point(start) && valid_point(goal),
            "invalid navigation position"
        );
        if self.polygons.len() > query.max_polygons {
            return Ok(NavPath::BudgetExceeded);
        }
        let (Some(start), Some(goal)) = (self.nearest(start, query)?, self.nearest(goal, query)?)
        else {
            return Ok(NavPath::Unreachable);
        };
        let mut distance = vec![u64::MAX; self.polygons.len()];
        let mut previous = vec![usize::MAX; self.polygons.len()];
        let mut queue = BinaryHeap::new();
        queue.push(Reverse((0u64, 0u64, start.polygon)));
        distance[start.polygon] = 0;
        let mut expanded = 0;
        while let Some(Reverse((_, cost, node))) = queue.pop() {
            if cost != distance[node] {
                continue;
            }
            if node == goal.polygon {
                let mut corridor = vec![node];
                while *corridor.last().unwrap() != start.polygon {
                    corridor.push(previous[*corridor.last().unwrap()]);
                }
                corridor.reverse();
                return self.path_points(&corridor, start.position, goal.position, query);
            }
            if expanded == query.max_expansions {
                return Ok(NavPath::BudgetExceeded);
            }
            expanded += 1;
            for &next in &self.polygons[node].links {
                if cost + 1 < distance[next] {
                    distance[next] = cost + 1;
                    previous[next] = node;
                    let a = self.polygons[next].cell;
                    let b = self.polygons[goal.polygon].cell;
                    let heuristic = ((a[0] - b[0]) as f64).hypot((a[1] - b[1]) as f64);
                    queue.push(Reverse((
                        (cost + 1) * 1000 + (heuristic * 1000.) as u64,
                        cost + 1,
                        next,
                    )));
                }
            }
        }
        Ok(NavPath::Unreachable)
    }
    fn portal(&self, from: usize, to: usize) -> (Vec2, Vec2) {
        let a = &self.polygons[from];
        let b = &self.polygons[to];
        let s = self.settings.cell_size;
        let p = Vec2::new(a.cell[0] as f32, a.cell[1] as f32) * s;
        match (b.cell[0] - a.cell[0], b.cell[1] - a.cell[1]) {
            (1, 0) => (p + Vec2::new(s, s), p + Vec2::new(s, 0.)),
            (-1, 0) => (p, p + Vec2::new(0., s)),
            (0, 1) => (p + Vec2::new(0., s), p + Vec2::new(s, s)),
            _ => (p + Vec2::new(s, 0.), p),
        }
    }
    fn path_points(
        &self,
        corridor: &[usize],
        start: [f32; 3],
        goal: [f32; 3],
        query: QuerySettings,
    ) -> Result<NavPath> {
        let xz = |p: [f32; 3]| Vec2::new(p[0], p[2]);
        let mut portals = vec![(xz(start), xz(start))];
        portals.extend(
            corridor
                .windows(2)
                .map(|pair| self.portal(pair[0], pair[1])),
        );
        portals.push((xz(goal), xz(goal)));
        let Some(corners) = funnel::pull(&portals, query.max_points) else {
            return Ok(NavPath::BudgetExceeded);
        };
        // Lift each crossing onto its source and destination planes. This preserves
        // ramps and explicit step transitions instead of cutting through terrain.
        let mut points = vec![start];
        let mut segment = 0;
        for (index, &(left, right)) in portals.iter().enumerate().skip(1).take(corridor.len() - 1) {
            loop {
                if segment + 1 >= corners.len() {
                    anyhow::bail!("invalid funnel corridor");
                }
                if let Some(p) =
                    funnel::crossing(corners[segment], corners[segment + 1], left, right)
                {
                    push_point(&mut points, self.polygons[corridor[index - 1]].point(p));
                    push_point(&mut points, self.polygons[corridor[index]].point(p));
                    break;
                }
                segment += 1;
            }
            if points.len() > query.max_points {
                return Ok(NavPath::BudgetExceeded);
            }
        }
        push_point(&mut points, goal);
        // Remove only collinear 3D points; terrain slope/step corners remain.
        let mut smooth: Vec<[f32; 3]> = Vec::new();
        for p in points {
            while smooth.len() >= 2 {
                let a = Vec3::from(smooth[smooth.len() - 2]);
                let b = Vec3::from(smooth[smooth.len() - 1]);
                let c = Vec3::from(p);
                if (b - a).cross(c - b).length_squared() > 1e-10 || (b - a).dot(c - b) < 0. {
                    break;
                }
                smooth.pop();
            }
            smooth.push(p);
        }
        if smooth.len() > query.max_points {
            return Ok(NavPath::BudgetExceeded);
        }
        Ok(NavPath::Reached {
            points: smooth,
            polygons: corridor.len(),
        })
    }
}
fn push_point(points: &mut Vec<[f32; 3]>, p: [f32; 3]) {
    if points
        .last()
        .is_none_or(|q| Vec3::from(*q).distance_squared(Vec3::from(p)) > 1e-12)
    {
        points.push(p);
    }
}
fn valid_point(p: [f32; 3]) -> bool {
    p.iter().all(|v| v.is_finite() && v.abs() <= 100_000.)
}

fn closest_triangle(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Vec3 {
    let ab = b - a;
    let ac = c - a;
    let normal = ab.cross(ac);
    let projection = p - normal * ((p - a).dot(normal) / normal.length_squared());
    let inside = [(a, b), (b, c), (c, a)]
        .iter()
        .all(|(x, y)| (*y - *x).cross(projection - *x).dot(normal) >= 0.);
    if inside {
        return projection;
    }
    let mut nearest = a;
    let mut distance = p.distance_squared(a);
    for (x, y) in [(a, b), (b, c), (c, a)] {
        let edge = y - x;
        let q = x + edge * ((p - x).dot(edge) / edge.length_squared()).clamp(0., 1.);
        let d = p.distance_squared(q);
        if d < distance {
            nearest = q;
            distance = d;
        }
    }
    nearest
}
