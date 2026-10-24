use crate::{
    BACKEND_ENV_VAR, CameraView, FrameHistory, FramePacer, GpuRequirements, NativeRenderPasses,
    RenderLimits, WgpuBackends, WgpuOptions, WindowSurface,
    input::{Gamepads, PressLatch, sample_tick, window_input},
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
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

pub const FIXED_DELTA: f64 = 1.0 / 60.0;

/// Shortest wait, in seconds, between simulation steps while nothing is shown.
const MIN_IDLE_WAIT: f64 = 0.001;

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
    pub profile: engine_core::ProfileSnapshot,
    pub ticks: u64,
    pub fixed_delta: f64,
    pub scene: RuntimeSnapshot,
    pub presented_frames: u64,
}

/// Pick the wgpu graphics API for a run. An explicit `option` wins and the
/// environment is ignored; otherwise `env_value` (the content of
/// `ENGINE_RENDER_BACKEND`) selects it, and unset or blank means
/// [`WgpuBackends::Primary`]. Unknown values are an error listing the accepted ones.
pub fn resolve_wgpu_backends(
    option: Option<WgpuBackends>,
    env_value: Option<&str>,
) -> Result<WgpuBackends> {
    if let Some(backends) = option {
        return Ok(backends);
    }
    Ok(WgpuBackends::from_env_value(env_value)?.unwrap_or_default())
}

/// [`resolve_wgpu_backends`] reading `ENGINE_RENDER_BACKEND` from the process
/// environment. Every consumer of the selection (visible runs, headless runs,
/// offscreen screenshots) goes through this so a typo fails the same way.
pub fn resolve_backends_from_environment(option: Option<WgpuBackends>) -> Result<WgpuBackends> {
    match std::env::var(BACKEND_ENV_VAR) {
        Ok(value) => resolve_wgpu_backends(option, Some(&value)),
        Err(std::env::VarError::NotPresent) => resolve_wgpu_backends(option, None),
        Err(std::env::VarError::NotUnicode(_)) if option.is_some() => {
            resolve_wgpu_backends(option, None)
        }
        Err(std::env::VarError::NotUnicode(_)) => {
            anyhow::bail!("{BACKEND_ENV_VAR} is not valid unicode")
        }
    }
}

/// Run a native generation. Headless mode never initializes winit, GPU, gamepad
/// or audio devices. Native mode must be called from the process main thread.
///
/// The wgpu graphics API comes from `ENGINE_RENDER_BACKEND`
/// (`primary|vulkan|dx12|metal|gl|all`, default primary); see
/// [`run_with_wgpu_backends`] to choose it explicitly.
pub fn run(runtime: Runtime, options: RunOptions) -> Result<RunReport> {
    run_with_wgpu_backends(runtime, options, None)
}

/// [`run`] with an explicit wgpu graphics API. `None` resolves the
/// `ENGINE_RENDER_BACKEND` environment variable (invalid values are an error,
/// even for headless runs, so typos surface). `Some(backends)` ignores the
/// environment. Headless runs never create a device.
pub fn run_with_wgpu_backends(
    mut runtime: Runtime,
    options: RunOptions,
    backends: Option<WgpuBackends>,
) -> Result<RunReport> {
    let backends = resolve_backends_from_environment(backends)?;
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
        backends,
        surface: None,
        mapper,
        raw: RawInput::default(),
        pointer: [0.0; 2],
        gamepads: None,
        occluded: false,
        present_skipped: false,
        previous: BTreeSet::new(),
        latch: PressLatch::default(),
        pacer: FramePacer::new(FIXED_DELTA),
        history: FrameHistory::default(),
        last_frame: Instant::now(),
        ticks: 0,
        presented_frames: 0,
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
        profile: app.runtime.app().profile(),
        ticks: app.ticks,
        fixed_delta: FIXED_DELTA,
        scene: app.runtime.try_snapshot()?,
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
        profile: runtime.app().profile(),
        ticks,
        fixed_delta: FIXED_DELTA,
        scene: runtime.try_snapshot()?,
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
    backends: WgpuBackends,
    surface: Option<WindowSurface>,
    mapper: ActionMapper,
    raw: RawInput,
    pointer: [f32; 2],
    gamepads: Option<Gamepads>,
    occluded: bool,
    /// The last present produced no image (surface occluded, lost, timed out
    /// or zero-sized), so redraws must not be requested back to back.
    present_skipped: bool,
    previous: BTreeSet<String>,
    latch: PressLatch,
    pacer: FramePacer,
    /// The last two simulation frames, for presenting between them.
    history: FrameHistory,
    last_frame: Instant,
    ticks: u64,
    presented_frames: u64,
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
        let Some(size) = self.surface.as_ref().map(|host| host.window.inner_size()) else {
            return Ok(());
        };
        // Bank the real time since the last redraw and run the fixed ticks it
        // buys: none on a fast display, several after a slow frame, never more
        // than the pacer's cap (a stall does not fast-forward the game).
        let now = Instant::now();
        let step = self
            .pacer
            .advance(now.duration_since(self.last_frame).as_secs_f64());
        self.last_frame = now;
        let run = u64::from(step.ticks).min(self.options.ticks - self.ticks);
        for index in 0..run {
            if stop_requested(&self.runtime, &self.options) {
                event_loop.exit();
                return Ok(());
            }
            self.tick(size.width, size.height)?;
            // Only the last two ticks of a frame can be presented, so only
            // they are extracted.
            if index + 2 == run {
                self.record_frame()?;
            }
        }
        if run > 0 {
            self.record_frame()?;
        }
        let finished = self.ticks >= self.options.ticks;
        // The final frame shows the final tick, not a blend toward it.
        let alpha = if finished { 1.0 } else { step.alpha };
        if !self.occluded
            && let (Some(host), Some(frame)) = (self.surface.as_mut(), self.history.frame_at(alpha))
        {
            let mut scene = self.runtime.extract_scene_frame()?;
            let mut overlay = frame.into_owned();
            if scene.camera3d.is_some() {
                let meshes: BTreeSet<_> = scene
                    .meshes
                    .iter()
                    .map(|draw| format!("mesh:{}", draw.id))
                    .collect();
                overlay.draws.retain(|draw| !meshes.contains(&draw.id));
            }
            scene.sprites = overlay;
            let shown = host
                .present_scene(&scene, Some(self.runtime.app().render_world()))?
                .is_some();
            self.present_skipped = !shown;
            if shown {
                self.presented_frames += 1;
            }
        }
        if finished {
            event_loop.exit();
        }
        Ok(())
    }

    /// Extract the frame for the tick just run into the presentation history.
    fn record_frame(&mut self) -> Result<()> {
        let frame = self.runtime.extract_scene_frame()?.sprites;
        self.history.push(frame, self.runtime.scene_revision());
        Ok(())
    }

    /// Sample input and run one fixed simulation tick.
    fn tick(&mut self, width: u32, height: u32) -> Result<()> {
        if let Some(gamepads) = &mut self.gamepads {
            gamepads.sample(&mut self.raw);
        }
        let camera = CameraView::new(&self.runtime.extract_camera(), width, height)?;
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
        let no_input = BTreeSet::new();
        let injected = self.options.inputs.get(&self.ticks).unwrap_or(&no_input);
        let input = sample_tick(
            &mut self.mapper,
            &mut self.raw,
            &mut self.latch,
            injected,
            &mut self.previous,
        );
        self.runtime.tick(FIXED_DELTA as f32, input)?;
        self.ticks += 1;
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
            // Time spent suspended is not simulated.
            self.present_skipped = false;
            self.pacer.reset();
            self.last_frame = Instant::now();
            return;
        }
        let result = (|| -> Result<()> {
            let frame = self.runtime.extract_scene_frame()?;
            let [width, height] = frame.sprites.camera.logical_size;
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
            let mut host = pollster::block_on(WindowSurface::with_options(
                window,
                RenderLimits::default(),
                WgpuOptions {
                    backends: self.backends,
                    requirements,
                    ..Default::default()
                },
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
            host.present_scene(&frame, Some(self.runtime.app().render_world()))?;
            host.window.set_visible(true);
            host.window.request_redraw();
            self.surface = Some(host);
            // Tick zero is the first frame to blend from. Startup time (device
            // creation, shader compilation) is not simulated.
            self.history
                .push(frame.sprites, self.runtime.scene_revision());
            self.pacer.reset();
            self.last_frame = Instant::now();
            Ok(())
        })();
        if let Err(error) = result {
            self.fail(event_loop, error);
        }
    }
    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        self.raw.focused = false;
        self.raw.down.clear();
        self.latch.clear();
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
        // A press that ends before the next tick must still reach it.
        let before = matches!(
            event,
            WindowEvent::KeyboardInput {
                event: winit::event::KeyEvent {
                    state: ElementState::Pressed,
                    ..
                },
                ..
            } | WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        )
        .then(|| self.raw.down.clone());
        window_input(&mut self.raw, &event, &mut self.pointer);
        if let Some(before) = before {
            self.latch.note_presses(&before, &self.raw.down);
        }
        if matches!(event, WindowEvent::Focused(false)) {
            // Preserve focus loss even if focus returns before a fixed tick.
            // The mapper then suppresses keys held during reactivation.
            self.latch.clear();
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
            WindowEvent::Resized(size) => {
                self.present_skipped = false;
                self.surface
                    .as_mut()
                    .unwrap()
                    .resize(size.width, size.height)
            }
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
        let Some(host) = &self.surface else {
            return;
        };
        if !host.is_suspended() && !self.occluded && !self.present_skipped {
            // Present as fast as the display takes frames: every redraw
            // acquires a swapchain image, which blocks on the present mode
            // (vsync by default), so this follows the refresh rate rather than
            // spinning.
            host.window.request_redraw();
            event_loop.set_control_flow(ControlFlow::Poll);
            return;
        }
        // Nothing can be shown (minimized, occluded, no image to acquire):
        // keep the simulation ticking in real time without redraws and wait for
        // the next tick instead of spinning.
        let due = self.last_frame
            + Duration::from_secs_f64(self.pacer.until_next_tick().max(MIN_IDLE_WAIT));
        if Instant::now() >= due
            && let Err(error) = self.redraw(event_loop)
        {
            self.fail(event_loop, error);
            return;
        }
        let due = self.last_frame
            + Duration::from_secs_f64(self.pacer.until_next_tick().max(MIN_IDLE_WAIT));
        event_loop.set_control_flow(ControlFlow::WaitUntil(due));
    }
}

#[cfg(test)]
mod run_tests {
    use super::*;
    use anyhow::anyhow;

    #[test]
    fn combine_passes_through_two_successes() {
        assert!(combine(Ok(()), Ok(())).is_ok());
    }

    #[test]
    fn combine_surfaces_a_cleanup_failure_after_success() {
        let error = combine(Ok(()), Err(anyhow!("cleanup broke"))).unwrap_err();
        assert_eq!(error.to_string(), "cleanup broke");
    }

    #[test]
    fn combine_keeps_the_primary_error_when_cleanup_succeeds() {
        let error = combine(Err(anyhow!("tick failed")), Ok(())).unwrap_err();
        assert_eq!(error.to_string(), "tick failed");
    }

    #[test]
    fn combine_reports_both_failures_with_the_primary_first() {
        let error =
            combine(Err(anyhow!("tick failed")), Err(anyhow!("gpu idle failed"))).unwrap_err();
        let text = format!("{error:#}");
        assert!(
            text.starts_with("cleanup also failed: gpu idle failed"),
            "{text}"
        );
        assert!(text.ends_with("tick failed"), "{text}");
    }

    #[test]
    fn fixed_delta_is_one_sixtieth_of_a_second() {
        assert_eq!(FIXED_DELTA, 1.0 / 60.0);
        assert!((FIXED_DELTA * 60.0 - 1.0).abs() < 1e-12);
    }

    #[test]
    fn default_options_describe_a_native_open_ended_run() {
        let options = RunOptions::default();
        assert!(!options.headless);
        assert_eq!(options.ticks, 1_000_000);
        assert!(options.inputs.is_empty());
        assert!(options.user_data.is_none());
        assert!(options.shutdown.is_none());
    }

    #[test]
    fn explicit_backends_win_over_the_environment() {
        assert_eq!(
            resolve_wgpu_backends(Some(WgpuBackends::Dx12), Some("vulkan")).unwrap(),
            WgpuBackends::Dx12
        );
        assert_eq!(
            resolve_wgpu_backends(Some(WgpuBackends::Gl), Some("garbage")).unwrap(),
            WgpuBackends::Gl
        );
    }

    #[test]
    fn environment_selects_the_graphics_api_case_insensitively() {
        for (value, expected) in [
            ("vulkan", WgpuBackends::Vulkan),
            (" DX12 ", WgpuBackends::Dx12),
            ("Metal", WgpuBackends::Metal),
            ("gl", WgpuBackends::Gl),
            ("all", WgpuBackends::All),
            ("wgpu", WgpuBackends::Primary),
            ("auto", WgpuBackends::Primary),
        ] {
            assert_eq!(
                resolve_wgpu_backends(None, Some(value)).unwrap(),
                expected,
                "{value}"
            );
        }
    }

    #[test]
    fn unset_or_blank_environment_defaults_to_primary() {
        assert_eq!(
            resolve_wgpu_backends(None, None).unwrap(),
            WgpuBackends::Primary
        );
        assert_eq!(
            resolve_wgpu_backends(None, Some("  ")).unwrap(),
            WgpuBackends::Primary
        );
    }

    #[test]
    fn unknown_values_name_the_variable_and_the_accepted_apis() {
        let error = resolve_wgpu_backends(None, Some("glide"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("ENGINE_RENDER_BACKEND"), "{error}");
        assert!(
            error.contains("primary, vulkan, dx12, metal, gl or all"),
            "{error}"
        );
    }
}
