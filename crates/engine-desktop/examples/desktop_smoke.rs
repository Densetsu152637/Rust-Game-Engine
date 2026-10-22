//! Main-thread visible smoke. Arrow keys move the atlas sprite; exits after two
//! seconds (120 fixed ticks, or `--ticks N`) and prints the tick and presented
//! frame rates. `--headless` exercises identical game stepping.
//! `ENGINE_PRESENT_MODE=immediate` presents faster than the display refresh.
use engine_core::{Stage, Time, bevy_ecs::prelude::*};
use engine_desktop::{RunOptions, run};
use engine_project::{ActionFrame, Project, Runtime, SpriteRenderer, Transform};

fn movement(
    input: Res<ActionFrame>,
    time: Res<Time>,
    mut query: Query<&mut Transform, With<SpriteRenderer>>,
) {
    let x = u8::from(input.held("right")) as f32 - u8::from(input.held("left")) as f32;
    let y = u8::from(input.held("up")) as f32 - u8::from(input.held("down")) as f32;
    for mut transform in &mut query {
        transform.position[0] += x * time.dt * 3.0;
        transform.position[1] += y * time.dt * 3.0;
    }
}

fn main() -> anyhow::Result<()> {
    let project = Project::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/assets/project.json"),
    )?;
    let mut runtime = Runtime::new(project)?;
    runtime
        .app_mut()
        .schedule_mut(Stage::Update)
        .add_systems(movement);
    let arguments: Vec<String> = std::env::args().collect();
    let ticks = arguments
        .iter()
        .position(|arg| arg == "--ticks")
        .and_then(|at| arguments.get(at + 1))
        .and_then(|value| value.parse().ok())
        .unwrap_or(120);
    let started = std::time::Instant::now();
    let report = run(
        runtime,
        RunOptions {
            headless: arguments.iter().any(|arg| arg == "--headless"),
            ticks,
            ..Default::default()
        },
    )?;
    let seconds = started.elapsed().as_secs_f64();
    println!(
        "desktop smoke complete: {} fixed ticks ({:.1}/s), {} presented frames ({:.1} fps) in {seconds:.2} s",
        report.ticks,
        report.ticks as f64 / seconds,
        report.presented_frames,
        report.presented_frames as f64 / seconds,
    );
    Ok(())
}
