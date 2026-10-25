//! Crowd steering service: `engine.CrowdAgent`, `engine.NavigationObstacle`,
//! `engine.OffMeshLink` and `engine.Formation` over `engine_navigation::Crowd`.
//!
//! One group (mesh + carving overlay + crowd) exists per navmesh asset id, so agents on the same
//! mesh avoid each other. Everything is keyed by `BTreeMap`, so output order is deterministic.
use super::*;
use engine_navigation::{
    AgentDesc, AgentId, Crowd, CrowdEvent, CrowdSettings, DynamicShape, FormationAnchor,
    FormationDesc, FormationId, GoalOutcome, LinkId, NavContext, NavOverlay, ObstacleId,
    OffMeshLinkDesc,
};
use std::sync::Arc;

const MAX_AGENTS: usize = 1024;
const MAX_OBSTACLES: usize = 256;
const MAX_LINKS: usize = 256;
const MAX_FORMATIONS: usize = 64;
const MAX_GROUPS: usize = 16;
const MAX_POLYGONS: usize = 262_144;

struct Group {
    mesh: Arc<NavMesh>,
    overlay: NavOverlay,
    crowd: Crowd,
}
struct AgentState {
    id: AgentId,
    mesh: String,
    config: CrowdAgent,
}
struct ObstacleState {
    mesh: String,
    id: ObstacleId,
    config: NavigationObstacle,
    position: Vec3,
}
struct LinkState {
    mesh: String,
    id: LinkId,
    config: OffMeshLink,
    position: Vec3,
}
struct FormationState {
    mesh: String,
    id: FormationId,
    applied: Option<(FormationDesc, Vec<AgentId>)>,
}
#[derive(Default)]
pub(super) struct Crowds {
    groups: BTreeMap<String, Group>,
    agents: BTreeMap<Entity, AgentState>,
    obstacles: BTreeMap<Entity, ObstacleState>,
    links: BTreeMap<Entity, LinkState>,
    formations: BTreeMap<Entity, FormationState>,
    next_id: u32,
}

fn authored(world: &World, entity: Entity) -> Option<String> {
    world
        .get::<crate::runtime::AuthoredId>(entity)
        .map(|a| a.id.clone())
}
fn finite_in(v: f32, lo: f32, hi: f32) -> bool {
    v.is_finite() && (lo..=hi).contains(&v)
}
fn check_agent(c: &CrowdAgent) -> Result<()> {
    if !finite_in(c.radius, 0.01, 100.)
        || !finite_in(c.max_speed, 0., 1000.)
        || !finite_in(c.arrival_radius, 0.001, 100.)
        || c.target.is_some_and(|t| t.iter().any(|v| !v.is_finite()))
    {
        return Err(invalid("Invalid crowd agent configuration"));
    }
    Ok(())
}

impl Crowds {
    fn group(&mut self, world: &World, id: &str) -> Result<&mut Group> {
        if !self.groups.contains_key(id) {
            if self.groups.len() >= MAX_GROUPS {
                return Err(invalid("Crowd navigation mesh cache capacity exceeded"));
            }
            let project = &world.resource::<crate::runtime::ProjectResource>().0;
            let mesh = load_navmesh(project, id)?;
            if self
                .groups
                .values()
                .map(|g| g.mesh.polygon_count())
                .sum::<usize>()
                + mesh.polygon_count()
                > MAX_POLYGONS
            {
                return Err(invalid("Crowd cached polygon capacity exceeded"));
            }
            let overlay = NavOverlay::new(&mesh);
            let crowd = Crowd::new(CrowdSettings {
                max_agents: MAX_AGENTS,
                ..Default::default()
            })
            .map_err(mapped)?;
            self.groups.insert(
                id.to_string(),
                Group {
                    mesh,
                    overlay,
                    crowd,
                },
            );
        }
        Ok(self.groups.get_mut(id).expect("group inserted"))
    }
    fn fresh_id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }

    pub fn sync(&mut self, world: &mut World) -> Result<()> {
        // Gather authored state in entity order.
        let mut agents = BTreeMap::new();
        let mut obstacles = BTreeMap::new();
        let mut links = BTreeMap::new();
        let mut formations = BTreeMap::new();
        for e in world.iter_entities() {
            if let Some(c) = e.get::<CrowdAgent>() {
                agents.insert(e.id(), c.clone());
            }
            if let Some(c) = e.get::<NavigationObstacle>() {
                obstacles.insert(e.id(), c.clone());
            }
            if let Some(c) = e.get::<OffMeshLink>() {
                links.insert(e.id(), c.clone());
            }
            if let Some(c) = e.get::<Formation>() {
                formations.insert(e.id(), c.clone());
            }
        }
        if agents.len() > MAX_AGENTS
            || obstacles.len() > MAX_OBSTACLES
            || links.len() > MAX_LINKS
            || formations.len() > MAX_FORMATIONS
        {
            return Err(invalid("Crowd capacity exceeded"));
        }
        for c in agents.values() {
            check_agent(c)?;
        }
        for c in obstacles.values() {
            let ok = c.half_extents.iter().all(|v| finite_in(*v, 0., 10_000.))
                && finite_in(c.radius, 0., 10_000.)
                && finite_in(c.half_height, 0., 10_000.);
            if !ok {
                return Err(invalid("Invalid navigation obstacle configuration"));
            }
        }
        for c in links.values() {
            if c.end.iter().any(|v| !v.is_finite())
                || !finite_in(c.cost, 0., 1e6)
                || !finite_in(c.snap_distance, 0., 100.)
            {
                return Err(invalid("Invalid off-mesh link configuration"));
            }
        }
        for c in formations.values() {
            if c.slots.len() > 256
                || !c.yaw.is_finite()
                || c.slots.iter().flatten().any(|v| !v.is_finite())
            {
                return Err(invalid("Invalid formation configuration"));
            }
        }

        // Removals (component gone, disabled, or moved to another mesh).
        let gone: Vec<Entity> = self
            .agents
            .iter()
            .filter(|(e, s)| {
                agents
                    .get(e)
                    .is_none_or(|c| c.navmesh.as_deref() != Some(s.mesh.as_str()))
            })
            .map(|(e, _)| *e)
            .collect();
        for e in gone {
            let s = self.agents.remove(&e).expect("listed agent");
            if let Some(g) = self.groups.get_mut(&s.mesh) {
                g.crowd.remove_agent(s.id);
            }
        }
        let gone: Vec<Entity> = self
            .obstacles
            .iter()
            .filter(|(e, s)| {
                obstacles
                    .get(e)
                    .is_none_or(|c| !c.enabled || c.navmesh.as_deref() != Some(s.mesh.as_str()))
            })
            .map(|(e, _)| *e)
            .collect();
        for e in gone {
            let s = self.obstacles.remove(&e).expect("listed obstacle");
            if let Some(g) = self.groups.get_mut(&s.mesh) {
                g.overlay.remove_obstacle(s.id);
            }
        }
        let gone: Vec<Entity> = self
            .links
            .iter()
            .filter(|(e, s)| {
                links
                    .get(e)
                    .is_none_or(|c| c.navmesh.as_deref() != Some(s.mesh.as_str()))
            })
            .map(|(e, _)| *e)
            .collect();
        for e in gone {
            let s = self.links.remove(&e).expect("listed link");
            if let Some(g) = self.groups.get_mut(&s.mesh) {
                g.overlay.remove_link(s.id);
            }
        }
        let gone: Vec<Entity> = self
            .formations
            .iter()
            .filter(|(e, s)| {
                formations
                    .get(e)
                    .is_none_or(|c| c.navmesh.as_deref() != Some(s.mesh.as_str()))
            })
            .map(|(e, _)| *e)
            .collect();
        for e in gone {
            let s = self.formations.remove(&e).expect("listed formation");
            if let Some(g) = self.groups.get_mut(&s.mesh) {
                g.crowd.remove_formation(s.id);
            }
        }

        // Obstacles and links first so agent routes see them.
        for (e, c) in &obstacles {
            let (Some(mesh), true) = (&c.navmesh, c.enabled) else {
                continue;
            };
            let position = world_pose(world, *e)?.2;
            if self
                .obstacles
                .get(e)
                .is_some_and(|s| s.config == *c && s.position == position)
            {
                continue;
            }
            let shape = match c.shape {
                ObstacleShape::Box => DynamicShape::Box {
                    min: (position - Vec3::from(c.half_extents)).to_array(),
                    max: (position + Vec3::from(c.half_extents)).to_array(),
                },
                ObstacleShape::Cylinder => DynamicShape::Cylinder {
                    base: (position - Vec3::Y * c.half_height).to_array(),
                    radius: c.radius,
                    height: c.half_height * 2.,
                },
            };
            let id = match self.obstacles.get(e) {
                Some(s) => s.id,
                None => self.fresh_id(),
            };
            let group = self.group(world, mesh)?;
            group
                .overlay
                .set_obstacle(&group.mesh, id, shape)
                .map_err(mapped)?;
            self.obstacles.insert(
                *e,
                ObstacleState {
                    mesh: mesh.clone(),
                    id,
                    config: c.clone(),
                    position,
                },
            );
        }
        for (e, c) in &links {
            let Some(mesh) = &c.navmesh else { continue };
            let position = world_pose(world, *e)?.2;
            if self
                .links
                .get(e)
                .is_some_and(|s| s.config == *c && s.position == position)
            {
                continue;
            }
            let id = match self.links.get(e) {
                Some(s) => s.id,
                None => self.fresh_id(),
            };
            let group = self.group(world, mesh)?;
            let desc = OffMeshLinkDesc {
                start: position.to_array(),
                end: c.end,
                cost: c.cost,
                bidirectional: c.bidirectional,
                enabled: c.enabled,
                snap_distance: c.snap_distance,
            };
            group
                .overlay
                .set_link(&group.mesh, id, desc)
                .map_err(mapped)?;
            self.links.insert(
                *e,
                LinkState {
                    mesh: mesh.clone(),
                    id,
                    config: c.clone(),
                    position,
                },
            );
        }

        // Agents.
        for (e, c) in &agents {
            let Some(mesh) = &c.navmesh else { continue };
            let position = world_pose(world, *e)?.2;
            if !self.agents.contains_key(e) {
                let group = self.group(world, mesh)?;
                let id = group
                    .crowd
                    .add_agent(AgentDesc {
                        position,
                        radius: c.radius,
                        max_speed: c.max_speed,
                        arrival_radius: c.arrival_radius,
                    })
                    .map_err(mapped)?;
                self.agents.insert(
                    *e,
                    AgentState {
                        id,
                        mesh: mesh.clone(),
                        config: CrowdAgent {
                            target: None,
                            formation: None,
                            ..c.clone()
                        },
                    },
                );
            }
            let state = self.agents.get_mut(e).expect("agent exists");
            let id = state.id;
            let group = self.groups.get_mut(mesh).expect("group exists");
            // External pose writes (teleports) win.
            let current = group.crowd.agent(id).expect("crowd agent").position;
            if current.distance(position) > 1e-3 {
                group.crowd.teleport(id, position).map_err(mapped)?;
            }
            if state.config.radius != c.radius || state.config.max_speed != c.max_speed {
                group
                    .crowd
                    .set_agent_params(id, c.radius, c.max_speed)
                    .map_err(mapped)?;
                state.config.radius = c.radius;
                state.config.max_speed = c.max_speed;
            }
            let goal_changed = state.config.target != c.target || state.config.repath != c.repath;
            let formation_changed = state.config.formation != c.formation;
            state.config.formation = c.formation.clone();
            state.config.arrival_radius = c.arrival_radius;
            if formation_changed && c.formation.is_none() {
                group.crowd.clear_goal(id).map_err(mapped)?;
            }
            if goal_changed && c.formation.is_none() {
                state.config.target = c.target;
                state.config.repath = c.repath;
                match c.target {
                    Some(t) => {
                        let outcome = {
                            let Group {
                                mesh: m,
                                overlay: o,
                                crowd,
                            } = group;
                            let ctx = NavContext {
                                mesh: m,
                                overlay: Some(o),
                                query: QuerySettings::default(),
                            };
                            crowd.set_goal(id, Vec3::from(t), &ctx).map_err(mapped)?
                        };
                        let (name, payload) = match outcome {
                            GoalOutcome::Reached => {
                                ("navigation.repathed", serde_json::json!({"target":t}))
                            }
                            GoalOutcome::Unreachable => (
                                "navigation.failed",
                                serde_json::json!({"target":t,"reason":"unreachable"}),
                            ),
                            GoalOutcome::BudgetExceeded => (
                                "navigation.failed",
                                serde_json::json!({"target":t,"reason":"budget_exceeded"}),
                            ),
                        };
                        emit(world, *e, None, name, payload)?;
                    }
                    None => group.crowd.clear_goal(id).map_err(mapped)?,
                }
            } else if c.formation.is_some() {
                state.config.target = c.target;
                state.config.repath = c.repath;
            }
        }

        // Formations: membership follows `CrowdAgent.formation` naming the formation entity.
        for (e, c) in &formations {
            let Some(mesh) = &c.navmesh else { continue };
            let Some(own_id) = authored(world, *e) else {
                continue;
            };
            let position = world_pose(world, *e)?.2;
            let members: Vec<(Entity, AgentId)> = agents
                .iter()
                .filter(|(_, a)| {
                    a.formation.as_deref() == Some(own_id.as_str())
                        && a.navmesh.as_deref() == Some(mesh.as_str())
                })
                .filter_map(|(m, _)| Some((*m, self.agents.get(m)?.id)))
                .collect();
            let leader = c
                .leader
                .as_deref()
                .and_then(|l| crate::services::find_entity(world, l))
                .and_then(|l| self.agents.get(&l))
                .filter(|s| s.mesh == *mesh)
                .map(|s| s.id);
            let desc = FormationDesc {
                slots: c.slots.clone(),
                anchor: match leader {
                    Some(id) => FormationAnchor::Leader(id),
                    None => FormationAnchor::Fixed(position.to_array()),
                },
                yaw: c.yaw,
                follow_heading: c.follow_heading,
            };
            let ids: Vec<AgentId> = members.iter().map(|(_, id)| *id).collect();
            if !self.formations.contains_key(e) {
                let id = self.fresh_id();
                self.formations.insert(
                    *e,
                    FormationState {
                        mesh: mesh.clone(),
                        id,
                        applied: None,
                    },
                );
            }
            let fid = self.formations[e].id;
            let (anchor_changed, members_changed) = {
                let previous = self.formations[e].applied.as_ref();
                (
                    previous.map(|(d, _)| d) != Some(&desc),
                    previous.map(|(_, m)| m) != Some(&ids),
                )
            };
            let group = self.group(world, mesh)?;
            if anchor_changed {
                group
                    .crowd
                    .set_formation(fid, desc.clone())
                    .map_err(mapped)?;
            }
            if members_changed {
                group.crowd.assign_formation(fid, &ids).map_err(mapped)?;
            }
            if anchor_changed || members_changed {
                self.formations
                    .get_mut(e)
                    .expect("formation exists")
                    .applied = Some((desc, ids));
            }
        }

        // Drop groups that nothing references any more.
        let used: BTreeSet<&str> = self
            .agents
            .values()
            .map(|s| s.mesh.as_str())
            .chain(self.obstacles.values().map(|s| s.mesh.as_str()))
            .chain(self.links.values().map(|s| s.mesh.as_str()))
            .chain(self.formations.values().map(|s| s.mesh.as_str()))
            .collect();
        self.groups.retain(|id, _| used.contains(id.as_str()));
        Ok(())
    }

    pub fn step(&mut self, world: &mut World, dt: f32) -> Result<()> {
        let dt = dt.clamp(1e-4, 1.);
        let ids: Vec<String> = self.groups.keys().cloned().collect();
        for mesh in ids {
            let reverse: BTreeMap<AgentId, Entity> = self
                .agents
                .iter()
                .filter(|(_, s)| s.mesh == mesh)
                .map(|(e, s)| (s.id, *e))
                .collect();
            let links: BTreeMap<LinkId, Entity> = self
                .links
                .iter()
                .filter(|(_, s)| s.mesh == mesh)
                .map(|(e, s)| (s.id, *e))
                .collect();
            let group = self.groups.get_mut(&mesh).expect("group exists");
            let Group {
                mesh: m,
                overlay,
                crowd,
            } = group;
            let ctx = NavContext {
                mesh: m,
                overlay: Some(overlay),
                query: QuerySettings::default(),
            };
            let events = crowd.step(dt, Some(&ctx)).map_err(mapped)?;
            for event in events {
                let (agent, name, payload) = match event {
                    CrowdEvent::Arrived { agent } => {
                        let p = crowd.agent(agent).map(|a| a.position.to_array());
                        (
                            agent,
                            "navigation.arrived",
                            serde_json::json!({"position":p}),
                        )
                    }
                    CrowdEvent::LinkEntered { agent, link } => (
                        agent,
                        "navigation.link_entered",
                        serde_json::json!({"link":links.get(&link).and_then(|l| authored(world, *l))}),
                    ),
                    CrowdEvent::LinkExited { agent, link } => (
                        agent,
                        "navigation.link_exited",
                        serde_json::json!({"link":links.get(&link).and_then(|l| authored(world, *l))}),
                    ),
                    CrowdEvent::Repathed { agent } => (
                        agent,
                        "navigation.repathed",
                        serde_json::json!({"reason":"invalidated"}),
                    ),
                    CrowdEvent::PathFailed { agent } => (
                        agent,
                        "navigation.failed",
                        serde_json::json!({"reason":"blocked"}),
                    ),
                };
                if let Some(entity) = reverse.get(&agent) {
                    emit(world, *entity, None, name, payload)?;
                }
            }
            for (agent, position) in crowd.positions() {
                if let Some(entity) = reverse.get(&agent) {
                    crate::transforms::set_world_position(world, *entity, position.to_array())?;
                }
            }
        }
        Ok(())
    }
}
