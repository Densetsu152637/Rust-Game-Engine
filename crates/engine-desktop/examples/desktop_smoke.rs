//! Main-thread visible smoke. Arrow keys move the atlas sprite; exits after two
//! seconds (120 fixed ticks). `--headless` exercises identical game stepping.
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
    let report = run(
        runtime,
        RunOptions {
            headless: std::env::args().any(|arg| arg == "--headless"),
            ticks: 120,
            ..Default::default()
        },
    )?;
    println!(
        "desktop smoke complete: {} fixed ticks, {} presented frames",
        report.ticks, report.presented_frames
    );
    Ok(())
}
