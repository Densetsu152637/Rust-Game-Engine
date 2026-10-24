use super::*;

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeSnapshot {
    /// Nonempty means the compatibility snapshot is incomplete; hosts use try_snapshot.
    pub diagnostics: Vec<crate::Diagnostic>,
    pub events: Vec<crate::events::EventRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<crate::networking::NetworkStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_devices: Option<crate::input::InputDevicesSnapshot>,
    /// Renderer-reported temporal upscaler state; absent without a 3D camera.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upscaler: Option<crate::rendering3d::UpscalerStatus>,
    /// Renderer-reported frame generation state; absent without a 3D camera.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_generation: Option<crate::rendering3d::FrameGenerationStatus>,
    /// XR host status; `not_attached` when XR is enabled without a host.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xr: Option<crate::xr::XrStatusReport>,
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
        crate::catalog::hydrate(
            self.app.world_mut(),
            &self.scene,
            &self.project.component_catalog,
        )?;
        crate::networking::validate(self.app.world())?;
        crate::simulation::reset(self.app.world_mut())?;
        crate::refresh_world_transforms(self.app.world_mut())?;
        self.validate_event_bindings()?;
        crate::events::clear(self.app.world_mut());
        self.app
            .world_mut()
            .resource_mut::<TriggerEvents>()
            .0
            .clear();
        self.app.world_mut().resource_mut::<UiModel>().clear();
        self.app
            .world_mut()
            .resource_mut::<crate::ui_tree::GameUi>()
            .clear();
        crate::audio::synchronize(self.app.world_mut())?;
        self.attach_scripts()?;
        Ok(())
    }
    pub(super) fn snapshot_entity(&self, entity: Entity) -> Result<SceneEntity> {
        crate::services::snapshot_entity(self.app.world(), entity)
    }
    pub fn snapshot(&self) -> RuntimeSnapshot {
        let mut scene = self.scene.clone();
        let mut diagnostics = Vec::new();
        scene.entities = self
            .app
            .world()
            .resource::<EntityLookup>()
            .0
            .values()
            .filter(|e| self.app.world().get_entity(**e).is_ok())
            .filter_map(|e| match self.snapshot_entity(*e) {
                Ok(entity) => Some(entity),
                Err(error) => {
                    diagnostics.extend(error.diagnostics);
                    None
                }
            })
            .collect();
        RuntimeSnapshot {
            diagnostics,
            events: crate::events::records(self.app.world()),
            network: crate::networking::status(self.app.world()),
            input_devices: self
                .app
                .world()
                .get_resource::<crate::input::InputDevices>()
                .map(crate::input::InputDevices::snapshot),
            upscaler: self.upscaler_status(),
            frame_generation: self.frame_generation_status(),
            xr: crate::xr::status(self.app.world()),
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
    pub fn try_snapshot(&self) -> Result<RuntimeSnapshot> {
        let snapshot = self.snapshot();
        if snapshot.diagnostics.is_empty() {
            Ok(snapshot)
        } else {
            Err(Error {
                diagnostics: snapshot.diagnostics,
            })
        }
    }
}
pub(crate) fn collision_from_scene(scene: &Scene) -> Result<gameplay2d::World> {
    let mut world = gameplay2d::World::default();
    for entity in &scene.entities {
        if let Some(collider) = &entity.components.collider_2d {
            world.upsert(
                entity.id.clone(),
                crate::transforms::collider_body(
                    crate::transforms::scene_world_transform(scene, &entity.id)?,
                    collider,
                ),
            )?;
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
    if let Some(value) = &c.camera3d {
        e.insert(value.clone());
    } else {
        e.remove::<crate::rendering3d::Camera3D>();
    }
    if let Some(value) = &c.material {
        e.insert(value.clone());
    } else {
        e.remove::<crate::rendering3d::MeshMaterial>();
    }
    if let Some(parent) = &authored.parent {
        e.insert(crate::Parent(parent.clone()));
    } else {
        e.remove::<crate::Parent>();
    }
    e.insert(crate::catalog::DynamicComponents(c.custom.clone()));
    if let Some(bindings) = &c.event_bindings {
        e.insert(bindings.clone());
    } else {
        e.remove::<crate::EventBindings>();
    }
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
pub(crate) fn spawn(world: &mut World, authored: &SceneEntity) -> Entity {
    let entity = world.spawn_empty().id();
    apply_components(world, entity, authored);
    world
        .resource_mut::<EntityLookup>()
        .0
        .insert(authored.id.clone(), entity);
    entity
}
