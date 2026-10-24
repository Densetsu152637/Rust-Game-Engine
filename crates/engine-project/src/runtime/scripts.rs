use super::*;

/// Explicit ownership for native systems outside a script callback. ECS storage is Bevy's.
pub fn spawn_owned<B: Bundle>(world: &mut World, owner: Entity, bundle: B) -> Result<Entity> {
    if world.get_entity(owner).is_err() {
        return Err(Error::new(
            "script.owner.dead",
            "Cannot spawn for a dead owner",
        ));
    }
    Ok(world.spawn((OwnedBy(owner), bundle)).id())
}
pub(super) fn invoke_owned<T>(
    world: &mut World,
    owner: Entity,
    callback: impl FnOnce(&mut World) -> Result<T>,
) -> Result<T> {
    let before: BTreeSet<_> = world.iter_entities().map(|e| e.id()).collect();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(world)))
        .unwrap_or_else(|_| {
            Err(Error::new(
                "script.panic",
                "Native script callback panicked",
            ))
        });
    let created: Vec<_> = world
        .iter_entities()
        .filter(|e| {
            !before.contains(&e.id())
                && !e.contains::<OwnedBy>()
                && !e.contains::<bevy_ecs::resource::IsResource>()
        })
        .map(|e| e.id())
        .collect();
    for entity in created {
        world.entity_mut(entity).insert(OwnedBy(owner));
    }
    refresh_gameplay_input(world);
    result
}
pub(super) fn despawn_owned(world: &mut World, owner: Entity) {
    let children: Vec<_> = world
        .iter_entities()
        .filter(|e| e.get::<OwnedBy>().is_some_and(|o| o.0 == owner))
        .map(|e| e.id())
        .collect();
    for child in children {
        despawn_owned(world, child);
        world.despawn(child);
    }
}

/// Native code has full World access; callbacks run serially at lifecycle boundaries.
/// Use ordinary Bevy schedules for data-parallel custom component systems.
pub trait NativeScript: Send + Sync + 'static {
    fn on_event(
        &mut self,
        _world: &mut World,
        _owner: Entity,
        _event: &crate::EngineEvent,
    ) -> Result<()> {
        Ok(())
    }
    fn on_create(&mut self, _world: &mut World, _owner: Entity) -> Result<()> {
        Ok(())
    }
    fn on_update(&mut self, _world: &mut World, _owner: Entity, _dt: f32) -> Result<()> {
        Ok(())
    }
    fn on_destroy(&mut self, _world: &mut World, _owner: Entity) -> Result<()> {
        Ok(())
    }
}
pub type ScriptFactory = fn(&mut World, Entity) -> Result<Box<dyn NativeScript>>;
#[derive(Resource, Default)]
pub struct ScriptRegistry {
    pub factories: BTreeMap<String, ScriptFactory>,
}
/// Called by an external game's configure(&mut EngineApp), keyed by manifest asset ID.
pub fn register_script(
    app: &mut EngineApp,
    asset_id: impl Into<String>,
    factory: ScriptFactory,
) -> Result<()> {
    let id = asset_id.into();
    if id.is_empty() {
        return Err(Error::new(
            "script.asset.invalid",
            "Script asset ID must not be empty",
        ));
    }
    if !app.world().contains_resource::<ScriptRegistry>() {
        app.world_mut().insert_resource(ScriptRegistry::default());
    }
    app.world_mut()
        .resource_mut::<ScriptRegistry>()
        .factories
        .insert(id, factory);
    Ok(())
}
pub(super) struct ActiveScript {
    pub(super) owner: Entity,
    pub(super) asset: String,
    pub(super) script: Box<dyn NativeScript>,
}
pub(super) fn destroy_script(world: &mut World, mut script: ActiveScript) -> Result<()> {
    let result = invoke_owned(world, script.owner, |world| {
        script.script.on_destroy(world, script.owner)
    });
    despawn_owned(world, script.owner);
    result
}

impl Runtime {
    pub(super) fn validate_script_bindings(&self, scene: &Scene) -> Result<()> {
        let registry = self.app.world().resource::<ScriptRegistry>();
        for entity in &scene.entities {
            if let Some(script) = &entity.components.script
                && !registry.factories.contains_key(&script.asset)
            {
                return Err(Error::new(
                    "script.asset.unregistered",
                    format!(
                        "Script asset {} has no native factory; register it in gameplay::configure",
                        script.asset
                    ),
                )
                .at(&scene.source, &entity.id));
            }
        }
        Ok(())
    }
    pub(super) fn attach_scripts(&mut self) -> Result<()> {
        let mut pending: Vec<_> = self
            .app
            .world()
            .iter_entities()
            .filter_map(|e| e.get::<Script>().map(|s| (e.id(), s.asset.clone())))
            .filter(|(owner, asset)| {
                !self
                    .scripts
                    .iter()
                    .any(|s| s.owner == *owner && s.asset == *asset)
            })
            .collect();
        pending.sort_by_key(|(entity, _)| {
            self.app
                .world()
                .get::<AuthoredId>(*entity)
                .map(|id| id.id.clone())
                .unwrap_or_else(|| format!("entity:{}", entity.to_bits()))
        });
        for (owner, asset) in pending {
            let factory = self
                .app
                .world()
                .resource::<ScriptRegistry>()
                .factories
                .get(&asset)
                .copied()
                .ok_or_else(|| {
                    Error::new(
                        "script.asset.unregistered",
                        format!("Unregistered script asset {asset}"),
                    )
                })?;
            let mut script =
                match invoke_owned(self.app.world_mut(), owner, |world| factory(world, owner)) {
                    Ok(script) => script,
                    Err(error) => {
                        despawn_owned(self.app.world_mut(), owner);
                        return Err(error);
                    }
                };
            if let Err(error) = invoke_owned(self.app.world_mut(), owner, |world| {
                script.on_create(world, owner)
            }) {
                let _ = destroy_script(
                    self.app.world_mut(),
                    ActiveScript {
                        owner,
                        asset,
                        script,
                    },
                );
                return Err(error);
            }
            self.scripts.push(ActiveScript {
                owner,
                asset,
                script,
            });
        }
        Ok(())
    }
}
