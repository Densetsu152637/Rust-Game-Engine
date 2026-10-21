use crate::{
    Error, Result,
    content::{self, FontAtlas, Mesh, Texture},
    gameplay2d::{self, Body, TriggerEvent},
    input::{ActionFrame, BindingMap, InputSnapshot},
    persistence::{PersistenceOptions, PersistenceStore},
    schema::*,
    ui::UiModel,
};
use bevy_ecs::prelude::*;
use engine_core::EngineApp;
use serde::Serialize;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};

#[derive(Component, Clone, Debug)]
pub struct AuthoredId {
    pub id: String,
    pub name: String,
}
#[derive(Resource, Clone, Default)]
pub struct EntityLookup(pub BTreeMap<String, Entity>);
impl EntityLookup {
    pub fn get(&self, id: &str) -> Option<Entity> {
        self.0.get(id).copied()
    }
}
#[derive(Resource, Clone, Debug)]
pub struct InputSettings {
    pub bindings: BindingMap,
    pub deadzone: f32,
    pub revision: u64,
}
impl InputSettings {
    pub fn rebind(&mut self, bindings: BindingMap, deadzone: f32) -> Result<()> {
        let check = crate::input::validate_bindings(&bindings, false);
        if !check.errors.is_empty() || !deadzone.is_finite() || !(0.0..1.0).contains(&deadzone) {
            return Err(Error::new(
                "input.bindings.invalid",
                check.errors.join("; "),
            ));
        }
        self.bindings = bindings;
        self.deadzone = deadzone;
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }
}
#[derive(Resource, Clone, Default)]
pub struct TriggerEvents(pub Vec<TriggerEvent>);
#[derive(Resource, Clone, Default)]
pub struct GameState(pub serde_json::Map<String, serde_json::Value>);
#[derive(Resource, Clone)]
pub struct Persistence(pub Arc<PersistenceStore>);
#[derive(Resource, Clone, Default)]
pub struct ControlLocks(pub BTreeSet<String>);
#[derive(Clone, Debug)]
pub struct SceneRequest {
    pub asset: String,
    pub spawn: String,
    pub traveller: String,
}
#[derive(Resource, Clone, Default)]
pub struct PendingScene(pub Option<SceneRequest>);
#[derive(Component, Clone, Copy, Debug)]
pub struct SpriteFrame(pub usize);

/// Native code has full World access; callbacks run serially at lifecycle boundaries.
/// Use ordinary Bevy schedules for data-parallel custom component systems.
pub trait NativeScript: Send + Sync + 'static {
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
struct ActiveScript {
    owner: Entity,
    asset: String,
    script: Box<dyn NativeScript>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawSpace {
    World,
    Ui,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendMode {
    Opaque,
    StraightAlpha,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrawState {
    pub blend: BlendMode,
    pub depth_test: bool,
    pub depth_write: bool,
}
impl Default for DrawState {
    fn default() -> Self {
        Self {
            blend: BlendMode::StraightAlpha,
            depth_test: false,
            depth_write: false,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Draw {
    pub id: String,
    pub mesh: Arc<Mesh>,
    pub texture: Option<Arc<Texture>>,
    pub layer: i32,
    pub space: DrawSpace,
    pub clip: Option<[f32; 4]>,
    pub state: DrawState,
}
#[derive(Clone, Debug)]
pub struct RenderFrame {
    pub camera: Camera2D,
    pub draws: Vec<Draw>,
}
#[derive(Clone, Debug, Serialize)]
pub struct RuntimeSnapshot {
    pub ticks: u64,
    pub scene: Scene,
    pub live_entities: usize,
    pub active_scripts: usize,
    pub scene_revision: u64,
    pub triggers: Vec<TriggerEvent>,
}

#[derive(Default)]
struct Content {
    textures: BTreeMap<String, Arc<Texture>>,
    meshes: BTreeMap<String, Arc<Mesh>>,
    fonts: BTreeMap<String, FontAtlas>,
}
impl Content {
    fn load(project: &Project) -> Result<Self> {
        let mut content = Self::default();
        for asset in project.assets.values() {
            let path = project.resolve_asset(&asset.id)?;
            match asset.kind.as_str() {
                "texture" => {
                    content
                        .textures
                        .insert(asset.id.clone(), Arc::new(content::load_texture(&path)?));
                }
                "mesh" => {
                    content
                        .meshes
                        .insert(asset.id.clone(), Arc::new(content::load_mesh(&path)?));
                }
                "font" => {
                    content
                        .fonts
                        .insert(asset.id.clone(), content::load_font(&path)?);
                }
                _ => {}
            }
        }
        Ok(content)
    }
}

pub struct Runtime {
    app: EngineApp,
    project: Project,
    scene: Scene,
    content: Content,
    scripts: Vec<ActiveScript>,
    started: bool,
    stopped: bool,
    ticks: u64,
    elapsed: f64,
    scene_revision: u64,
    geometry: RefCell<BTreeMap<String, (blake3::Hash, Arc<Mesh>)>>,
}
impl Runtime {
    /// Prepare authored entities. Register native systems/scripts before start or first tick.
    pub fn new(project: Project) -> Result<Self> {
        project.validate_scene(&project.scene)?;
        let content = Content::load(&project)?;
        let scene = project.scene.clone();
        let mut app = EngineApp::new();
        app.world_mut().insert_resource(EntityLookup::default());
        app.world_mut().insert_resource(ActionFrame::default());
        app.world_mut().insert_resource(InputSettings {
            bindings: project.input_bindings.clone(),
            deadzone: 0.2,
            revision: 0,
        });
        app.world_mut().insert_resource(UiModel::default());
        app.world_mut()
            .insert_resource(gameplay2d::World::default());
        app.world_mut().insert_resource(TriggerEvents::default());
        app.world_mut().insert_resource(GameState::default());
        app.world_mut().insert_resource(ControlLocks::default());
        app.world_mut().insert_resource(PendingScene::default());
        app.world_mut().insert_resource(ScriptRegistry::default());
        for entity in &scene.entities {
            spawn(app.world_mut(), entity);
        }
        let collision = collision_from_scene(&scene)?;
        app.world_mut().insert_resource(collision);
        Ok(Self {
            app,
            project,
            scene,
            content,
            scripts: vec![],
            started: false,
            stopped: false,
            ticks: 0,
            elapsed: 0.0,
            scene_revision: 0,
            geometry: RefCell::new(BTreeMap::new()),
        })
    }
    pub fn app(&self) -> &EngineApp {
        &self.app
    }
    pub fn app_mut(&mut self) -> &mut EngineApp {
        &mut self.app
    }
    pub fn project(&self) -> &Project {
        &self.project
    }
    pub fn active_scene(&self) -> &Scene {
        &self.scene
    }
    pub fn entity(&self, id: &str) -> Option<Entity> {
        self.app
            .world()
            .resource::<EntityLookup>()
            .get(id)
            .filter(|entity| self.app.world().get_entity(*entity).is_ok())
    }
    pub fn bindings(&self) -> &BindingMap {
        &self.app.world().resource::<InputSettings>().bindings
    }
    pub fn set_user_data_root(&mut self, root: PathBuf) -> Result<()> {
        if self.started {
            return Err(Error::new(
                "runtime.started",
                "Configure user data before startup",
            ));
        }
        let id = blake3::hash(self.project.name.as_bytes()).to_hex()[..16].to_string();
        let store = PersistenceStore::new(PersistenceOptions::new(root, id))?;
        self.app
            .world_mut()
            .insert_resource(Persistence(Arc::new(store)));
        Ok(())
    }
    pub fn register_script(&mut self, id: impl Into<String>, factory: ScriptFactory) -> Result<()> {
        if self.started {
            return Err(Error::new(
                "runtime.started",
                "Register scripts before startup",
            ));
        }
        register_script(&mut self.app, id, factory)
    }
    fn validate_script_bindings(&self, scene: &Scene) -> Result<()> {
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
    fn attach_scripts(&mut self) -> Result<()> {
        let pending: Vec<_> = self
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
            let mut script = factory(self.app.world_mut(), owner)?;
            if let Err(error) = script.on_create(self.app.world_mut(), owner) {
                let _ = script.on_destroy(self.app.world_mut(), owner);
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
    pub fn start(&mut self) -> Result<()> {
        if self.stopped {
            return Err(Error::new(
                "runtime.stopped",
                "Stopped runtime cannot restart",
            ));
        }
        if self.started {
            return Ok(());
        }
        self.validate_script_bindings(&self.scene)?;
        self.app
            .startup()
            .map_err(|e| Error::new("runtime.startup", e.to_string()))?;
        self.started = true;
        if let Err(error) = self.attach_scripts() {
            let _ = self.stop();
            return Err(error);
        }
        Ok(())
    }
    pub fn tick(&mut self, dt: f32, input: InputSnapshot) -> Result<()> {
        if !dt.is_finite() || dt <= 0.0 {
            return Err(Error::new(
                "runtime.delta.invalid",
                "Delta must be finite positive seconds",
            ));
        }
        self.start()?;
        let result = self.tick_inner(dt, input);
        if result.is_err() {
            let _ = self.stop();
        }
        result
    }
    fn tick_inner(&mut self, dt: f32, input: InputSnapshot) -> Result<()> {
        {
            let world = self.app.world_mut();
            world.resource_mut::<UiModel>().tick(input);
            let mut gameplay = world.resource::<UiModel>().gameplay_frame();
            if !world.resource::<ControlLocks>().0.is_empty() {
                gameplay = ActionFrame {
                    focused: gameplay.focused,
                    ..Default::default()
                };
            }
            world.insert_resource(gameplay);
        }
        self.attach_scripts()?;
        let mut scripts = std::mem::take(&mut self.scripts);
        let mut failure = None;
        for script in &mut scripts {
            if self.app.world().get_entity(script.owner).is_ok()
                && let Err(error) = script
                    .script
                    .on_update(self.app.world_mut(), script.owner, dt)
            {
                failure = Some(error);
                break;
            }
        }
        self.scripts = scripts;
        if let Some(error) = failure {
            return Err(error);
        }
        self.app
            .tick(dt)
            .map_err(|e| Error::new("runtime.update", e.to_string()))?;
        self.sync_collision()?;
        let triggers = self
            .app
            .world_mut()
            .resource_mut::<gameplay2d::World>()
            .advance_triggers();
        self.app.world_mut().resource_mut::<TriggerEvents>().0 = triggers;
        let mut retained = Vec::new();
        for mut script in std::mem::take(&mut self.scripts) {
            let still_attached = self
                .app
                .world()
                .get::<Script>(script.owner)
                .is_some_and(|s| s.asset == script.asset);
            if still_attached {
                retained.push(script);
            } else {
                script
                    .script
                    .on_destroy(self.app.world_mut(), script.owner)?;
            }
        }
        self.scripts = retained;
        let request = self.app.world_mut().resource_mut::<PendingScene>().0.take();
        if let Some(request) = request {
            self.transition(request)?;
        }
        self.ticks += 1;
        self.elapsed += dt as f64;
        Ok(())
    }
    pub fn run_headless(&mut self, ticks: u64, dt: f32) -> Result<()> {
        let result = (|| {
            for _ in 0..ticks {
                self.tick(dt, ActionFrame::default())?;
            }
            Ok(())
        })();
        let stopped = self.stop();
        result.and(stopped)
    }
    pub fn stop(&mut self) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        let mut error = None;
        for mut script in std::mem::take(&mut self.scripts).into_iter().rev() {
            if let Err(e) = script.script.on_destroy(self.app.world_mut(), script.owner)
                && error.is_none()
            {
                error = Some(e);
            }
        }
        self.app.world_mut().resource_mut::<UiModel>().clear();
        if let Err(e) = self.app.shutdown()
            && error.is_none()
        {
            error = Some(Error::new("runtime.shutdown", e.to_string()));
        }
        self.stopped = true;
        if let Some(error) = error {
            Err(error)
        } else {
            Ok(())
        }
    }
    pub fn running(&self) -> bool {
        self.started && !self.stopped
    }
    pub fn position(&self, id: &str) -> Option<[f32; 3]> {
        self.entity(id)
            .and_then(|e| self.app.world().get::<Transform>(e).map(|t| t.position))
    }
    pub fn move_entity(&mut self, id: &str, delta: [f32; 2]) -> Result<gameplay2d::MoveResult> {
        self.sync_collision()?;
        let result = self
            .app
            .world_mut()
            .resource_mut::<gameplay2d::World>()
            .move_body(id, delta)?;
        let entity = self
            .entity(id)
            .ok_or_else(|| Error::new("runtime.entity.unknown", "Unknown entity"))?;
        if let Some(mut transform) = self.app.world_mut().get_mut::<Transform>(entity) {
            transform.position[0] = result.position[0];
            transform.position[1] = result.position[1];
        }
        Ok(result)
    }
    fn sync_collision(&mut self) -> Result<()> {
        let bodies: Vec<_> = self
            .app
            .world()
            .iter_entities()
            .filter_map(|e| {
                Some((
                    e.get::<AuthoredId>()?.id.clone(),
                    e.get::<Transform>()?.clone(),
                    e.get::<Collider2D>()?.clone(),
                ))
            })
            .collect();
        let mut collision = self.app.world_mut().resource_mut::<gameplay2d::World>();
        let ids: BTreeSet<_> = bodies.iter().map(|(id, _, _)| id.clone()).collect();
        let old: Vec<_> = collision
            .ids()
            .filter(|id| !ids.contains(*id))
            .map(str::to_owned)
            .collect();
        for id in old {
            collision.remove(&id);
        }
        for (id, t, b) in bodies {
            collision.upsert(id, body_from(&t, &b))?;
        }
        Ok(())
    }
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
    fn prepare_transition(&self, request: &SceneRequest) -> Result<(Scene, gameplay2d::World)> {
        let asset = self.project.require_asset(&request.asset, "scene")?;
        let mut scene = load_scene(&self.project.root, &asset.path)?;
        self.project.validate_scene(&scene)?;
        if !request.traveller.is_empty() {
            let entity = self
                .entity(&request.traveller)
                .ok_or_else(|| Error::new("runtime.scene.traveller", "Traveller does not exist"))?;
            let old = self.snapshot_entity(entity)?;
            if let Some(target) = scene
                .entities
                .iter_mut()
                .find(|e| e.id == request.traveller)
            {
                *target = old;
            } else {
                scene.entities.push(old);
            }
        }
        if !request.spawn.is_empty() {
            let position = scene
                .entities
                .iter()
                .find(|e| e.id == request.spawn)
                .and_then(|e| e.components.transform.as_ref())
                .map(|t| t.position)
                .ok_or_else(|| {
                    Error::new(
                        "runtime.scene.spawn",
                        "Spawn must name a transformed destination entity",
                    )
                })?;
            let traveller = scene
                .entities
                .iter_mut()
                .find(|e| e.id == request.traveller)
                .ok_or_else(|| {
                    Error::new("runtime.scene.traveller", "Spawn requires a traveller")
                })?;
            traveller
                .components
                .transform
                .as_mut()
                .ok_or_else(|| {
                    Error::new("runtime.scene.traveller", "Traveller requires Transform")
                })?
                .position = position;
        }
        self.project.validate_scene(&scene)?;
        self.validate_script_bindings(&scene)?;
        let collision = collision_from_scene(&scene)?;
        if !request.traveller.is_empty()
            && let Some(body) = collision.body(&request.traveller)
            && !collision.is_safe(body, &request.traveller)?
        {
            return Err(Error::new(
                "runtime.scene.spawn.blocked",
                "Traveller spawn overlaps a solid body",
            ));
        }
        Ok((scene, collision))
    }
    fn transition(&mut self, request: SceneRequest) -> Result<()> {
        // All authored data is prepared before mutating the live generation. Native callback
        // side effects cannot be rolled back; a callback failure stops the generation.
        let (scene, collision) = self.prepare_transition(&request)?;
        let retained = self.entity(&request.traveller);
        let mut keep_scripts = Vec::new();
        for mut script in std::mem::take(&mut self.scripts) {
            if Some(script.owner) == retained {
                keep_scripts.push(script);
            } else {
                script
                    .script
                    .on_destroy(self.app.world_mut(), script.owner)?;
            }
        }
        self.scripts = keep_scripts;
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
        self.attach_scripts()?;
        Ok(())
    }
    fn snapshot_entity(&self, entity: Entity) -> Result<SceneEntity> {
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
            live_entities: self.app.world().entities().len() as usize,
            active_scripts: self.scripts.len(),
            scene_revision: self.scene_revision,
            triggers: self.app.world().resource::<TriggerEvents>().0.clone(),
        }
    }
    fn cached_mesh(&self, id: &str, mesh: Mesh) -> Arc<Mesh> {
        let hash = blake3::hash(&serde_json::to_vec(&mesh).expect("finite geometry"));
        let mut cache = self.geometry.borrow_mut();
        if let Some((previous, cached)) = cache.get(id)
            && *previous == hash
        {
            return cached.clone();
        }
        let mesh = Arc::new(mesh);
        cache.insert(id.into(), (hash, mesh.clone()));
        mesh
    }
    pub fn extract_frame(&self) -> Result<RenderFrame> {
        let world = self.app.world();
        let mut camera = world
            .iter_entities()
            .find_map(|e| e.get::<Camera2D>().cloned())
            .unwrap_or_default();
        if let Some(position) = camera.follow.as_ref().and_then(|id| self.position(id)) {
            camera.center = [position[0], position[1]];
        }
        let mut draws = Vec::new();
        for entity in world.iter_entities() {
            let Some(transform) = entity.get::<Transform>() else {
                continue;
            };
            let id = entity
                .get::<AuthoredId>()
                .map(|a| a.id.clone())
                .unwrap_or_else(|| format!("entity:{}", entity.id().to_bits()));
            if let Some(renderer) = entity.get::<SpriteRenderer>().filter(|s| s.visible) {
                let texture = self
                    .content
                    .textures
                    .get(&renderer.texture)
                    .ok_or_else(|| Error::new("render.texture.unknown", &renderer.texture))?
                    .clone();
                let frame = entity.get::<SpriteFrame>().map(|f| f.0).unwrap_or_else(|| {
                    content::animation_frame(
                        self.elapsed,
                        renderer.frames_per_second,
                        renderer.frames.len(),
                        renderer.looping,
                    )
                });
                let source = if renderer.frames.is_empty() {
                    renderer
                        .source
                        .unwrap_or([0, 0, texture.width, texture.height])
                } else {
                    renderer.frames[frame.min(renderer.frames.len() - 1)]
                };
                let mut position = transform.position;
                if camera.pixel_snap {
                    for v in &mut position[..2] {
                        *v = (*v * camera.pixels_per_unit).round() / camera.pixels_per_unit;
                    }
                }
                let mesh = content::sprite_quad(
                    position,
                    renderer.size,
                    renderer.pivot,
                    source,
                    &texture,
                    renderer.tint,
                )?;
                let draw_id = format!("sprite:{id}");
                draws.push(Draw {
                    id: draw_id.clone(),
                    mesh: self.cached_mesh(&draw_id, mesh),
                    texture: Some(texture),
                    layer: renderer.layer,
                    space: DrawSpace::World,
                    clip: None,
                    state: DrawState::default(),
                });
            }
            if let Some(renderer) = entity.get::<MeshRenderer>() {
                let original = self
                    .content
                    .meshes
                    .get(&renderer.mesh)
                    .ok_or_else(|| Error::new("render.mesh.unknown", &renderer.mesh))?;
                let mut mesh = (**original).clone();
                for vertex in &mut mesh.vertices {
                    for axis in 0..3 {
                        vertex.position[axis] += transform.position[axis];
                    }
                }
                let texture = renderer
                    .texture
                    .as_ref()
                    .map(|id| {
                        self.content
                            .textures
                            .get(id)
                            .cloned()
                            .ok_or_else(|| Error::new("render.texture.unknown", id))
                    })
                    .transpose()?;
                let draw_id = format!("mesh:{id}");
                draws.push(Draw {
                    id: draw_id.clone(),
                    mesh: self.cached_mesh(&draw_id, mesh),
                    texture,
                    layer: 0,
                    space: DrawSpace::World,
                    clip: None,
                    state: DrawState {
                        blend: BlendMode::Opaque,
                        depth_test: true,
                        depth_write: true,
                    },
                });
            }
        }
        for panel in world.resource::<UiModel>().panels() {
            let o = &panel.options;
            let font = self
                .content
                .fonts
                .get(&o.font_asset)
                .ok_or_else(|| Error::new("render.font.unknown", &o.font_asset))?;
            let texture = self
                .content
                .textures
                .get(&font.texture)
                .ok_or_else(|| Error::new("render.texture.unknown", &font.texture))?
                .clone();
            let mut text = panel.lines.join("\n");
            for (index, choice) in o.choices.iter().enumerate() {
                text.push('\n');
                if index == panel.selected {
                    text.push_str("> ");
                } else {
                    text.push_str("  ");
                }
                text.push_str(choice);
            }
            let mesh = content::text_mesh(
                font,
                &texture,
                &text,
                [o.rect.x, o.rect.y - panel.scroll_offset],
                0.0,
                o.scale,
                o.color,
            )?;
            let id = format!("ui:{}", o.id);
            draws.push(Draw {
                id: id.clone(),
                mesh: self.cached_mesh(&id, mesh),
                texture: Some(texture),
                layer: o.layer,
                space: DrawSpace::Ui,
                clip: Some([
                    panel.clip.x,
                    panel.clip.y,
                    panel.clip.width,
                    panel.clip.height,
                ]),
                state: DrawState::default(),
            });
        }
        draws.sort_by(|a, b| {
            (a.space == DrawSpace::Ui, a.layer, &a.id).cmp(&(
                b.space == DrawSpace::Ui,
                b.layer,
                &b.id,
            ))
        });
        let active: BTreeSet<_> = draws.iter().map(|d| d.id.clone()).collect();
        self.geometry
            .borrow_mut()
            .retain(|id, _| active.contains(id));
        Ok(RenderFrame { camera, draws })
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
fn body_from(t: &Transform, b: &Collider2D) -> Body {
    Body {
        position: [t.position[0], t.position[1]],
        size: b.size,
        offset: b.offset,
        trigger: b.trigger,
        layer: b.layer,
        mask: b.mask,
    }
}
fn collision_from_scene(scene: &Scene) -> Result<gameplay2d::World> {
    let mut world = gameplay2d::World::default();
    for entity in &scene.entities {
        if let (Some(t), Some(b)) = (&entity.components.transform, &entity.components.collider_2d) {
            world.upsert(entity.id.clone(), body_from(t, b))?;
        }
    }
    Ok(world)
}
fn apply_components(world: &mut World, entity: Entity, authored: &SceneEntity) {
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
fn spawn(world: &mut World, authored: &SceneEntity) -> Entity {
    let entity = world.spawn_empty().id();
    apply_components(world, entity, authored);
    world
        .resource_mut::<EntityLookup>()
        .0
        .insert(authored.id.clone(), entity);
    entity
}
