//! Deterministic ORCA (RVO2-style) crowd steering over NavMesh paths.
//!
//! Avoidance is planar on XZ; Y follows the agent's path. All iteration orders are fixed
//! (agents by id, neighbours by distance then index, spatial hash by sorted cell), there is
//! no HashMap iteration and no randomness, so identical inputs give bitwise-identical output.
#![allow(clippy::needless_range_loop)] // loops index parallel per-agent arrays
use crate::{Agent, LinkId, NavMesh, NavOverlay, NavPath, QuerySettings, RouteLink};
use anyhow::{Result, ensure};
use glam::{Vec2, Vec3};
use std::collections::BTreeMap;

mod formation;
mod orca;
mod spatial;
pub use formation::{FormationAnchor, FormationDesc, MAX_FORMATION_SLOTS};
use orca::{Line, pair_line, solve};
use spatial::SpatialHash;

pub const MAX_CROWD_AGENTS: usize = 4096;
pub const MAX_CROWD_NEIGHBORS: usize = 64;
pub const MAX_CROWD_SEGMENTS: usize = 1024;
pub const MAX_CROWD_FORMATIONS: usize = 256;
const MAX_OBSTACLE_LINES: usize = 16;

pub type AgentId = u32;
pub type SegmentId = u32;
pub type FormationId = u32;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CrowdSettings {
    pub max_agents: usize,
    /// Neighbours considered per agent (nearest first).
    pub max_neighbors: usize,
    /// Neighbour search radius (also the spatial hash cell size).
    pub neighbor_distance: f32,
    /// Seconds of look-ahead for agent-agent avoidance.
    pub time_horizon: f32,
    /// Seconds of look-ahead for static segments.
    pub obstacle_time_horizon: f32,
    /// Extra clearance added to the combined radius in avoidance constraints.
    pub safety_margin: f32,
    /// Distance at which an intermediate waypoint counts as reached.
    pub waypoint_tolerance: f32,
    /// Deterministic per-agent preferred-velocity jitter (fraction of max speed) that breaks
    /// the exact symmetry of head-on encounters.
    pub symmetry_nudge: f32,
    /// Hard overlap resolution passes after integration (0 disables).
    pub contact_iterations: u32,
    /// Repath agents whose route is invalidated when a `NavContext` is supplied to `step`.
    pub auto_repath: bool,
}
impl Default for CrowdSettings {
    fn default() -> Self {
        Self {
            max_agents: 256,
            max_neighbors: 12,
            neighbor_distance: 5.,
            time_horizon: 2.,
            obstacle_time_horizon: 1.,
            safety_margin: 0.02,
            waypoint_tolerance: 0.1,
            symmetry_nudge: 0.02,
            contact_iterations: 4,
            auto_repath: true,
        }
    }
}
impl CrowdSettings {
    pub fn validate(self) -> Result<()> {
        let finite = |v: f32, lo: f32, hi: f32| v.is_finite() && (lo..=hi).contains(&v);
        ensure!(
            (1..=MAX_CROWD_AGENTS).contains(&self.max_agents)
                && (1..=MAX_CROWD_NEIGHBORS).contains(&self.max_neighbors)
                && finite(self.neighbor_distance, 0.1, 1000.)
                && finite(self.time_horizon, 0.05, 60.)
                && finite(self.obstacle_time_horizon, 0.05, 60.)
                && finite(self.safety_margin, 0., 10.)
                && finite(self.waypoint_tolerance, 0.001, 100.)
                && finite(self.symmetry_nudge, 0., 1.)
                && self.contact_iterations <= 16,
            "invalid crowd settings"
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AgentDesc {
    pub position: Vec3,
    pub radius: f32,
    pub max_speed: f32,
    /// Distance from the final goal at which the agent counts as arrived.
    pub arrival_radius: f32,
}
impl Default for AgentDesc {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            radius: 0.4,
            max_speed: 2.,
            arrival_radius: 0.1,
        }
    }
}

/// Navigation data an update may consult: mesh, optional carving/link overlay, query limits.
#[derive(Clone, Copy)]
pub struct NavContext<'a> {
    pub mesh: &'a NavMesh,
    pub overlay: Option<&'a NavOverlay>,
    pub query: QuerySettings,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoalOutcome {
    Reached,
    Unreachable,
    BudgetExceeded,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrowdEvent {
    Arrived {
        agent: AgentId,
    },
    LinkEntered {
        agent: AgentId,
        link: LinkId,
    },
    LinkExited {
        agent: AgentId,
        link: LinkId,
    },
    /// The route was invalidated (new obstacle, disabled link) and a new one was found.
    Repathed {
        agent: AgentId,
    },
    /// The route was invalidated and no new route exists; the agent stops.
    PathFailed {
        agent: AgentId,
    },
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CrowdStats {
    pub tick: u64,
    pub agents: usize,
    pub neighbor_pairs: usize,
    /// Hard overlap corrections applied by the last step (ORCA should keep this at or near 0).
    pub contact_corrections: usize,
}

#[derive(Clone, Debug)]
struct CrowdAgent {
    id: AgentId,
    follower: Agent,
    velocity: Vec2,
    radius: f32,
    max_speed: f32,
    arrival_radius: f32,
    /// Last requested navmesh goal (kept for repathing).
    goal: Option<Vec3>,
    /// Direct steering point: formation slot, held final goal, or explicit target.
    target: Option<Vec3>,
    origin: Vec3,
    links: Vec<RouteLink>,
    link_active: Option<usize>,
    arrived: bool,
    failed: bool,
    route_revision: u64,
}
/// Read-only view of an agent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AgentView {
    pub id: AgentId,
    pub position: Vec3,
    pub velocity: Vec2,
    pub radius: f32,
    pub max_speed: f32,
    pub arrived: bool,
    pub goal: Option<Vec3>,
    pub waypoints_remaining: usize,
    pub on_link: Option<LinkId>,
}
#[derive(Clone, Debug)]
struct FormationState {
    desc: FormationDesc,
    yaw: f32,
    /// (agent, slot) pairs sorted by agent id.
    members: Vec<(AgentId, usize)>,
}

#[derive(Clone, Debug)]
pub struct Crowd {
    settings: CrowdSettings,
    agents: Vec<CrowdAgent>,
    next_id: AgentId,
    segments: BTreeMap<SegmentId, [Vec2; 2]>,
    formations: BTreeMap<FormationId, FormationState>,
    stats: CrowdStats,
}

fn xz(v: Vec3) -> Vec2 {
    Vec2::new(v.x, v.z)
}
fn closest_on_segment(p: Vec2, s: [Vec2; 2]) -> Vec2 {
    let d = s[1] - s[0];
    let l = d.length_squared();
    if l <= 1e-12 {
        return s[0];
    }
    s[0] + d * ((p - s[0]).dot(d) / l).clamp(0., 1.)
}
fn nudge(id: AgentId) -> Vec2 {
    let h = id.wrapping_mul(2_654_435_761) >> 8;
    let angle = (h & 0xFFFF) as f32 / 65_536. * std::f32::consts::TAU;
    Vec2::new(angle.cos(), angle.sin())
}

impl Crowd {
    pub fn new(settings: CrowdSettings) -> Result<Self> {
        settings.validate()?;
        Ok(Self {
            settings,
            agents: Vec::new(),
            next_id: 0,
            segments: BTreeMap::new(),
            formations: BTreeMap::new(),
            stats: CrowdStats::default(),
        })
    }
    pub fn settings(&self) -> CrowdSettings {
        self.settings
    }
    pub fn stats(&self) -> CrowdStats {
        self.stats
    }
    pub fn len(&self) -> usize {
        self.agents.len()
    }
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }
    fn index(&self, id: AgentId) -> Option<usize> {
        self.agents.binary_search_by_key(&id, |a| a.id).ok()
    }
    pub fn add_agent(&mut self, desc: AgentDesc) -> Result<AgentId> {
        ensure!(
            self.agents.len() < self.settings.max_agents,
            "crowd agent limit reached"
        );
        ensure!(
            desc.radius.is_finite()
                && (0.01..=100.).contains(&desc.radius)
                && desc.max_speed.is_finite()
                && (0.0..=1000.).contains(&desc.max_speed)
                && desc.arrival_radius.is_finite()
                && (0.001..=100.).contains(&desc.arrival_radius),
            "invalid crowd agent"
        );
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("agent ids exhausted"))?;
        self.agents.push(CrowdAgent {
            id,
            follower: Agent::new(desc.position)?,
            velocity: Vec2::ZERO,
            radius: desc.radius,
            max_speed: desc.max_speed,
            arrival_radius: desc.arrival_radius,
            goal: None,
            target: None,
            origin: desc.position,
            links: Vec::new(),
            link_active: None,
            arrived: false,
            failed: false,
            route_revision: 0,
        });
        Ok(id)
    }
    pub fn remove_agent(&mut self, id: AgentId) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        self.agents.remove(i);
        for f in self.formations.values_mut() {
            f.members.retain(|(a, _)| *a != id);
        }
        true
    }
    pub fn agent(&self, id: AgentId) -> Option<AgentView> {
        let a = &self.agents[self.index(id)?];
        Some(AgentView {
            id,
            position: a.follower.position(),
            velocity: a.velocity,
            radius: a.radius,
            max_speed: a.max_speed,
            arrived: a.arrived,
            goal: a.goal.or(a.target),
            waypoints_remaining: a.follower.path().len() - a.follower.next_index(),
            on_link: a.link_active.map(|k| a.links[k].link),
        })
    }
    pub fn agent_ids(&self) -> impl Iterator<Item = AgentId> + '_ {
        self.agents.iter().map(|a| a.id)
    }
    /// `(id, position)` in ascending id order.
    pub fn positions(&self) -> Vec<(AgentId, Vec3)> {
        self.agents
            .iter()
            .map(|a| (a.id, a.follower.position()))
            .collect()
    }
    pub fn set_agent_params(&mut self, id: AgentId, radius: f32, max_speed: f32) -> Result<()> {
        ensure!(
            radius.is_finite()
                && (0.01..=100.).contains(&radius)
                && max_speed.is_finite()
                && (0.0..=1000.).contains(&max_speed),
            "invalid crowd agent"
        );
        let i = self
            .index(id)
            .ok_or_else(|| anyhow::anyhow!("unknown crowd agent"))?;
        self.agents[i].radius = radius;
        self.agents[i].max_speed = max_speed;
        Ok(())
    }
    pub fn teleport(&mut self, id: AgentId, position: Vec3) -> Result<()> {
        let i = self
            .index(id)
            .ok_or_else(|| anyhow::anyhow!("unknown crowd agent"))?;
        self.agents[i].follower.set_position(position)?;
        self.agents[i].velocity = Vec2::ZERO;
        Ok(())
    }

    /// Paths the agent to `goal` over the NavMesh (honouring carving and links).
    pub fn set_goal(&mut self, id: AgentId, goal: Vec3, nav: &NavContext) -> Result<GoalOutcome> {
        let i = self
            .index(id)
            .ok_or_else(|| anyhow::anyhow!("unknown crowd agent"))?;
        ensure!(goal.is_finite(), "invalid goal");
        let a = &mut self.agents[i];
        a.goal = Some(goal);
        a.arrived = false;
        a.target = None;
        Self::route(a, nav)
    }
    fn route(a: &mut CrowdAgent, nav: &NavContext) -> Result<GoalOutcome> {
        let goal = a.goal.expect("route requires a goal");
        let position = a.follower.position();
        a.route_revision = nav.overlay.map_or(0, NavOverlay::revision);
        a.link_active = None;
        let route =
            nav.mesh
                .find_route(position.to_array(), goal.to_array(), nav.query, nav.overlay)?;
        Ok(match route.path {
            NavPath::Reached { points, .. } => {
                a.follower
                    .set_path(points.into_iter().map(Vec3::from).collect())?;
                a.origin = position;
                a.links = route.links;
                a.failed = false;
                GoalOutcome::Reached
            }
            other => {
                a.follower.set_path(Vec::new())?;
                a.links.clear();
                a.target = None;
                a.failed = true;
                if other == NavPath::Unreachable {
                    GoalOutcome::Unreachable
                } else {
                    GoalOutcome::BudgetExceeded
                }
            }
        })
    }
    /// Follows explicit waypoints (no navmesh). Clears any goal.
    pub fn set_path(&mut self, id: AgentId, waypoints: Vec<Vec3>) -> Result<()> {
        let i = self
            .index(id)
            .ok_or_else(|| anyhow::anyhow!("unknown crowd agent"))?;
        let a = &mut self.agents[i];
        a.follower.set_path(waypoints)?;
        a.origin = a.follower.position();
        a.goal = None;
        a.target = None;
        a.links.clear();
        a.link_active = None;
        a.arrived = false;
        a.failed = false;
        Ok(())
    }
    /// Steers straight at `target` (no path). `None` stops steering.
    pub fn set_target(&mut self, id: AgentId, target: Option<Vec3>) -> Result<()> {
        let i = self
            .index(id)
            .ok_or_else(|| anyhow::anyhow!("unknown crowd agent"))?;
        ensure!(target.is_none_or(Vec3::is_finite), "invalid target");
        let a = &mut self.agents[i];
        a.follower.set_path(Vec::new())?;
        a.goal = None;
        a.links.clear();
        a.link_active = None;
        a.arrived = false;
        a.failed = false;
        a.origin = a.follower.position();
        a.target = target;
        Ok(())
    }
    pub fn clear_goal(&mut self, id: AgentId) -> Result<()> {
        self.set_target(id, None)
    }

    pub fn set_segment(&mut self, id: SegmentId, a: [f32; 2], b: [f32; 2]) -> Result<()> {
        ensure!(
            self.segments.contains_key(&id) || self.segments.len() < MAX_CROWD_SEGMENTS,
            "crowd obstacle segment limit reached"
        );
        ensure!(
            a.iter().chain(&b).all(|v| v.is_finite() && v.abs() <= 1e6),
            "invalid obstacle segment"
        );
        self.segments.insert(id, [Vec2::from(a), Vec2::from(b)]);
        Ok(())
    }
    pub fn remove_segment(&mut self, id: SegmentId) -> bool {
        self.segments.remove(&id).is_some()
    }
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    pub fn set_formation(&mut self, id: FormationId, desc: FormationDesc) -> Result<()> {
        ensure!(
            self.formations.contains_key(&id) || self.formations.len() < MAX_CROWD_FORMATIONS,
            "formation limit reached"
        );
        ensure!(
            desc.slots.len() <= MAX_FORMATION_SLOTS
                && desc.yaw.is_finite()
                && desc
                    .slots
                    .iter()
                    .flatten()
                    .all(|v| v.is_finite() && v.abs() <= 1e4)
                && match desc.anchor {
                    FormationAnchor::Fixed(p) => p.iter().all(|v| v.is_finite() && v.abs() <= 1e6),
                    FormationAnchor::Leader(_) => true,
                },
            "invalid formation"
        );
        let members = self
            .formations
            .get(&id)
            .map(|f| f.members.clone())
            .unwrap_or_default();
        let members = members
            .into_iter()
            .filter(|(_, s)| *s < desc.slots.len())
            .collect();
        self.formations.insert(
            id,
            FormationState {
                yaw: desc.yaw,
                desc,
                members,
            },
        );
        Ok(())
    }
    pub fn remove_formation(&mut self, id: FormationId) -> bool {
        self.formations.remove(&id).is_some()
    }
    pub fn formation_members(&self, id: FormationId) -> Option<&[(AgentId, usize)]> {
        self.formations.get(&id).map(|f| f.members.as_slice())
    }
    fn formation_anchor(&self, f: &FormationState) -> Option<[f32; 3]> {
        match f.desc.anchor {
            FormationAnchor::Fixed(p) => Some(p),
            FormationAnchor::Leader(id) => {
                Some(self.agents[self.index(id)?].follower.position().to_array())
            }
        }
    }
    /// World position of a formation slot at the current anchor and heading.
    pub fn slot_position(&self, id: FormationId, slot: usize) -> Option<[f32; 3]> {
        let f = self.formations.get(&id)?;
        f.desc.slot_world(self.formation_anchor(f)?, f.yaw, slot)
    }
    /// (Re)assigns `members` to slots by greedy nearest matching (see `formation`), replacing
    /// the previous membership. Unknown agents and the leader are ignored.
    pub fn assign_formation(&mut self, id: FormationId, members: &[AgentId]) -> Result<()> {
        let f = self
            .formations
            .get(&id)
            .ok_or_else(|| anyhow::anyhow!("unknown formation"))?;
        let anchor = self
            .formation_anchor(f)
            .ok_or_else(|| anyhow::anyhow!("formation leader is missing"))?;
        let slots: Vec<Vec2> = (0..f.desc.slots.len())
            .map(|s| xz(Vec3::from(f.desc.slot_world(anchor, f.yaw, s).unwrap())))
            .collect();
        let mut list: Vec<(AgentId, Vec2)> = members
            .iter()
            .copied()
            .filter(|m| f.desc.anchor != FormationAnchor::Leader(*m))
            .filter_map(|m| Some((m, xz(self.agents[self.index(m)?].follower.position()))))
            .collect();
        list.sort_unstable_by_key(|(m, _)| *m);
        list.dedup_by_key(|(m, _)| *m);
        let assignment = formation::assign_slots(&list, &slots);
        self.formations.get_mut(&id).unwrap().members = assignment;
        Ok(())
    }

    /// Agents whose route is stale: it crosses a now-blocked polygon, uses a disabled or removed
    /// link, or previously failed and the navigation data has since changed.
    pub fn invalidated_agents(&self, nav: &NavContext) -> Vec<AgentId> {
        self.agents
            .iter()
            .filter(|a| Self::stale(a, nav))
            .map(|a| a.id)
            .collect()
    }
    fn stale(a: &CrowdAgent, nav: &NavContext) -> bool {
        if a.goal.is_none() {
            return false;
        }
        let revision = nav.overlay.map_or(0, NavOverlay::revision);
        if a.route_revision == revision {
            return false;
        }
        if a.failed {
            return true;
        }
        let Some(overlay) = nav.overlay else {
            return false;
        };
        let path = a.follower.path();
        let next = a.follower.next_index();
        let link_ok = |l: &RouteLink| overlay.link(l.link).is_some_and(|k| k.desc.enabled);
        if a.links.iter().any(|l| l.exit_index >= next && !link_ok(l)) {
            return true;
        }
        let mut from = a.follower.position().to_array();
        for k in next..path.len() {
            let to = path[k].to_array();
            let on_link = a.links.iter().any(|l| l.exit_index == k);
            if !on_link && overlay.segment_blocked(nav.mesh, from, to) {
                return true;
            }
            from = to;
        }
        false
    }
    /// Reroutes every stale agent now; returns the events.
    pub fn repath_invalidated(&mut self, nav: &NavContext) -> Result<Vec<CrowdEvent>> {
        let mut events = Vec::new();
        for a in &mut self.agents {
            if Self::stale(a, nav) {
                events.push(match Self::route(a, nav)? {
                    GoalOutcome::Reached => CrowdEvent::Repathed { agent: a.id },
                    _ => CrowdEvent::PathFailed { agent: a.id },
                });
            } else if a.goal.is_some() {
                a.route_revision = nav.overlay.map_or(0, NavOverlay::revision);
            }
        }
        Ok(events)
    }

    /// Pops reached waypoints, activates/finishes link traversal. Returns true if on a link.
    fn advance_waypoints(&mut self, i: usize, events: &mut Vec<CrowdEvent>) -> bool {
        let tolerance = self.settings.waypoint_tolerance;
        let a = &mut self.agents[i];
        loop {
            if a.link_active.is_none()
                && let Some(k) = a
                    .links
                    .iter()
                    .position(|l| l.exit_index == a.follower.next_index())
            {
                a.link_active = Some(k);
                events.push(CrowdEvent::LinkEntered {
                    agent: a.id,
                    link: a.links[k].link,
                });
            }
            if a.link_active.is_some() {
                return true;
            }
            let Some(t) = a.follower.target() else {
                return false;
            };
            let last = a.follower.next_index() + 1 == a.follower.path().len();
            let tol = if last { a.arrival_radius } else { tolerance };
            if (xz(t) - xz(a.follower.position())).length() > tol {
                return false;
            }
            if last {
                // Hold the final point so the agent steers back if pushed away.
                a.target = Some(t);
                a.follower
                    .set_path(Vec::new())
                    .expect("empty path is valid");
                a.links.clear();
                if !a.arrived {
                    a.arrived = true;
                    events.push(CrowdEvent::Arrived { agent: a.id });
                }
                return false;
            }
            a.follower.skip_waypoint();
        }
    }

    fn update_formations(&mut self) {
        let ids: Vec<FormationId> = self.formations.keys().copied().collect();
        for fid in ids {
            let f = &self.formations[&fid];
            let Some(anchor) = self.formation_anchor(f) else {
                continue;
            };
            let mut yaw = f.yaw;
            if let (FormationAnchor::Leader(l), true) = (f.desc.anchor, f.desc.follow_heading)
                && let Some(i) = self.index(l)
                && self.agents[i].velocity.length() > 0.1
            {
                let v = self.agents[i].velocity;
                yaw = v.x.atan2(v.y);
            }
            let targets: Vec<(AgentId, [f32; 3])> = f
                .members
                .iter()
                .filter_map(|&(m, s)| Some((m, f.desc.slot_world(anchor, yaw, s)?)))
                .collect();
            self.formations.get_mut(&fid).unwrap().yaw = yaw;
            for (m, p) in targets {
                let Some(i) = self.index(m) else { continue };
                let a = &mut self.agents[i];
                if !a.follower.path().is_empty() || a.goal.is_some() {
                    a.follower
                        .set_path(Vec::new())
                        .expect("empty path is valid");
                    a.goal = None;
                    a.links.clear();
                    a.link_active = None;
                }
                a.target = Some(Vec3::from(p));
            }
        }
    }

    /// Advances the crowd by `dt` seconds (0 < dt <= 1). With `nav`, stale routes are repathed
    /// first (when `auto_repath`). Events are returned in deterministic order.
    pub fn step(&mut self, dt: f32, nav: Option<&NavContext>) -> Result<Vec<CrowdEvent>> {
        ensure!(
            dt.is_finite() && dt > 0. && dt <= 1.,
            "invalid crowd time step"
        );
        let mut events = Vec::new();
        if let (Some(nav), true) = (nav, self.settings.auto_repath) {
            events.extend(self.repath_invalidated(nav)?);
        }
        self.update_formations();
        let n = self.agents.len();
        let s = self.settings;
        // Preferred velocities.
        let mut preferred = vec![Vec2::ZERO; n];
        let mut on_link = vec![false; n];
        for i in 0..n {
            on_link[i] = self.advance_waypoints(i, &mut events);
            let a = &mut self.agents[i];
            if on_link[i] {
                continue;
            }
            let Some(t) = a.follower.target().or(a.target) else {
                continue;
            };
            let delta = xz(t) - xz(a.follower.position());
            let d = delta.length();
            let final_point = a.follower.path().is_empty();
            if final_point {
                if d <= a.arrival_radius {
                    if !a.arrived {
                        a.arrived = true;
                        events.push(CrowdEvent::Arrived { agent: a.id });
                    }
                    continue;
                }
                if d > a.arrival_radius * 2. {
                    a.arrived = false;
                }
            }
            if d > 1e-6 {
                let speed = a.max_speed.min(d / dt);
                preferred[i] = delta / d * speed + nudge(a.id) * (s.symmetry_nudge * a.max_speed);
            }
        }
        // Neighbour search + ORCA, from a snapshot so the result is order independent.
        let positions: Vec<Vec2> = self
            .agents
            .iter()
            .map(|a| xz(a.follower.position()))
            .collect();
        let velocities: Vec<Vec2> = self.agents.iter().map(|a| a.velocity).collect();
        let hash = SpatialHash::build(s.neighbor_distance, positions.iter().copied());
        let inv_dt = 1. / dt;
        let mut new_velocity = vec![Vec2::ZERO; n];
        let mut pairs = 0;
        let mut lines: Vec<Line> = Vec::new();
        let mut near: Vec<(u32, u32)> = Vec::new();
        for i in 0..n {
            if on_link[i] {
                continue;
            }
            let a = &self.agents[i];
            let p = positions[i];
            near.clear();
            hash.for_each_near(p, s.neighbor_distance, |j| {
                let d = positions[j as usize].distance_squared(p);
                if j as usize != i && d < s.neighbor_distance * s.neighbor_distance {
                    near.push((d.to_bits(), j));
                }
            });
            near.sort_unstable();
            near.truncate(s.max_neighbors);
            pairs += near.len();
            lines.clear();
            let reach = s.obstacle_time_horizon * a.max_speed + a.radius;
            let mut wall_points: Vec<(u32, Vec2)> = self
                .segments
                .values()
                .filter_map(|seg| {
                    let q = closest_on_segment(p, *seg);
                    let d = q.distance_squared(p);
                    (d < reach * reach).then_some((d.to_bits(), q))
                })
                .collect();
            wall_points.sort_by_key(|(d, q)| (*d, q.x.to_bits(), q.y.to_bits()));
            wall_points.truncate(MAX_OBSTACLE_LINES);
            for (_, q) in &wall_points {
                lines.push(pair_line(
                    *q - p,
                    velocities[i],
                    velocities[i],
                    a.radius + s.safety_margin,
                    1. / s.obstacle_time_horizon,
                    inv_dt,
                    1.,
                ));
            }
            let num_obst = lines.len();
            for &(_, j) in &near {
                let j = j as usize;
                lines.push(pair_line(
                    positions[j] - p,
                    velocities[i] - velocities[j],
                    velocities[i],
                    a.radius + self.agents[j].radius + s.safety_margin,
                    1. / s.time_horizon,
                    inv_dt,
                    0.5,
                ));
            }
            new_velocity[i] = solve(&lines, num_obst, a.max_speed, preferred[i]);
        }
        // Integrate.
        for i in 0..n {
            if on_link[i] {
                self.move_on_link(i, dt, &mut events);
                continue;
            }
            let a = &mut self.agents[i];
            a.velocity = new_velocity[i];
            let mut p = a.follower.position();
            p.x += a.velocity.x * dt;
            p.z += a.velocity.y * dt;
            a.follower.set_position(p)?;
        }
        let corrections = self.resolve_contacts(&on_link);
        for i in 0..n {
            if !on_link[i] {
                self.settle_height(i);
                // A waypoint may be reached this step; report arrival/link entry promptly.
                self.advance_waypoints(i, &mut events);
            }
        }
        self.stats = CrowdStats {
            tick: self.stats.tick + 1,
            agents: n,
            neighbor_pairs: pairs,
            contact_corrections: corrections,
        };
        Ok(events)
    }

    fn move_on_link(&mut self, i: usize, dt: f32, events: &mut Vec<CrowdEvent>) {
        let a = &mut self.agents[i];
        let k = a.link_active.expect("link agent has an active link");
        let target = a.follower.path()[a.links[k].exit_index];
        let position = a.follower.position();
        let delta = target - position;
        let d = delta.length();
        let step = a.max_speed * dt;
        let (next, done) = if d <= step || d <= 1e-6 {
            (target, true)
        } else {
            (position + delta * (step / d), false)
        };
        a.velocity = Vec2::new(next.x - position.x, next.z - position.z) * (1. / dt);
        a.follower
            .set_position(next)
            .expect("link position is finite");
        if done {
            events.push(CrowdEvent::LinkExited {
                agent: a.id,
                link: a.links[k].link,
            });
            a.link_active = None;
            a.follower.skip_waypoint();
            // Re-evaluates the remaining path (possibly the final point) next advance.
        }
    }

    fn settle_height(&mut self, i: usize) {
        let a = &mut self.agents[i];
        let p = a.follower.position();
        let (from, to) = if let Some(t) = a.follower.target() {
            let next = a.follower.next_index();
            let prev = if next > 0 {
                a.follower.path()[next - 1]
            } else {
                a.origin
            };
            (prev, t)
        } else if let Some(t) = a.target {
            (a.origin, t)
        } else {
            return;
        };
        let seg = xz(to) - xz(from);
        let l = seg.length_squared();
        let y = if l > 1e-10 {
            let t = ((xz(p) - xz(from)).dot(seg) / l).clamp(0., 1.);
            from.y + (to.y - from.y) * t
        } else {
            to.y
        };
        let mut q = p;
        q.y = y;
        a.follower.set_position(q).expect("height is finite");
    }

    /// Hard separation backstop plus static-segment projection. Returns corrections applied.
    fn resolve_contacts(&mut self, on_link: &[bool]) -> usize {
        let n = self.agents.len();
        let mut corrections = 0;
        let segments: Vec<[Vec2; 2]> = self.segments.values().copied().collect();
        let max_radius = self.agents.iter().map(|a| a.radius).fold(0., f32::max);
        for _ in 0..self.settings.contact_iterations {
            let positions: Vec<Vec2> = self
                .agents
                .iter()
                .map(|a| xz(a.follower.position()))
                .collect();
            let hash =
                SpatialHash::build(self.settings.neighbor_distance, positions.iter().copied());
            let mut changed = false;
            let mut list: Vec<u32> = Vec::new();
            for i in 0..n {
                list.clear();
                let pi = xz(self.agents[i].follower.position());
                hash.for_each_near(pi, max_radius * 2. + 0.5, |j| {
                    if j as usize > i {
                        list.push(j);
                    }
                });
                list.sort_unstable();
                for &j in &list {
                    let j = j as usize;
                    let pi = xz(self.agents[i].follower.position());
                    let pj = xz(self.agents[j].follower.position());
                    let sum = self.agents[i].radius + self.agents[j].radius;
                    let delta = pj - pi;
                    let d = delta.length();
                    if d >= sum {
                        continue;
                    }
                    let dir = if d > 1e-6 {
                        delta / d
                    } else {
                        nudge(self.agents[i].id)
                    };
                    let push = sum - d + 1e-4;
                    let (wi, wj) = match (on_link[i], on_link[j]) {
                        (true, true) => continue,
                        (true, false) => (0., 1.),
                        (false, true) => (1., 0.),
                        _ => (0.5, 0.5),
                    };
                    self.shift(i, -dir * (push * wi));
                    self.shift(j, dir * (push * wj));
                    corrections += 1;
                    changed = true;
                }
            }
            for i in 0..n {
                if on_link[i] {
                    continue;
                }
                let r = self.agents[i].radius;
                for seg in &segments {
                    let p = xz(self.agents[i].follower.position());
                    let q = closest_on_segment(p, *seg);
                    let d = p.distance(q);
                    if d < r {
                        let dir = if d > 1e-6 {
                            (p - q) / d
                        } else {
                            let e = seg[1] - seg[0];
                            Vec2::new(-e.y, e.x).normalize_or_zero()
                        };
                        self.shift(i, dir * (r - d + 1e-4));
                        corrections += 1;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        corrections
    }
    fn shift(&mut self, i: usize, delta: Vec2) {
        let a = &mut self.agents[i];
        let mut p = a.follower.position();
        p.x += delta.x;
        p.z += delta.y;
        a.follower
            .set_position(p)
            .expect("contact position is finite");
    }
}
