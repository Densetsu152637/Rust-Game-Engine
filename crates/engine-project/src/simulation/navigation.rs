use super::*;
struct Follower {
    config: NavigationAgent,
    agent: engine_navigation::Agent,
    notified: bool,
}
#[derive(Default)]
pub(super) struct Navigation {
    followers: BTreeMap<Entity, Follower>,
}
impl Navigation {
    pub fn sync(&mut self, world: &World) -> Result<()> {
        self.followers
            .retain(|e, _| world.get::<NavigationAgent>(*e).is_some());
        for entity in world.iter_entities() {
            if let Some(config) = entity.get::<NavigationAgent>() {
                if config.waypoints.len() > 65536
                    || !config.speed.is_finite()
                    || !(0.0..=1e6).contains(&config.speed)
                {
                    return Err(invalid("Navigation limits exceeded"));
                }
                if self
                    .followers
                    .get(&entity.id())
                    .is_none_or(|f| f.config != *config)
                {
                    let mut agent =
                        engine_navigation::Agent::new(world_pose(world, entity.id())?.2)
                            .map_err(mapped)?;
                    agent
                        .set_path(config.waypoints.iter().copied().map(Vec3::from).collect())
                        .map_err(mapped)?;
                    self.followers.insert(
                        entity.id(),
                        Follower {
                            config: config.clone(),
                            agent,
                            notified: config.waypoints.is_empty(),
                        },
                    );
                }
            }
        }
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
