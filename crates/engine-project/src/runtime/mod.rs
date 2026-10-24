use crate::{
    Error, Result,
    content::{self, FontAtlas, Mesh, Texture},
    gameplay2d::{self, TriggerEvent},
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

mod events;
mod render;
mod render3d;
mod resources;
mod scene;
mod scripts;

pub use crate::audio::RuntimeAudio;
pub use crate::services::{
    find_entity, gameplay_input, move_entity, refresh_gameplay_input, request_scene, resolve_asset,
    safe_position,
};
use render::Content;
pub use render::{BlendMode, Draw, DrawMotion, DrawSpace, DrawState, RenderFrame, snap_to_pixels};
pub use resources::*;
pub use scene::RuntimeSnapshot;
pub(crate) use scene::collision_from_scene;
pub(crate) use scene::spawn;
use scripts::{ActiveScript, destroy_script, invoke_owned};
pub use scripts::{NativeScript, ScriptFactory, ScriptRegistry, register_script, spawn_owned};

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
        Self::new_with_simulation(
            project,
            Some(crate::simulation::SimulationConfig::default()),
        )
    }
    /// Built-in schemas stay available when services are disabled; active writers then fail validation.
    pub fn new_with_simulation(
        project: Project,
        simulation: Option<crate::simulation::SimulationConfig>,
    ) -> Result<Self> {
        project.validate_scene(&project.scene)?;
        let content = Content::load(&project)?;
        let scene = project.scene.clone();
        let mut app = EngineApp::new();
        app.world_mut()
            .insert_resource(ProjectResource(Arc::new(project.clone())));
        crate::simulation::register_components(&mut app)?;
        crate::ui_tree::register(&mut app)?;
        crate::networking::register(&mut app)?;
        crate::simulation::install(app.world_mut(), simulation)?;
        app.world_mut().insert_resource(RuntimeLog::default());
        app.world_mut().insert_resource(EntityLookup::default());
        app.world_mut().insert_resource(ActionFrame::default());
        app.world_mut().insert_resource(InputSettings {
            bindings: project.input_bindings.clone(),
            deadzone: 0.2,
            revision: 0,
        });
        let mut ui = UiModel::default();
        for (id, font) in &content.fonts {
            let font = font.clone();
            ui.set_font_metrics(id.clone(), Arc::new(move |c| font.glyph(c).advance));
        }
        app.world_mut().insert_resource(ui);
        app.world_mut()
            .insert_resource(gameplay2d::World::default());
        app.world_mut().insert_resource(TriggerEvents::default());
        app.world_mut().insert_resource(GameState::default());
        app.world_mut().insert_resource(ControlLocks::default());
        app.world_mut().insert_resource(PendingScene::default());
        app.world_mut().insert_resource(ScriptRegistry::default());
        app.world_mut()
            .insert_resource(crate::rendering3d::UpscalerControl::default());
        app.world_mut()
            .insert_resource(crate::rendering3d::FrameGenerationControl::default());
        crate::events::clear(app.world_mut());
        for entity in &scene.entities {
            spawn(app.world_mut(), entity);
        }
        let collision = collision_from_scene(&scene)?;
        app.world_mut().insert_resource(collision);
        let mut runtime = Self {
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
        };
        let id = crate::persistence::project_id(&runtime.project.name);
        if let Ok(root) = PersistenceStore::default_user_data_root(&id) {
            runtime.set_user_data_root(root)?;
        }
        Ok(runtime)
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
    pub fn set_audio(&mut self, control: engine_audio::AudioControl) -> Result<()> {
        if self.started {
            return Err(Error::new(
                "runtime.started",
                "Configure audio before startup",
            ));
        }
        let audio = crate::audio::RuntimeAudio::new(control, &self.project)?;
        self.app.world_mut().insert_resource(audio);
        Ok(())
    }
    pub fn set_user_data_root(&mut self, root: PathBuf) -> Result<()> {
        if self.started {
            return Err(Error::new(
                "runtime.started",
                "Configure user data before startup",
            ));
        }
        let id = crate::persistence::project_id(&self.project.name);
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
        crate::catalog::hydrate(
            self.app.world_mut(),
            &self.scene,
            &self.project.component_catalog,
        )?;
        self.validate_event_bindings()?;
        crate::networking::validate(self.app.world())?;
        crate::simulation::prepare(self.app.world_mut())?;
        crate::transforms::refresh_world_transforms(self.app.world_mut())?;
        self.sync_collision()?;
        if let Err(error) = self.app.startup() {
            let error = Error::new("runtime.startup", error.to_string());
            let _ = self.stop();
            return Err(error);
        }
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
        if let Err(error) = self.start() {
            let _ = self.stop();
            return Err(error);
        }
        let result = self.tick_inner(dt, input);
        if result.is_err() {
            let _ = self.stop();
        }
        result
    }
    fn tick_inner(&mut self, dt: f32, input: InputSnapshot) -> Result<()> {
        let input = crate::input::poll_devices(self.app.world_mut(), input);
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
        crate::networking::validate(self.app.world())?;
        crate::networking::before_step(self.app.world_mut(), dt)?;
        self.attach_scripts()?;
        self.dispatch_events()?;
        let viewport = self.extract_camera().logical_size;
        let ui_input = self.app.world().resource::<UiModel>().gameplay_frame();
        crate::ui_tree::tick_world(self.app.world_mut(), viewport, &ui_input)?;
        let mut scripts = std::mem::take(&mut self.scripts);
        let mut failure = None;
        for script in &mut scripts {
            if self
                .app
                .world()
                .get::<Script>(script.owner)
                .is_some_and(|s| s.asset == script.asset)
                && let Err(error) = invoke_owned(self.app.world_mut(), script.owner, |world| {
                    script.script.on_update(world, script.owner, dt)
                })
            {
                failure = Some(error);
                break;
            }
        }
        self.scripts = scripts;
        if let Some(error) = failure {
            return Err(error);
        }
        refresh_gameplay_input(self.app.world_mut());
        let tick = self.ticks + 1;
        self.app
            .tick_with_post_update(dt, |world| {
                crate::networking::validate(world)?;
                crate::simulation::step(world, dt)?;
                crate::networking::after_step(world, tick)?;
                crate::transforms::refresh_world_transforms(world)?;
                Ok(())
            })
            .map_err(|e| Error::new("runtime.update", e.to_string()))?;
        self.sync_collision()?;
        crate::audio::synchronize(self.app.world_mut())?;
        let triggers = self
            .app
            .world_mut()
            .resource_mut::<gameplay2d::World>()
            .advance_triggers();
        self.app.world_mut().resource_mut::<TriggerEvents>().0 = triggers;
        self.queue_trigger_events()?;
        crate::audio::notify_triggers(self.app.world_mut())?;
        let mut retained = Vec::new();
        let mut retirement_error = None;
        for script in std::mem::take(&mut self.scripts) {
            let still_attached = self
                .app
                .world()
                .get::<Script>(script.owner)
                .is_some_and(|s| s.asset == script.asset);
            if still_attached {
                retained.push(script);
            } else {
                if let Err(error) = destroy_script(self.app.world_mut(), script)
                    && retirement_error.is_none()
                {
                    retirement_error = Some(error);
                }
            }
        }
        self.scripts = retained;
        if let Some(error) = retirement_error {
            return Err(error);
        }
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
        crate::networking::shutdown(self.app.world_mut());
        for script in std::mem::take(&mut self.scripts).into_iter().rev() {
            if let Err(e) = destroy_script(self.app.world_mut(), script)
                && error.is_none()
            {
                error = Some(e);
            }
        }
        self.app.world_mut().resource_mut::<UiModel>().clear();
        self.app
            .world_mut()
            .resource_mut::<crate::ui_tree::GameUi>()
            .clear();
        self.app
            .world_mut()
            .resource_mut::<ControlLocks>()
            .0
            .clear();
        self.app.world_mut().resource_mut::<PendingScene>().0 = None;
        let remaining: Vec<_> = self
            .app
            .world()
            .iter_entities()
            .filter(|e| e.contains::<OwnedBy>())
            .map(|e| e.id())
            .collect();
        for entity in remaining {
            self.app.world_mut().despawn(entity);
        }
        if let Err(e) = crate::audio::shutdown(self.app.world_mut())
            && error.is_none()
        {
            error = Some(e);
        }
        if let Err(e) = self.app.shutdown()
            && error.is_none()
        {
            error = Some(Error::new("runtime.shutdown", e.to_string()));
        }
        self.stopped = true;
        crate::events::discard_pending(self.app.world_mut());
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
        self.entity(id).and_then(|e| {
            crate::world_transform(self.app.world(), e)
                .ok()
                .map(|t| t.position())
        })
    }
    pub fn move_entity(&mut self, id: &str, delta: [f32; 2]) -> Result<gameplay2d::MoveResult> {
        crate::services::move_entity(self.app.world_mut(), id, delta)
    }
    fn sync_collision(&mut self) -> Result<()> {
        crate::services::sync_collision(self.app.world_mut())
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
