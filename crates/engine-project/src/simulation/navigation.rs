use super::*;
struct Follower {
    config: NavigationAgent,
    agent: engine_navigation::Agent,
    notified: bool,
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
                    self.meshes[id]
                        .find_path(position.to_array(), target, config.query)
                        .map_err(mapped)?
                };
                match path {
                    NavPath::Reached { points, polygons } => {
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
                    .set_path(config.waypoints.iter().copied().map(Vec3::from).collect())
                    .map_err(mapped)?;
            }
            self.followers.insert(
                entity,
                Follower {
                    config,
                    agent,
                    notified,
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
        for (e, follower) in &mut self.followers {
            follower
                .agent
                .advance(dt, follower.config.speed)
                .map_err(mapped)?;
            crate::transforms::set_world_position(world, *e, follower.agent.position().to_array())?;
            if follower.agent.arrived() && !follower.notified {
                emit(
                    world,
                    *e,
                    None,
                    "navigation.arrived",
                    serde_json::json!({"position":follower.agent.position().to_array()}),
                )?;
                follower.notified = true;
            }
        }
        Ok(())
    }
}
