//! End-to-end authored GPU particles: project JSON (a `ParticleEmitter`
//! component) -> `Runtime::extract_particles` -> `GpuRenderer::advance_particles`
//! once per scene tick -> `render_scene` -> HDR readback.
//! Run explicitly on a real adapter with --ignored --test-threads=1.
use anyhow::Result;
use engine_desktop::*;
use engine_project::{ActionFrame, Project, Runtime, particles::ParticleFrame};
use serde_json::json;
use std::fs;

/// Seconds per scene tick: exactly representable, so 4 particles per second
/// spawn exactly one particle per tick.
const DT: f32 = 0.25;

/// A stationary red disc of radius 0.5 m at the origin per particle, in a 4 m
/// orthographic view (16 px per metre, 64 x 64 pixels).
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let entities = json!([
        {"id":"camera","name":"Camera","components":{
            "Transform":{"position":[0,0,3]},
            "Camera2D":{"logicalSize":[64,64]},
            "Camera3D":{"projection":{"kind":"orthographic","height":4,"near":0.1,"far":10},
                "lighting":{"ambient":0.25,"exposure":1}}}},
        {"id":"sparks","name":"Sparks","components":{
            "Transform":{"position":[0,0,0]},
            "ParticleEmitter":{"capacity":4,"spawn_rate":4.0,"lifetime":[100.0,100.0],
                "speed":[0.0,0.0],"spread":0.0,"gravity":[0,0,0],"size":[0.5,0.5],
                "start_color":[1,0,0,1],"end_color":[1,0,0,1]}}}
    ]);
    fs::write(
        root.join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"particles","entities":entities}))
            .unwrap(),
    )
    .unwrap();
    fs::write(
        root.join("project.json"),
        serde_json::to_vec(&json!({"schema":1,"name":"authored-particles",
            "startup_scene":"scene.json","assets":[]}))
        .unwrap(),
    )
    .unwrap();
    dir
}

fn draw(
    renderer: &mut GpuRenderer,
    target: &OffscreenTarget,
    runtime: &Runtime,
) -> Result<render_pipeline::HdrImage> {
    renderer.render_scene(&runtime.extract_scene_frame()?, &target.view, None)?;
    renderer
        .pipeline()?
        .read_scene_hdr(render_pipeline::ViewId::MAIN)
}

/// One scene tick plus the host's once-per-tick particle step, as
/// `DesktopRunner` performs it.
fn tick(runtime: &mut Runtime, renderer: &mut GpuRenderer) -> Result<ParticleFrame> {
    runtime.tick(DT, ActionFrame::default())?;
    let frame = runtime.extract_particles()?;
    renderer.advance_particles(&frame)?;
    Ok(frame)
}

#[test]
#[ignore = "requires a real GPU adapter and HDR readback"]
fn authored_particles_simulate_once_per_scene_tick_and_draw_without_advancing() -> Result<()> {
    let dir = project();
    let mut runtime = Runtime::new(Project::load(dir.path().join("project.json"))?)?;
    runtime.start()?;
    let gpu = render_pipeline::GpuContext::headless_with_options(&WgpuOptions::default())?;
    let target = OffscreenTarget::new(&gpu, 64, 64)?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;

    // Before any scene tick nothing has spawned: the particle pass is off and
    // the frame is the particle-free baseline.
    let baseline = draw(&mut renderer, &target, &runtime)?;
    assert_eq!(
        renderer.advance_particles(&runtime.extract_particles()?)?,
        render_pipeline::ParticleTickReport {
            tick: 0,
            ..Default::default()
        },
        "a frame before the first tick simulates nothing"
    );
    assert_eq!(renderer.particle_report().unwrap().simulated_ticks, 0);

    // Three scene ticks spawn three particles on the origin: a red disc.
    let mut last = ParticleFrame::default();
    for _ in 0..3 {
        last = tick(&mut runtime, &mut renderer)?;
    }
    let report = renderer.particle_report().unwrap();
    assert_eq!(report.simulated_ticks, 3);
    assert_eq!(report.last_tick, Some(last.tick));
    assert_eq!(report.emitters.len(), 1);
    assert_eq!(report.emitters[0].id, "sparks");
    assert_eq!(report.emitters[0].spawn_attempts, 3);

    let image = draw(&mut renderer, &target, &runtime)?;
    let centre = image.pixel(32, 32);
    assert!(
        centre[0] > 0.99 && centre[1] < 0.01 && centre[2] < 0.01,
        "red disc at the centre: {centre:?} (baseline {:?})",
        baseline.pixel(32, 32)
    );
    assert_eq!(
        image.pixel(2, 2),
        baseline.pixel(2, 2),
        "outside the disc is untouched"
    );
    let changed = (0..64u32)
        .flat_map(|y| (0..64u32).map(move |x| (x, y)))
        .filter(|&(x, y)| image.pixel(x, y) != baseline.pixel(x, y))
        .collect::<Vec<_>>();
    assert!(!changed.is_empty());
    assert!(
        changed
            .iter()
            .all(|&(x, y)| (x as i64 - 32).pow(2) + (y as i64 - 32).pow(2) <= 10 * 10),
        "only the 8 px disc changes"
    );

    // Redrawing, and feeding the same tick again, never advances the clock.
    for _ in 0..3 {
        assert_eq!(draw(&mut renderer, &target, &runtime)?.pixels, image.pixels);
    }
    let repeat = renderer.advance_particles(&last)?;
    assert!(!repeat.advanced);
    let after = renderer.particle_report().unwrap();
    assert_eq!(after.simulated_ticks, 3);
    assert_eq!(after.repeated_ticks, 1);
    assert_eq!(after.emitters[0].spawn_attempts, 3);
    let view = renderer
        .pipeline()?
        .particle_view_report(render_pipeline::ViewId::MAIN)
        .unwrap();
    assert_eq!((view.emitters_drawn, view.instances), (1, 3));

    // Removing the emitter frees its pool and restores the baseline.
    let removed = ParticleFrame {
        tick: last.tick + 1,
        emitters: vec![],
        ..last.clone()
    };
    renderer.advance_particles(&removed)?;
    assert!(renderer.particle_report().unwrap().emitters.is_empty());
    assert!(renderer.pipeline()?.read_particles("sparks")?.is_none());
    let restored = draw(&mut renderer, &target, &runtime)?;
    assert_eq!(restored.pixels, baseline.pixels, "no emitters, baseline");
    Ok(())
}

#[test]
#[ignore = "requires a real GPU adapter and HDR readback"]
fn the_authored_pool_is_bounded_by_its_capacity() -> Result<()> {
    let dir = project();
    let mut runtime = Runtime::new(Project::load(dir.path().join("project.json"))?)?;
    runtime.start()?;
    let gpu = render_pipeline::GpuContext::headless_with_options(&WgpuOptions::default())?;
    let target = OffscreenTarget::new(&gpu, 64, 64)?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;
    // Capacity 4 and one spawn per tick: six ticks attempt six spawns, so two
    // find their ring slot still alive and are dropped.
    for _ in 0..6 {
        tick(&mut runtime, &mut renderer)?;
    }
    let readback = renderer.pipeline()?.read_particles("sparks")?.unwrap();
    assert_eq!(readback.counters.alive, 4);
    assert_eq!(readback.counters.spawned, 4);
    assert_eq!(readback.dropped_total(), 2);
    draw(&mut renderer, &target, &runtime)?;
    assert_eq!(renderer.particle_report().unwrap().simulated_ticks, 6);
    Ok(())
}
