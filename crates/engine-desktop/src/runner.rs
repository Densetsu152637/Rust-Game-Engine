use crate::{
    CameraView, GpuRequirements, NativeRenderPasses, RenderLimits, WindowSurface,
    input::{Gamepads, window_input},
};
use anyhow::{Context, Result, ensure};
use engine_audio::{AudioConfig, AudioEngine};
use engine_project::{
    Runtime,
    input::{ActionMapper, RawInput, merge_input},
    runtime::{InputSettings, RuntimeSnapshot},
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

pub const FIXED_DELTA: f64 = 1.0 / 60.0;

pub struct RunOptions {
    pub headless: bool,
    /// Number of fixed simulation updates, in 1..=1,000,000.
    pub ticks: u64,
    /// Zero-based fixed tick -> held injected actions. An absent action releases
    /// on the following tick; consecutive entries stay held without new presses.
    pub inputs: BTreeMap<u64, BTreeSet<String>>,
    pub user_data: Option<PathBuf>,
    /// Checked at tick boundaries. Cooperative shutdown runs game cleanup before
    /// releasing native devices; the generation launcher owns the flag.
    pub shutdown: Option<Arc<AtomicBool>>,
}
impl Default for RunOptions {
    fn default() -> Self {
        Self {
            headless: false,
            ticks: 1_000_000,
            inputs: BTreeMap::new(),
            user_data: None,
            shutdown: None,
        }
    }
}

#[derive(Serialize)]
pub struct RunReport {
    pub ticks: u64,
    pub fixed_delta: f64,
    pub scene: RuntimeSnapshot,
    pub presented_frames: u64,
}

/// Run a native generation. Headless mode never initializes winit, GPU, gamepad
/// or audio devices. Native mode must be called from the process main thread.
pub fn run(mut runtime: Runtime, options: RunOptions) -> Result<RunReport> {
    ensure!(
        (1..=1_000_000).contains(&options.ticks),
        "ticks must be 1..=1000000"
    );
    for (tick, actions) in &options.inputs {
        ensure!(
            *tick < options.ticks,
            "input tick {tick} exceeds run length"
        );
        for action in actions {
            ensure!(
                runtime.project().input_bindings.contains_key(action),
                "unknown injected action {action}"
            );
        }
    }
    if let Some(path) = &options.user_data {
        runtime.set_user_data_root(path.clone())?;
    }
    let audio = if options.headless {
        AudioEngine::offline(AudioConfig::default())?
    } else {
        AudioEngine::device(AudioConfig::default())?
    };
    runtime.set_audio(audio.control())?;
    let mapper = ActionMapper::new(runtime.bindings().clone())?;
    let start = runtime.start();
    if let Err(error) = start {
        return Err(stop_after_error(&mut runtime, error.into()));
    }
    if options.headless {
        return run_headless(runtime, options, audio);
    }
    let event_loop = match EventLoop::new() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            return Err(stop_after_error(
                &mut runtime,
                anyhow::Error::from(error).context("creating main-thread event loop"),
            ));
        }
    };
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = DesktopRunner {
        runtime,
        audio,
        options,
        surface: None,
        mapper,
        raw: RawInput::default(),
        pointer: [0.0; 2],
        gamepads: None,
        occluded: false,
        previous: BTreeSet::new(),
        ticks: 0,
        presented_frames: 0,
        next_tick: Instant::now(),
        error: None,
    };
    let event_result = event_loop.run_app(&mut app).context("desktop event loop");
    let cleanup = app.runtime.stop().map_err(anyhow::Error::from);
    let audio_error = app.audio.device_error();
    app.audio.shutdown();
    let gpu_cleanup = app
        .surface
        .as_ref()
        .map_or(Ok(()), |host| host.renderer.gpu.wait_idle());
    combine(
        app.error.take().map_or(event_result, Err),
        combine(
            combine(cleanup, gpu_cleanup),
            audio_error.map_or(Ok(()), |e| Err(e.into())),
        ),
    )?;
    Ok(RunReport {
        ticks: app.ticks,
        fixed_delta: FIXED_DELTA,
        scene: app.runtime.snapshot(),
        presented_frames: app.presented_frames,
    })
}

fn combine(result: Result<()>, cleanup: Result<()>) -> Result<()> {
    match (result, cleanup) {
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("cleanup also failed: {cleanup:#}")))
        }
        (Err(error), _) => Err(error),
        (_, cleanup) => cleanup,
    }
}

fn stop_after_error(runtime: &mut Runtime, error: anyhow::Error) -> anyhow::Error {
    match runtime.stop() {
        Ok(()) => error,
        Err(cleanup) => error.context(format!("runtime cleanup also failed: {cleanup}")),
    }
}

fn run_headless(
    mut runtime: Runtime,
    options: RunOptions,
    mut audio: AudioEngine,
) -> Result<RunReport> {
    let mut previous = BTreeSet::new();
    let mut ticks = 0;
    let mut audio_frame = [0.0_f32; 1600]; // 48,000 / 60 frames, stereo.
    let result = (|| -> Result<()> {
        for tick in 0..options.ticks {
            if stop_requested(&runtime, &options) {
                break;
            }
            let actions = options.inputs.get(&tick).cloned().unwrap_or_default();
            let input = merge_input(&Default::default(), &actions, &mut previous);
            runtime.tick(FIXED_DELTA as f32, input)?;
            audio.mix(&mut audio_frame)?;
            ticks += 1;
        }
        Ok(())
    })();
    let cleanup = runtime.stop().map_err(anyhow::Error::from);
    audio.shutdown();
    combine(result, cleanup)?;
    Ok(RunReport {
        ticks,
        fixed_delta: FIXED_DELTA,
        scene: runtime.snapshot(),
        presented_frames: 0,
    })
}

fn stop_requested(runtime: &Runtime, options: &RunOptions) -> bool {
    options
        .shutdown
        .as_ref()
        .is_some_and(|flag| flag.load(Ordering::Acquire))
        || runtime.app().stop_requested()
        || !runtime.app().is_running()
}

struct DesktopRunner {
    runtime: Runtime,
    audio: AudioEngine,
    options: RunOptions,
    surface: Option<WindowSurface>,
    mapper: ActionMapper,
    raw: RawInput,
    pointer: [f32; 2],
    gamepads: Option<Gamepads>,
    occluded: bool,
    previous: BTreeSet<String>,
    ticks: u64,
    presented_frames: u64,
    next_tick: Instant,
    error: Option<anyhow::Error>,
}
impl DesktopRunner {
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: anyhow::Error) {
        self.error = Some(error);
        event_loop.exit();
    }
    fn redraw(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        if let Some(error) = self.audio.device_error() {
            return Err(error.into());
        }
        if stop_requested(&self.runtime, &self.options) || self.ticks >= self.options.ticks {
            event_loop.exit();
            return Ok(());
        }
        let Some(host) = self.surface.as_mut() else {
            return Ok(());
        };
        let now = Instant::now();
        if now < self.next_tick {
            return Ok(());
        }
        // One update per redraw, with bounded catch-up. No giant wall-time delta
        // enters gameplay after dragging/minimizing/debugger pauses.
        if now.duration_since(self.next_tick) > Duration::from_millis(250) {
            self.next_tick = now;
        }
        self.next_tick += Duration::from_secs_f64(FIXED_DELTA);
        if let Some(gamepads) = &mut self.gamepads {
            gamepads.sample(&mut self.raw);
        }
        let frame = self.runtime.extract_frame()?;
        let size = host.window.inner_size();
        let camera = CameraView::new(&frame.camera, size.width, size.height)?;
        self.raw.pointer = camera.logical_pointer(self.pointer);
        if self.mapper.bindings() != self.runtime.bindings() {
            let validation = self.mapper.rebind(self.runtime.bindings().clone(), false);
            ensure!(
                validation.errors.is_empty(),
                "runtime input rebind rejected: {:?}",
                validation.errors
            );
        }
        self.mapper.set_deadzone(
            self.runtime
                .app()
                .world()
                .resource::<InputSettings>()
                .deadzone,
        )?;
        let physical = self.mapper.sample(&self.raw);
        self.raw.wheel = [0.0; 2];
        let injected = self
            .options
            .inputs
            .get(&self.ticks)
            .cloned()
            .unwrap_or_default();
        let input = merge_input(&physical, &injected, &mut self.previous);
        self.runtime.tick(FIXED_DELTA as f32, input)?;
        self.ticks += 1;
        let frame = self.runtime.extract_frame()?;
        if !self.occluded
            && host
                .present(&frame, Some(self.runtime.app().render_world()))?
                .is_some()
        {
            self.presented_frames += 1;
        }
        if self.ticks >= self.options.ticks {
            event_loop.exit();
        }
        Ok(())
    }
}

impl ApplicationHandler for DesktopRunner {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.surface.is_some() {
            let host = self.surface.as_mut().unwrap();
            let size = host.window.inner_size();
            if let Err(error) = host.resize(size.width, size.height) {
                self.fail(event_loop, error);
            }
            return;
        }
        let result = (|| -> Result<()> {
            let frame = self.runtime.extract_frame()?;
            let [width, height] = frame.camera.logical_size;
            let window = Arc::new(
                event_loop
                    .create_window(
                        Window::default_attributes()
                            .with_title(self.runtime.project().name.clone())
                            .with_inner_size(LogicalSize::new(
                                width as f64 * 2.0,
                                height as f64 * 2.0,
                            ))
                            .with_visible(false),
                    )
                    .context("creating desktop window")?,
            );
            let requirements = self
                .runtime
                .app_mut()
                .world_mut()
                .remove_resource::<GpuRequirements>()
                .unwrap_or_default();
            let mut host = pollster::block_on(WindowSurface::with_requirements(
                window,
                RenderLimits::default(),
                requirements,
            ))?;
            let passes = self
                .runtime
                .app_mut()
                .world_mut()
                .remove_resource::<NativeRenderPasses>()
                .unwrap_or_default();
            host.renderer.set_passes(passes)?;
            // Failure to initialize the controller service is propagated. A
            // service with no connected gamepad is valid and still polls hotplug.
            self.gamepads = Some(Gamepads::new()?);
            host.present(&frame, Some(self.runtime.app().render_world()))?;
            host.window.set_visible(true);
            host.window.request_redraw();
            self.surface = Some(host);
            Ok(())
        })();
        if let Err(error) = result {
            self.fail(event_loop, error);
        }
    }
    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        self.raw.focused = false;
        self.raw.down.clear();
        self.mapper.sample(&self.raw);
        if let Some(host) = &mut self.surface
            && let Err(error) = host.resize(0, 0)
        {
            self.fail(event_loop, error);
        }
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if self.surface.as_ref().is_none_or(|s| s.window.id() != id) {
            return;
        }
        window_input(&mut self.raw, &event, &mut self.pointer);
        if matches!(event, WindowEvent::Focused(false)) {
            // Preserve focus loss even if focus returns before a fixed tick.
            // The mapper then suppresses keys held during reactivation.
            self.mapper.sample(&self.raw);
        }
        let result = match event {
            WindowEvent::Occluded(occluded) => {
                self.occluded = occluded;
                Ok(())
            }
            WindowEvent::CloseRequested => {
                event_loop.exit();
                Ok(())
            }
            WindowEvent::Resized(size) => self
                .surface
                .as_mut()
                .unwrap()
                .resize(size.width, size.height),
            WindowEvent::ScaleFactorChanged { .. } => {
                let host = self.surface.as_mut().unwrap();
                let size = host.window.inner_size();
                host.resize(size.width, size.height)
            }
            WindowEvent::RedrawRequested => self.redraw(event_loop),
            _ => Ok(()),
        };
        if let Err(error) = result {
            self.fail(event_loop, error);
        }
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if stop_requested(&self.runtime, &self.options) {
            event_loop.exit();
            return;
        }
        if Instant::now() >= self.next_tick {
            if self
                .surface
                .as_ref()
                .is_some_and(|host| host.is_suspended() || self.occluded)
            {
                if let Err(error) = self.redraw(event_loop) {
                    self.fail(event_loop, error);
                }
            } else if let Some(host) = &self.surface {
                host.window.request_redraw();
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_tick));
    }
}
