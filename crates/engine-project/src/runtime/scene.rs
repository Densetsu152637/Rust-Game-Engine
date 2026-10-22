use super::*;

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeSnapshot {
    pub ticks: u64,
    pub scene: Scene,
    pub live_entities: usize,
    pub active_scripts: usize,
    pub scene_revision: u64,
    pub triggers: Vec<TriggerEvent>,
    pub logs: Vec<String>,
}

impl Runtime {
    pub fn request_scene(
        &mut self,
        asset: impl Into<String>,
        spawn: impl Into<String>,
        traveller: impl Into<String>,
    ) -> Result<()> {
        let request = SceneRequest {
            asset: asset.into(),
            spawn: spawn.into(),
            traveller: traveller.into(),
        };
        self.prepare_transition(&request)?;
        if self.app.world().resource::<PendingScene>().0.is_some() {
            return Err(Error::new(
                "runtime.scene.pending",
                "A scene transition is already pending",
            ));
        }
        self.app.world_mut().resource_mut::<PendingScene>().0 = Some(request);
        Ok(())
    }
    pub(super) fn prepare_transition(
        &self,
        request: &SceneRequest,
    ) -> Result<(Scene, gameplay2d::World)> {
        crate::services::prepare_scene(self.app.world(), &self.project, request)
    }
    pub(super) fn transition(&mut self, request: SceneRequest) -> Result<()> {
        // All authored data is prepared before mutating the live generation. Native callback
        // side effects cannot be rolled back; a callback failure stops the generation.
        let (scene, collision) = self.prepare_transition(&request)?;
        let retained = self.entity(&request.traveller);
        let mut keep_scripts = Vec::new();
        let mut retirement_error = None;
        for script in std::mem::take(&mut self.scripts) {
            if Some(script.owner) == retained {
                keep_scripts.push(script);
            } else {
                if let Err(error) = destroy_script(self.app.world_mut(), script)
                    && retirement_error.is_none()
                {
                    retirement_error = Some(error);
                }
            }
        }
        self.scripts = keep_scripts;
        if let Some(error) = retirement_error {
            return Err(error);
        }
        let previous: Vec<_> = self
            .app
            .world()
            .resource::<EntityLookup>()
            .0
            .values()
            .copied()
            .collect();
        for entity in previous {
            if Some(entity) != retained {
                self.app.world_mut().despawn(entity);
            }
        }
        self.app
            .world_mut()
            .resource_mut::<EntityLookup>()
            .0
            .clear();
        for authored in &scene.entities {
            if authored.id == request.traveller
                && let Some(entity) = retained
            {
                apply_components(self.app.world_mut(), entity, authored);
                self.app
                    .world_mut()
                    .resource_mut::<EntityLookup>()
                    .0
                    .insert(authored.id.clone(), entity);
                continue;
            }
            spawn(self.app.world_mut(), authored);
        }
        self.scene = scene;
        self.scene_revision += 1;
        self.geometry.borrow_mut().clear();
        self.app.world_mut().insert_resource(collision);
        self.app
            .world_mut()
            .resource_mut::<TriggerEvents>()
            .0
            .clear();
        self.app.world_mut().resource_mut::<UiModel>().clear();
        crate::audio::synchronize(self.app.world_mut())?;
        self.attach_scripts()?;
        Ok(())
    }
    pub(super) fn snapshot_entity(&self, entity: Entity) -> Result<SceneEntity> {
        let e = self
            .app
            .world()
            .get_entity(entity)
            .map_err(|_| Error::new("runtime.entity.dead", "Entity no longer exists"))?;
        let id = e.get::<AuthoredId>().ok_or_else(|| {
            Error::new("runtime.entity.unauthored", "Entity has no persistent ID")
        })?;
        Ok(SceneEntity {
            id: id.id.clone(),
            name: id.name.clone(),
            components: Components {
                transform: e.get::<Transform>().cloned(),
                script: e.get::<Script>().cloned(),
                mesh_renderer: e.get::<MeshRenderer>().cloned(),
                sprite_renderer: e.get::<SpriteRenderer>().cloned(),
                camera_2d: e.get::<Camera2D>().cloned(),
                collider_2d: e.get::<Collider2D>().cloned(),
            },
        })
    }
    pub fn snapshot(&self) -> RuntimeSnapshot {
        let mut scene = self.scene.clone();
        scene.entities = self
            .app
            .world()
            .resource::<EntityLookup>()
            .0
            .values()
            .filter_map(|e| self.snapshot_entity(*e).ok())
            .collect();
        RuntimeSnapshot {
            ticks: self.ticks,
            scene,
            live_entities: self
                .app
                .world()
                .iter_entities()
                .filter(|e| !e.contains::<bevy_ecs::resource::IsResource>())
                .count(),
            active_scripts: self.scripts.len(),
            scene_revision: self.scene_revision,
            triggers: self.app.world().resource::<TriggerEvents>().0.clone(),
            logs: self.app.world().resource::<RuntimeLog>().0.clone(),
        }
    }
}
pub(super) fn body_from(t: &Transform, b: &Collider2D) -> Body {
    Body {
        position: [t.position[0], t.position[1]],
        size: b.size,
        offset: b.offset,
        trigger: b.trigger,
        layer: b.layer,
        mask: b.mask,
    }
}
pub(super) fn collision_from_scene(scene: &Scene) -> Result<gameplay2d::World> {
    let mut world = gameplay2d::World::default();
    for entity in &scene.entities {
        if let (Some(t), Some(b)) = (&entity.components.transform, &entity.components.collider_2d) {
            world.upsert(entity.id.clone(), body_from(t, b))?;
        }
    }
    Ok(world)
}
pub(super) fn apply_components(world: &mut World, entity: Entity, authored: &SceneEntity) {
    let mut e = world.entity_mut(entity);
    e.insert(AuthoredId {
        id: authored.id.clone(),
        name: authored.name.clone(),
    });
    let c = &authored.components;
    if let Some(v) = &c.transform {
        e.insert(v.clone());
    }
    if let Some(v) = &c.script {
        e.insert(v.clone());
    }
    if let Some(v) = &c.mesh_renderer {
        e.insert(v.clone());
    }
    if let Some(v) = &c.sprite_renderer {
        e.insert(v.clone());
    }
    if let Some(v) = &c.camera_2d {
        e.insert(v.clone());
    }
    if let Some(v) = &c.collider_2d {
        e.insert(v.clone());
    }
}
pub(super) fn spawn(world: &mut World, authored: &SceneEntity) -> Entity {
    let entity = world.spawn_empty().id();
    apply_components(world, entity, authored);
    world
        .resource_mut::<EntityLookup>()
        .0
        .insert(authored.id.clone(), entity);
    entity
}
