//! Dynamic obstacle carving and off-mesh links layered over an immutable `NavMesh`.
//!
//! The baked mesh is never modified. A `NavOverlay` keeps a per-polygon block counter and a set
//! of off-mesh links; `NavMesh::find_route` consults it. Carving is therefore per-polygon
//! (cell-resolution) and conservative: any polygon whose cell square overlaps the obstacle
//! footprint inflated by the carve margin is blocked, so a carved path stays outside the
//! inflated footprint by construction, with at most one `cell_size` of extra blocked area.
use super::{NavLocation, NavMesh, NavPath, QuerySettings, valid_point};
use anyhow::{Result, ensure};
use glam::Vec2;
use std::{
    cmp::Reverse,
    collections::{BTreeMap, BinaryHeap},
};

pub const MAX_DYNAMIC_OBSTACLES: usize = 1024;
pub const MAX_OFF_MESH_LINKS: usize = 1024;
/// Fixed-point scale: one polygon hop costs 1000 units. Link costs are rounded to this scale.
const UNIT: u64 = 1000;
const EPS: f32 = 1e-4;

pub type ObstacleId = u32;
pub type LinkId = u32;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DynamicShape {
    Box {
        min: [f32; 3],
        max: [f32; 3],
    },
    /// `base` is the centre of the bottom disc.
    Cylinder {
        base: [f32; 3],
        radius: f32,
        height: f32,
    },
}
impl DynamicShape {
    fn validate(self) -> Result<()> {
        match self {
            Self::Box { min, max } => ensure!(
                valid_point(min) && valid_point(max) && (0..3).all(|i| min[i] <= max[i]),
                "invalid obstacle box"
            ),
            Self::Cylinder {
                base,
                radius,
                height,
            } => ensure!(
                valid_point(base)
                    && radius.is_finite()
                    && (0.0..=10_000.).contains(&radius)
                    && height.is_finite()
                    && (0.0..=10_000.).contains(&height),
                "invalid obstacle cylinder"
            ),
        }
        Ok(())
    }
    fn y_range(self) -> (f32, f32) {
        match self {
            Self::Box { min, max } => (min[1], max[1]),
            Self::Cylinder { base, height, .. } => (base[1], base[1] + height),
        }
    }
    /// XZ bounds inflated by `margin`.
    fn bounds(self, margin: f32) -> (Vec2, Vec2) {
        match self {
            Self::Box { min, max } => (
                Vec2::new(min[0], min[2]) - Vec2::splat(margin),
                Vec2::new(max[0], max[2]) + Vec2::splat(margin),
            ),
            Self::Cylinder { base, radius, .. } => {
                let c = Vec2::new(base[0], base[2]);
                let r = Vec2::splat(radius + margin);
                (c - r, c + r)
            }
        }
    }
    fn overlaps_cell(self, margin: f32, lo: Vec2, hi: Vec2) -> bool {
        match self {
            Self::Box { .. } => {
                let (bmin, bmax) = self.bounds(margin);
                hi.x > bmin.x + EPS
                    && lo.x < bmax.x - EPS
                    && hi.y > bmin.y + EPS
                    && lo.y < bmax.y - EPS
            }
            Self::Cylinder { base, radius, .. } => {
                let c = Vec2::new(base[0], base[2]);
                let q = c.clamp(lo, hi);
                q.distance(c) < radius + margin - EPS
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OffMeshLinkDesc {
    pub start: [f32; 3],
    pub end: [f32; 3],
    /// Traversal cost in polygon-hop units (an ordinary adjacent-polygon step costs 1).
    pub cost: f32,
    pub bidirectional: bool,
    pub enabled: bool,
    /// Maximum distance an endpoint may be from the mesh when snapping.
    pub snap_distance: f32,
}
impl Default for OffMeshLinkDesc {
    fn default() -> Self {
        Self {
            start: [0.; 3],
            end: [0.; 3],
            cost: 1.,
            bidirectional: true,
            enabled: true,
            snap_distance: 1.,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OffMeshLink {
    pub desc: OffMeshLinkDesc,
    pub start: NavLocation,
    pub end: NavLocation,
}

/// A link crossed by a route. `entry_index`/`exit_index` index `NavRoute::path` points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteLink {
    pub link: LinkId,
    pub forward: bool,
    pub entry_index: usize,
    pub exit_index: usize,
}
#[derive(Clone, Debug, PartialEq)]
pub struct NavRoute {
    pub path: NavPath,
    /// Total cost in polygon-hop units (hops plus link costs). Zero unless reached.
    pub cost: f32,
    pub links: Vec<RouteLink>,
}
impl NavRoute {
    fn status(path: NavPath) -> Self {
        Self {
            path,
            cost: 0.,
            links: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
struct ObstacleEntry {
    shape: DynamicShape,
    polygons: Vec<u32>,
}
#[derive(Clone, Debug)]
pub struct NavOverlay {
    polygon_count: usize,
    margin: f32,
    agent_height: f32,
    cell_size: f32,
    cells: BTreeMap<[i32; 2], Vec<u32>>,
    block: Vec<u16>,
    blocked_total: usize,
    obstacles: BTreeMap<ObstacleId, ObstacleEntry>,
    links: BTreeMap<LinkId, OffMeshLink>,
    revision: u64,
}
impl NavOverlay {
    /// The carve margin defaults to the mesh's baked agent radius.
    pub fn new(mesh: &NavMesh) -> Self {
        let mut cells: BTreeMap<[i32; 2], Vec<u32>> = BTreeMap::new();
        for (i, p) in mesh.polygons.iter().enumerate() {
            cells.entry(p.cell).or_default().push(i as u32);
        }
        Self {
            polygon_count: mesh.polygons.len(),
            margin: mesh.settings.agent_radius,
            agent_height: mesh.settings.agent_height,
            cell_size: mesh.settings.cell_size,
            cells,
            block: vec![0; mesh.polygons.len()],
            blocked_total: 0,
            obstacles: BTreeMap::new(),
            links: BTreeMap::new(),
            revision: 0,
        }
    }
    pub fn with_margin(mut self, margin: f32) -> Result<Self> {
        ensure!(
            margin.is_finite() && (0.0..=100.).contains(&margin) && self.obstacles.is_empty(),
            "invalid carve margin"
        );
        self.margin = margin;
        Ok(self)
    }
    pub fn margin(&self) -> f32 {
        self.margin
    }
    /// Increments on every obstacle or link change; compare to detect stale routes.
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn is_blocked(&self, polygon: usize) -> bool {
        self.block.get(polygon).is_some_and(|b| *b > 0)
    }
    pub fn blocked_polygons(&self) -> usize {
        self.blocked_total
    }
    pub fn obstacle_count(&self) -> usize {
        self.obstacles.len()
    }
    pub fn obstacle_ids(&self) -> impl Iterator<Item = ObstacleId> + '_ {
        self.obstacles.keys().copied()
    }
    pub fn obstacle(&self, id: ObstacleId) -> Option<DynamicShape> {
        self.obstacles.get(&id).map(|e| e.shape)
    }
    pub fn link(&self, id: LinkId) -> Option<&OffMeshLink> {
        self.links.get(&id)
    }
    pub fn link_ids(&self) -> impl Iterator<Item = LinkId> + '_ {
        self.links.keys().copied()
    }
    fn check_mesh(&self, mesh: &NavMesh) -> Result<()> {
        ensure!(
            mesh.polygons.len() == self.polygon_count,
            "overlay was built for a different NavMesh"
        );
        Ok(())
    }
    /// Adds (or replaces) an obstacle; returns how many polygons it covers.
    pub fn set_obstacle(
        &mut self,
        mesh: &NavMesh,
        id: ObstacleId,
        shape: DynamicShape,
    ) -> Result<usize> {
        self.check_mesh(mesh)?;
        shape.validate()?;
        ensure!(
            self.obstacles.contains_key(&id) || self.obstacles.len() < MAX_DYNAMIC_OBSTACLES,
            "dynamic obstacle limit reached"
        );
        let polygons = self.covered(mesh, shape);
        self.remove_obstacle(id);
        for &p in &polygons {
            let b = &mut self.block[p as usize];
            if *b == 0 {
                self.blocked_total += 1;
            }
            *b = b.saturating_add(1);
        }
        let count = polygons.len();
        self.obstacles.insert(id, ObstacleEntry { shape, polygons });
        self.revision += 1;
        Ok(count)
    }
    pub fn remove_obstacle(&mut self, id: ObstacleId) -> bool {
        let Some(entry) = self.obstacles.remove(&id) else {
            return false;
        };
        for p in entry.polygons {
            let b = &mut self.block[p as usize];
            *b = b.saturating_sub(1);
            if *b == 0 {
                self.blocked_total -= 1;
            }
        }
        self.revision += 1;
        true
    }
    pub fn clear_obstacles(&mut self) {
        let ids: Vec<_> = self.obstacles.keys().copied().collect();
        for id in ids {
            self.remove_obstacle(id);
        }
    }
    fn covered(&self, mesh: &NavMesh, shape: DynamicShape) -> Vec<u32> {
        let (lo, hi) = shape.bounds(self.margin);
        let s = self.cell_size;
        let c0 = [(lo.x / s).floor() as i32, (lo.y / s).floor() as i32];
        let c1 = [(hi.x / s).floor() as i32, (hi.y / s).floor() as i32];
        let (y0, y1) = shape.y_range();
        let mut out = Vec::new();
        for (cell, polys) in self.cells.range([c0[0], i32::MIN]..=[c1[0], i32::MAX]) {
            if cell[1] < c0[1] || cell[1] > c1[1] {
                continue;
            }
            let lo = Vec2::new(cell[0] as f32, cell[1] as f32) * s;
            if !shape.overlaps_cell(self.margin, lo, lo + Vec2::splat(s)) {
                continue;
            }
            for &p in polys {
                let poly = &mesh.polygons[p as usize];
                let corners = [
                    lo,
                    lo + Vec2::new(s, 0.),
                    lo + Vec2::splat(s),
                    lo + Vec2::new(0., s),
                ]
                .map(|v| poly.height(v));
                let hmin = corners.iter().copied().fold(f32::MAX, f32::min);
                let hmax = corners.iter().copied().fold(f32::MIN, f32::max);
                if y1 >= hmin - EPS && y0 <= hmax + self.agent_height {
                    out.push(p);
                }
            }
        }
        out.sort_unstable();
        out
    }
    /// Adds (or replaces) an off-mesh link; endpoints snap to the nearest mesh location.
    pub fn set_link(&mut self, mesh: &NavMesh, id: LinkId, desc: OffMeshLinkDesc) -> Result<()> {
        self.check_mesh(mesh)?;
        ensure!(
            self.links.contains_key(&id) || self.links.len() < MAX_OFF_MESH_LINKS,
            "off-mesh link limit reached"
        );
        ensure!(
            valid_point(desc.start)
                && valid_point(desc.end)
                && desc.cost.is_finite()
                && (0.0..=1e6).contains(&desc.cost)
                && desc.snap_distance.is_finite()
                && (0.0..=100.).contains(&desc.snap_distance),
            "invalid off-mesh link"
        );
        let query = QuerySettings {
            max_distance: desc.snap_distance,
            ..Default::default()
        };
        let snap = |p| -> Result<NavLocation> {
            mesh.nearest(p, query)?
                .ok_or_else(|| anyhow::anyhow!("off-mesh link endpoint is not near the NavMesh"))
        };
        let link = OffMeshLink {
            desc,
            start: snap(desc.start)?,
            end: snap(desc.end)?,
        };
        self.links.insert(id, link);
        self.revision += 1;
        Ok(())
    }
    pub fn set_link_enabled(&mut self, id: LinkId, enabled: bool) -> Result<()> {
        let link = self
            .links
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("unknown off-mesh link"))?;
        if link.desc.enabled != enabled {
            link.desc.enabled = enabled;
            self.revision += 1;
        }
        Ok(())
    }
    pub fn remove_link(&mut self, id: LinkId) -> bool {
        let removed = self.links.remove(&id).is_some();
        if removed {
            self.revision += 1;
        }
        removed
    }
    /// True if the straight XZ segment passes through a blocked polygon at a compatible height.
    pub fn segment_blocked(&self, mesh: &NavMesh, a: [f32; 3], b: [f32; 3]) -> bool {
        if self.blocked_total == 0 || self.check_mesh(mesh).is_err() {
            return false;
        }
        let (pa, pb) = (glam::Vec3::from(a), glam::Vec3::from(b));
        let step = (self.cell_size * 0.5).max(1e-3);
        let n = ((pb - pa).length() / step).ceil().clamp(1., 100_000.) as u32;
        (0..=n).any(|i| {
            let p = pa.lerp(pb, i as f32 / n as f32);
            let cell = [
                (p.x / self.cell_size).floor() as i32,
                (p.z / self.cell_size).floor() as i32,
            ];
            self.cells.get(&cell).is_some_and(|polys| {
                polys.iter().any(|&q| {
                    self.block[q as usize] > 0
                        && (mesh.polygons[q as usize].height(Vec2::new(p.x, p.z)) - p.y).abs()
                            <= self.agent_height
                })
            })
        })
    }
}

impl NavMesh {
    /// Like `find_path`, honouring `overlay` carving and off-mesh links. With `None` (or an empty
    /// overlay) the polygon route equals `find_path`'s. Costs are polygon hops plus link costs.
    pub fn find_route(
        &self,
        start: [f32; 3],
        goal: [f32; 3],
        query: QuerySettings,
        overlay: Option<&NavOverlay>,
    ) -> Result<NavRoute> {
        query.validate()?;
        ensure!(
            valid_point(start) && valid_point(goal),
            "invalid navigation position"
        );
        if let Some(o) = overlay {
            o.check_mesh(self)?;
        }
        if self.polygons.len() > query.max_polygons {
            return Ok(NavRoute::status(NavPath::BudgetExceeded));
        }
        let blocked = |i: usize| overlay.is_some_and(|o| o.is_blocked(i));
        let (Some(start), Some(goal)) = (
            self.nearest_filtered(start, query, blocked)?,
            self.nearest_filtered(goal, query, blocked)?,
        ) else {
            return Ok(NavRoute::status(NavPath::Unreachable));
        };
        // Adjacency of enabled links, sorted by (polygon, link id) for determinism.
        let mut jumps: BTreeMap<usize, Vec<(LinkId, bool, usize, u64)>> = BTreeMap::new();
        if let Some(o) = overlay {
            for (&id, l) in &o.links {
                if !l.desc.enabled {
                    continue;
                }
                let cost = ((l.desc.cost as f64) * UNIT as f64).round() as u64;
                let cost = cost.max(1);
                jumps
                    .entry(l.start.polygon)
                    .or_default()
                    .push((id, true, l.end.polygon, cost));
                if l.desc.bidirectional {
                    jumps.entry(l.end.polygon).or_default().push((
                        id,
                        false,
                        l.start.polygon,
                        cost,
                    ));
                }
            }
        }
        let use_heuristic = jumps.is_empty();
        let n = self.polygons.len();
        let mut distance = vec![u64::MAX; n];
        // previous polygon and the link (id, forward) used to arrive, if any
        let mut previous: Vec<(usize, Option<(LinkId, bool)>)> = vec![(usize::MAX, None); n];
        let mut queue = BinaryHeap::new();
        queue.push(Reverse((0u64, 0u64, start.polygon)));
        distance[start.polygon] = 0;
        let mut expanded = 0;
        let goal_cell = self.polygons[goal.polygon].cell;
        let relax = |cost: u64,
                     next: usize,
                     from: usize,
                     via: Option<(LinkId, bool)>,
                     distance: &mut Vec<u64>,
                     previous: &mut Vec<(usize, Option<(LinkId, bool)>)>,
                     queue: &mut BinaryHeap<Reverse<(u64, u64, usize)>>| {
            if cost < distance[next] {
                distance[next] = cost;
                previous[next] = (from, via);
                let h = if use_heuristic {
                    let a = self.polygons[next].cell;
                    (((a[0] - goal_cell[0]) as f64).hypot((a[1] - goal_cell[1]) as f64)
                        * UNIT as f64) as u64
                } else {
                    0
                };
                queue.push(Reverse((cost + h, cost, next)));
            }
        };
        while let Some(Reverse((_, cost, node))) = queue.pop() {
            if cost != distance[node] {
                continue;
            }
            if node == goal.polygon {
                return self.assemble_route(start, goal, node, cost, &previous, overlay, query);
            }
            if expanded == query.max_expansions {
                return Ok(NavRoute::status(NavPath::BudgetExceeded));
            }
            expanded += 1;
            for &next in &self.polygons[node].links {
                if !blocked(next) {
                    relax(
                        cost + UNIT,
                        next,
                        node,
                        None,
                        &mut distance,
                        &mut previous,
                        &mut queue,
                    );
                }
            }
            if let Some(list) = jumps.get(&node) {
                for &(id, forward, to, step) in list {
                    if !blocked(to) {
                        relax(
                            cost + step,
                            to,
                            node,
                            Some((id, forward)),
                            &mut distance,
                            &mut previous,
                            &mut queue,
                        );
                    }
                }
            }
        }
        Ok(NavRoute::status(NavPath::Unreachable))
    }
    #[allow(clippy::too_many_arguments)]
    fn assemble_route(
        &self,
        start: NavLocation,
        goal: NavLocation,
        end: usize,
        cost: u64,
        previous: &[(usize, Option<(LinkId, bool)>)],
        overlay: Option<&NavOverlay>,
        query: QuerySettings,
    ) -> Result<NavRoute> {
        // Walk back, splitting the polygon chain into corridors at link edges.
        let mut chain = vec![end];
        let mut via = Vec::new(); // (index in chain after which the link occurs, link)
        let mut cursor = end;
        while cursor != start.polygon {
            let (parent, link) = previous[cursor];
            if let Some(l) = link {
                via.push((chain.len(), l));
            }
            chain.push(parent);
            cursor = parent;
        }
        chain.reverse();
        let len = chain.len();
        // `via` entries counted from the end: the link joins chain[len-1-k-1] -> chain[len-1-k].
        let mut breaks: Vec<(usize, (LinkId, bool))> =
            via.into_iter().map(|(k, l)| (len - k, l)).collect();
        breaks.reverse();
        let mut points: Vec<[f32; 3]> = Vec::new();
        let mut links = Vec::new();
        let mut seg_start = start.position;
        let mut from = 0;
        for (to_index, (id, forward)) in breaks {
            let corridor = &chain[from..to_index];
            let link = overlay
                .and_then(|o| o.links.get(&id))
                .ok_or_else(|| anyhow::anyhow!("route link vanished"))?;
            let (entry, exit) = if forward {
                (link.start.position, link.end.position)
            } else {
                (link.end.position, link.start.position)
            };
            if !self.append_segment(&mut points, corridor, seg_start, entry, query)? {
                return Ok(NavRoute::status(NavPath::BudgetExceeded));
            }
            let entry_index = points.len() - 1;
            links.push(RouteLink {
                link: id,
                forward,
                entry_index,
                exit_index: entry_index + 1,
            });
            seg_start = exit;
            from = to_index;
        }
        if !self.append_segment(&mut points, &chain[from..], seg_start, goal.position, query)? {
            return Ok(NavRoute::status(NavPath::BudgetExceeded));
        }
        if points.len() > query.max_points {
            return Ok(NavRoute::status(NavPath::BudgetExceeded));
        }
        Ok(NavRoute {
            path: NavPath::Reached {
                points,
                polygons: chain.len(),
            },
            cost: cost as f32 / UNIT as f32,
            links,
        })
    }
    /// Appends one corridor's points; false if the point budget is exceeded.
    fn append_segment(
        &self,
        points: &mut Vec<[f32; 3]>,
        corridor: &[usize],
        from: [f32; 3],
        to: [f32; 3],
        query: QuerySettings,
    ) -> Result<bool> {
        match self.path_points(corridor, from, to, query)? {
            NavPath::Reached { points: p, .. } => {
                points.extend(p);
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}
