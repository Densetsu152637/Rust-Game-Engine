use super::*;
use crate::world::coordinates::WorldOrigin;
/// Navigation meshes, `target` and `waypoints` are absolute world coordinates (equal to local
/// ones while `WorldOrigin.offset` is zero). A follower walks its path in the local frame of
/// the origin that was current when the path was planned (`frame`) and converts to the
/// current local frame when it writes the `Transform`, so origin rebasing never moves it.
struct Follower {
    config: NavigationAgent,
    agent: engine_navigation::Agent,
    notified: bool,
    frame: [f64; 3],
}
fn origin(world: &World) -> [f64; 3] {
    world
        .get_resource::<WorldOrigin>()
        .map_or([0.0; 3], |o| o.offset)
}
/// `point` (in the frame `from`) expressed in the frame `to`, rounded once.
fn reframe(point: [f32; 3], from: [f64; 3], to: [f64; 3]) -> [f32; 3] {
    std::array::from_fn(|i| (f64::from(point[i]) + from[i] - to[i]) as f32)
}
#[derive(Default)]
pub(super) struct Navigation {
    followers: BTreeMap<Entity, Follower>,
    meshes: BTreeMap<String, std::sync::Arc<NavMesh>>,
}
impl Navigation {
    pub fn sync(&mut self, world: &mut World) -> Result<()> {
        self.followers
            .retain(|e, _| world.get::<NavigationAgent>(*e).is_some());
        let configs: Vec<_> = world
            .iter_entities()
            .filter_map(|e| e.get::<NavigationAgent>().cloned().map(|c| (e.id(), c)))
            .collect();
        if configs.len() > 1024 {
            return Err(invalid("Navigation agent capacity exceeded"));
        }
        let active: BTreeSet<_> = configs
            .iter()
            .filter_map(|(_, c)| c.navmesh.as_deref())
            .collect();
        self.meshes.retain(|id, _| active.contains(id.as_str()));
        let mut query_work = 0usize;
        for (entity, config) in configs {
            if config.waypoints.len() > 65536
                || !config.speed.is_finite()
                || !(0.0..=1e6).contains(&config.speed)
                || config.navmesh.is_some() != config.target.is_some()
                || (config.navmesh.is_some() && !config.waypoints.is_empty())
            {
                return Err(invalid("Invalid navigation agent configuration"));
            }
            if self
                .followers
                .get(&entity)
                .is_some_and(|f| f.config == config)
            {
                continue;
            }
            let frame = origin(world);
            let position = world_pose(world, entity)?.2;
            let mut agent = engine_navigation::Agent::new(position).map_err(mapped)?;
            let mut notified = config.waypoints.is_empty();
            if let (Some(id), Some(target)) = (&config.navmesh, config.target) {
                config.query.validate().map_err(mapped)?;
                if !self.meshes.contains_key(id) {
                    if self.meshes.len() >= 16 {
                        return Err(invalid("Navigation mesh cache capacity exceeded"));
                    }
                    let project = &world.resource::<crate::runtime::ProjectResource>().0;
                    let mesh = load_navmesh(project, id)?;
                    if self
                        .meshes
                        .values()
                        .map(|m| m.polygon_count())
                        .sum::<usize>()
                        + mesh.polygon_count()
                        > 262_144
                    {
                        return Err(invalid("Navigation cached polygon capacity exceeded"));
                    }
                    self.meshes.insert(id.clone(), mesh);
                }
                query_work = query_work
                    .saturating_add(self.meshes[id].polygon_count() * 2)
                    .saturating_add(config.query.max_expansions);
                let path = if query_work > 1_000_000 {
                    NavPath::BudgetExceeded
                } else {
                    let start = reframe(position.to_array(), frame, [0.0; 3]);
                    self.meshes[id]
                        .find_path(start, target, config.query)
                        .map_err(mapped)?
                };
                match path {
                    NavPath::Reached { points, polygons } => {
                        let points: Vec<[f32; 3]> = points
                            .into_iter()
                            .map(|p| reframe(p, [0.0; 3], frame))
                            .collect();
                        // Nearest queries may project a distant authored pose onto a floor.
                        // Snap before following so the unvalidated connector is never walked.
                        if let Some(start) = points.first() {
                            crate::transforms::set_world_position(world, entity, *start)?;
                            agent = engine_navigation::Agent::new(Vec3::from(*start))
                                .map_err(mapped)?;
                        }
                        agent
                            .set_path(points.into_iter().map(Vec3::from).collect())
                            .map_err(mapped)?;
                        notified = false;
                        emit(
                            world,
                            entity,
                            None,
                            "navigation.repathed",
                            serde_json::json!({"target":target,"polygons":polygons}),
                        )?;
                    }
                    NavPath::Unreachable | NavPath::BudgetExceeded => {
                        let reason = if matches!(path, NavPath::Unreachable) {
                            "unreachable"
                        } else {
                            "budget_exceeded"
                        };
                        emit(
                            world,
                            entity,
                            None,
                            "navigation.failed",
                            serde_json::json!({"target":target,"reason":reason}),
                        )?;
                    }
                }
            } else {
                agent
                    .set_path(
                        config
                            .waypoints
                            .iter()
                            .map(|w| Vec3::from(reframe(*w, [0.0; 3], frame)))
                            .collect(),
                    )
                    .map_err(mapped)?;
            }
            self.followers.insert(
                entity,
                Follower {
                    config,
                    agent,
                    notified,
                    frame,
                },
            );
        }
        let active: BTreeSet<_> = self
            .followers
            .values()
            .filter_map(|f| f.config.navmesh.as_deref())
            .collect();
        self.meshes.retain(|id, _| active.contains(id.as_str()));
        Ok(())
    }
    pub fn step(&mut self, world: &mut World, dt: f32) -> Result<()> {
        let current = origin(world);
        for (e, follower) in &mut self.followers {
            follower
                .agent
                .advance(dt, follower.config.speed)
                .map_err(mapped)?;
            let walked = follower.agent.position().to_array();
            let position = reframe(walked, follower.frame, current);
            crate::transforms::set_world_position(world, *e, position)?;
            if follower.agent.arrived() && !follower.notified {
                let absolute: [f64; 3] =
                    std::array::from_fn(|i| f64::from(walked[i]) + follower.frame[i]);
                emit(
                    world,
                    *e,
                    None,
                    "navigation.arrived",
                    serde_json::json!({"position":position,"absolute":absolute}),
                )?;
                follower.notified = true;
            }
        }
        Ok(())
    }
}
