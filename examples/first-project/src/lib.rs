//! Native authored-owner movement and project-defined component systems.
use engine_core::bevy_ecs;
use engine_core::{anyhow, prelude::*};
use engine_project::{ActionFrame, NativeScript, Transform, register_script};

#[derive(Component, Clone, Debug, PartialEq)]
pub struct PlayerStats {
    pub ticks: u64,
    pub moves: u64,
    pub enabled: bool,
}
#[derive(Resource, Default)]
pub struct LifecycleLog(pub Vec<String>);
#[derive(SystemSet, Clone, Debug, PartialEq, Eq, Hash)]
pub struct PlayerStatsTick;

pub fn configure(app: &mut EngineApp) -> anyhow::Result<()> {
    app.world_mut().init_resource::<LifecycleLog>();
    app.world_mut().init_resource::<FixtureState>();
    app.schedule_mut(Stage::Update)
        .add_systems(tick_stats.in_set(PlayerStatsTick));
    app.schedule_mut(Stage::Update)
        .add_systems((mutate_fixture, late_fixture).chain_ignore_deferred());
    register_script(app, "asset:player-script", |_, _| Ok(Box::new(Player)))?;
    register_script(app, "asset:player-reload", |_, _| {
        Ok(Box::new(ReloadedPlayer))
    })?;
    register_script(app, "asset:create-then-fail", |_, _| {
        Ok(Box::new(CreateThenFail::default()))
    })?;
    register_script(app, "asset:teardown-fails", |_, _| {
        Ok(Box::new(TeardownFails))
    })?;
    register_script(app, "asset:system-fixture", |_, _| {
        Ok(Box::new(SystemFixture::default()))
    })?;
    register_script(app, "asset:system-abort", |_, _| Ok(Box::new(AbortFixture)))?;
    Ok(())
}
fn tick_stats(mut players: Query<&mut PlayerStats>, input: Res<ActionFrame>) {
    for mut stats in &mut players {
        if stats.enabled {
            stats.ticks += 1;
            stats.moves += u64::from(input.pressed("move_right"));
        }
    }
}
pub struct Player;
impl NativeScript for Player {
    fn on_create(&mut self, world: &mut World, owner: Entity) -> engine_project::Result<()> {
        world.entity_mut(owner).insert(PlayerStats {
            ticks: 0,
            moves: 0,
            enabled: true,
        });
        Ok(())
    }
    fn on_update(
        &mut self,
        world: &mut World,
        owner: Entity,
        _: f32,
    ) -> engine_project::Result<()> {
        if world.resource::<ActionFrame>().pressed("move_right") {
            world
                .get_mut::<Transform>(owner)
                .ok_or_else(|| {
                    engine_project::Error::new("example.transform", "Player requires Transform")
                })?
                .position[0] += 1.0;
        }
        Ok(())
    }
}
pub struct ReloadedPlayer;
impl NativeScript for ReloadedPlayer {
    fn on_update(
        &mut self,
        world: &mut World,
        owner: Entity,
        _: f32,
    ) -> engine_project::Result<()> {
        if world.resource::<ActionFrame>().pressed("move_right") {
            world
                .get_mut::<Transform>(owner)
                .ok_or_else(|| {
                    engine_project::Error::new("example.transform", "Player requires Transform")
                })?
                .position[0] += 10.0;
        }
        Ok(())
    }
}
#[derive(Default)]
pub struct CreateThenFail {
    spawned: Option<Entity>,
}
impl NativeScript for CreateThenFail {
    fn on_create(&mut self, world: &mut World, _: Entity) -> engine_project::Result<()> {
        self.spawned = Some(
            world
                .spawn(Transform {
                    position: [8.0, 0.0, 0.0],
                    ..Default::default()
                })
                .id(),
        );
        world
            .resource_mut::<LifecycleLog>()
            .0
            .push("partial-on-create-effect".into());
        Err(engine_project::Error::new(
            "example.setup",
            "intentional setup failure after side effects",
        ))
    }
    fn on_destroy(&mut self, world: &mut World, _: Entity) -> engine_project::Result<()> {
        if let Some(entity) = self.spawned.take() {
            world.despawn(entity);
            world
                .resource_mut::<LifecycleLog>()
                .0
                .push("spawned-entity-cleaned-by-script".into());
        }
        Ok(())
    }
}
pub struct TeardownFails;
impl NativeScript for TeardownFails {
    fn on_create(&mut self, world: &mut World, _: Entity) -> engine_project::Result<()> {
        world
            .resource_mut::<LifecycleLog>()
            .0
            .push("script-started 7".into());
        Ok(())
    }
    fn on_update(
        &mut self,
        world: &mut World,
        owner: Entity,
        dt: f32,
    ) -> engine_project::Result<()> {
        Player.on_update(world, owner, dt)
    }
    fn on_destroy(&mut self, world: &mut World, _: Entity) -> engine_project::Result<()> {
        world
            .resource_mut::<LifecycleLog>()
            .0
            .push("old-destroy".into());
        Err(engine_project::Error::new(
            "example.teardown",
            "authored script teardown failed",
        ))
    }
}
#[derive(Component)]
pub struct TestState(pub u64);
#[derive(Component)]
pub struct LateState(pub u64);
#[derive(Resource, Default)]
pub struct FixtureState {
    pub owner: Option<Entity>,
    pub spawned: Option<Entity>,
    pub created_late: bool,
}
fn mutate_fixture(
    mut query: Query<(Entity, &mut TestState)>,
    mut commands: Commands,
    mut fixture: ResMut<FixtureState>,
    mut log: ResMut<LifecycleLog>,
) {
    let Some(owner) = fixture.owner else {
        return;
    };
    let mut entities: Vec<_> = query
        .iter()
        .map(|(entity, state)| (entity, state.0))
        .collect();
    entities.sort_by_key(|(entity, _)| entity.to_bits());
    for (entity, ticks) in entities {
        log.0.push(format!("state:{entity:?}:{ticks}"));
        if entity == owner {
            query.get_mut(entity).unwrap().1.0 += 1;
            if let Some(spawned) = fixture.spawned {
                commands.entity(spawned).remove::<TestState>();
            }
            if !fixture.created_late {
                commands.spawn((LateState(1), engine_project::runtime::OwnedBy(owner)));
                fixture.created_late = true;
            }
        }
    }
}
fn late_fixture(query: Query<(Entity, &LateState)>, mut log: ResMut<LifecycleLog>) {
    for (entity, _) in &query {
        log.0.push(format!("late:{entity:?}"));
    }
}
#[derive(Default)]
pub struct SystemFixture {
    spawned: Option<Entity>,
}
impl NativeScript for SystemFixture {
    fn on_create(&mut self, world: &mut World, owner: Entity) -> engine_project::Result<()> {
        world.entity_mut(owner).insert(TestState(0));
        self.spawned = Some(engine_project::runtime::spawn_owned(
            world,
            owner,
            TestState(0),
        )?);
        world.insert_resource(FixtureState {
            owner: Some(owner),
            spawned: self.spawned,
            created_late: false,
        });
        Ok(())
    }
    fn on_update(
        &mut self,
        world: &mut World,
        owner: Entity,
        _: f32,
    ) -> engine_project::Result<()> {
        let _ = (world, owner);
        Ok(())
    }
    fn on_destroy(&mut self, world: &mut World, _: Entity) -> engine_project::Result<()> {
        if let Some(entity) = self.spawned.take() {
            world.despawn(entity);
        }
        *world.resource_mut::<FixtureState>() = FixtureState::default();
        Ok(())
    }
}
#[derive(Component)]
pub struct AbortState(pub u64);
pub struct AbortFixture;
impl NativeScript for AbortFixture {
    fn on_create(&mut self, world: &mut World, owner: Entity) -> engine_project::Result<()> {
        world.entity_mut(owner).insert(AbortState(0));
        Ok(())
    }
    fn on_update(&mut self, _: &mut World, _: Entity, _: f32) -> engine_project::Result<()> {
        // Validate a candidate mutation before publication. Native World changes
        // are not automatically rollbackable; this fixture publishes nothing.
        let _candidate = (AbortState(9), AbortState(1));
        Err(engine_project::Error::new(
            "example.system",
            "intentional system failure",
        ))
    }
    fn on_destroy(&mut self, world: &mut World, owner: Entity) -> engine_project::Result<()> {
        let value = world.get::<AbortState>(owner).map_or(0, |s| s.0);
        world
            .resource_mut::<LifecycleLog>()
            .0
            .push(format!("abort-state:{value}"));
        Ok(())
    }
}
