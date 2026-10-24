use std::{
    any::TypeId,
    collections::{HashMap, HashSet},
    time::Instant,
};

use crate::profile::{ProfileSnapshot, Profiler};
use anyhow::{Result, anyhow, bail, ensure};
use engine_ecs::{
    ExtractionSet, RenderWorld,
    bevy_ecs::{
        prelude::*,
        schedule::{LogLevel, ScheduleBuildSettings, ScheduleLabel},
    },
    cleanup_extracted_entities, extract_component,
};
use tracing::{debug, error, info};

/// Startup/update/extract/shutdown use the simulation world; render uses its snapshot.
#[derive(ScheduleLabel, Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Startup,
    Update,
    Extract,
    Render,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    Configuring,
    Running,
    Failed,
    Stopped,
}

/// Simulation seconds and index of the update currently being executed.
/// Advanced before update systems; never derived from wall-clock timing.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct Time {
    pub dt: f32,
    pub elapsed: f64,
    pub tick: u64,
}

/// A system can request stopping at the next phase boundary.
#[derive(Resource, Debug, Clone, Default)]
pub struct RuntimeControl {
    stop_requested: bool,
}

impl RuntimeControl {
    pub fn request_stop(&mut self) {
        self.stop_requested = true;
    }

    pub fn stop_requested(&self) -> bool {
        self.stop_requested
    }
}

/// One native runtime generation. No window, project schema, or device dependency.
///
/// Schedules reject cycles and unordered conflicting accesses by default. A native
/// schedule can deliberately change its build settings. Bevy cannot diagnose an
/// ordering edge to an unpopulated label or a label in another schedule; callers
/// must register those targets in the same schedule and test the intended order.
/// System panics remain panics: process supervision, not this object, isolates them.
pub struct EngineApp {
    world: World,
    render: RenderWorld,
    schedules: HashMap<Stage, Schedule>,
    extracted_types: HashSet<TypeId>,
    state: AppState,
    profiler: Profiler,
}

impl Default for EngineApp {
    fn default() -> Self {
        Self::new()
    }
}

impl EngineApp {
    pub fn new() -> Self {
        let mut world = World::new();
        world.init_resource::<Time>();
        world.init_resource::<RuntimeControl>();
        let mut schedules = HashMap::new();
        for stage in [
            Stage::Startup,
            Stage::Update,
            Stage::Extract,
            Stage::Render,
            Stage::Shutdown,
        ] {
            let mut schedule = Schedule::new(stage);
            schedule.set_build_settings(ScheduleBuildSettings {
                ambiguity_detection: LogLevel::Error,
                ..Default::default()
            });
            schedules.insert(stage, schedule);
        }
        let mut app = Self {
            world,
            render: RenderWorld::default(),
            schedules,
            extracted_types: HashSet::new(),
            state: AppState::Configuring,
            profiler: Profiler::default(),
        };
        app.schedule_mut(Stage::Extract)
            .configure_sets((ExtractionSet::Cleanup, ExtractionSet::Components).chain())
            .add_systems(cleanup_extracted_entities.in_set(ExtractionSet::Cleanup));
        app
    }

    pub fn world(&self) -> &World {
        &self.world
    }
    pub fn world_mut(&mut self) -> &mut World {
        &mut self.world
    }
    pub fn render_world(&self) -> &World {
        self.render.world()
    }
    pub fn render_world_mut(&mut self) -> &mut World {
        self.render.world_mut()
    }
    pub fn render_snapshot(&self) -> &RenderWorld {
        &self.render
    }
    pub fn state(&self) -> AppState {
        self.state
    }
    pub fn is_running(&self) -> bool {
        self.state == AppState::Running
    }
    pub fn time(&self) -> &Time {
        self.world.resource::<Time>()
    }

    /// CPU wall-clock timings; independent of deterministic simulation time.
    pub fn profile(&self) -> ProfileSnapshot {
        self.profiler.snapshot()
    }

    /// Retains the newest frames. Zero disables frame retention, not aggregates.
    pub fn set_profile_capacity(&mut self, capacity: usize) -> Result<()> {
        self.profiler.set_capacity(capacity)
    }

    pub fn schedule_mut(&mut self, stage: Stage) -> &mut Schedule {
        self.schedules.get_mut(&stage).expect("all stages exist")
    }

    /// Installs native change-driven extraction once for this component type.
    pub fn extract_component<T: Component + Clone>(&mut self) -> &mut Self {
        if self.extracted_types.insert(TypeId::of::<T>()) {
            self.schedule_mut(Stage::Extract).add_systems(
                extract_component::<T>
                    .in_set(ExtractionSet::Components)
                    // These systems modify distinct component types in one render
                    // world. Bevy serializes the resource access; either order is valid.
                    .ambiguous_with(ExtractionSet::Components),
            );
        }
        self
    }

    pub fn request_stop(&mut self) {
        info!("stop requested by host");
        self.world.resource_mut::<RuntimeControl>().request_stop();
    }

    pub fn stop_requested(&self) -> bool {
        self.world.resource::<RuntimeControl>().stop_requested()
    }

    /// Executes startup exactly once. An already running app is unchanged.
    pub fn startup(&mut self) -> Result<()> {
        match self.state {
            AppState::Running => return Ok(()),
            AppState::Configuring => {}
            other => bail!("cannot start app in state {other:?}; create a new generation"),
        }
        info!("engine app starting");
        self.state = AppState::Running;
        let result = self.run_stage(Stage::Startup);
        if result.is_ok() {
            debug!("engine app started");
        }
        result
    }

    /// Advances one fixed update, extraction, and render. A stop request ends
    /// execution after the current phase and runs shutdown. Delta must be finite
    /// and positive. Graph errors mark the app failed; call shutdown for cleanup.
    pub fn tick(&mut self, dt: f32) -> Result<()> {
        ensure!(
            dt.is_finite() && dt > 0.0,
            "dt must be finite and positive seconds"
        );
        self.startup()?;
        if self.stop_requested() {
            return self.shutdown();
        }
        {
            let mut time = self.world.resource_mut::<Time>();
            time.tick = time
                .tick
                .checked_add(1)
                .ok_or_else(|| anyhow!("simulation tick overflow"))?;
            time.dt = dt;
            time.elapsed += f64::from(dt);
        }
        let started = Instant::now();
        self.profiler.begin_frame(self.time().tick);
        let result = self.tick_stages();
        self.profiler.end_frame(
            started.elapsed(),
            self.world
                .archetypes()
                .iter()
                .map(|archetype| archetype.len())
                .sum(),
            self.render
                .world()
                .archetypes()
                .iter()
                .map(|archetype| archetype.len())
                .sum(),
            result.is_ok(),
        );
        result
    }

    fn tick_stages(&mut self) -> Result<()> {
        for stage in [Stage::Update, Stage::Extract, Stage::Render] {
            self.run_stage(stage)?;
            if self.stop_requested() {
                return self.shutdown();
            }
        }
        // Standalone Bevy applications own removal-event maintenance. System
        // change ticks retain independent readers' observations.
        self.world.clear_trackers();
        self.render.world_mut().clear_trackers();
        Ok(())
    }

    /// Runs at most `ticks` updates without sleeping, then shuts down, including
    /// after a returned graph error. Returns the original error with cleanup context.
    pub fn run_fixed(&mut self, ticks: u64, dt: f32) -> Result<()> {
        ensure!(
            dt.is_finite() && dt > 0.0,
            "dt must be finite and positive seconds"
        );
        let run: Result<()> = (|| {
            self.startup()?;
            for _ in 0..ticks {
                if !self.is_running() || self.stop_requested() {
                    break;
                }
                self.tick(dt)?;
            }
            Ok(())
        })();
        let cleanup = self.shutdown();
        match (run, cleanup) {
            (Err(error), Err(cleanup)) => {
                Err(error.context(format!("shutdown also failed: {cleanup:#}")))
            }
            (Err(error), _) => Err(error),
            (_, result) => result,
        }
    }

    /// Runs shutdown once even after a graph failure. No implicit shutdown in
    /// Drop: hosts must call this before dropping device-owning resources.
    pub fn shutdown(&mut self) -> Result<()> {
        if self.state == AppState::Stopped {
            return Ok(());
        }
        if self.state == AppState::Configuring {
            self.state = AppState::Stopped;
            return Ok(());
        }
        info!(ticks = self.time().tick, "engine app shutting down");
        let result = self.run_stage(Stage::Shutdown);
        self.state = AppState::Stopped;
        match &result {
            Ok(()) => info!("engine app stopped"),
            Err(error) => error!(%error, "engine app shutdown failed"),
        }
        result
    }

    fn run_stage(&mut self, stage: Stage) -> Result<()> {
        let started = Instant::now();
        let result = tracing::trace_span!("stage", ?stage).in_scope(|| self.execute_stage(stage));
        self.profiler
            .record_stage(stage, started.elapsed(), result.is_ok());
        if let Err(error) = &result {
            error!(?stage, %error, "stage failed; app marked failed");
            self.state = AppState::Failed;
        }
        result
    }

    fn execute_stage(&mut self, stage: Stage) -> Result<()> {
        if stage == Stage::Extract {
            ensure!(
                !self.world.contains_resource::<RenderWorld>(),
                "RenderWorld is reserved for extraction"
            );
            self.world.insert_resource(std::mem::take(&mut self.render));
        }
        if stage == Stage::Render {
            self.render
                .world_mut()
                .insert_resource(self.world.resource::<RuntimeControl>().clone());
            self.render
                .world_mut()
                .insert_resource(*self.world.resource::<Time>());
        }
        let schedule = self.schedules.get_mut(&stage).expect("all stages exist");
        let world = if stage == Stage::Render {
            self.render.world_mut()
        } else {
            &mut self.world
        };
        let result = schedule
            .initialize(world)
            .map_err(|error| anyhow!("{stage:?} schedule is invalid: {error}"))
            .map(|_| schedule.run(world));
        if stage == Stage::Extract {
            self.render = self
                .world
                .remove_resource::<RenderWorld>()
                .ok_or_else(|| anyhow!("an extraction system removed the reserved RenderWorld"))?;
        }
        if stage == Stage::Render
            && self
                .render
                .world()
                .get_resource::<RuntimeControl>()
                .is_some_and(RuntimeControl::stop_requested)
        {
            info!("stop requested by render systems");
            self.request_stop();
        }
        result
    }
}
