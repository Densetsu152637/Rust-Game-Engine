//! Separate-process acceptance driver; the same configure/scripts are loaded by
//! the native runtime compiler and desktop launcher.
use engine_audio::{AudioConfig, AudioEngine};
use engine_project::{ActionFrame, Project, Runtime};
use std::path::PathBuf;
fn run() -> engine_core::Result<serde_json::Value> {
    let args: Vec<_> = std::env::args().collect();
    engine_core::anyhow::ensure!(
        args.len() >= 4,
        "usage: lab-walkthrough new|restore male|female USER_DATA [--omit-confirm]"
    );
    let (mode, variant) = (&args[1], &args[2]);
    engine_core::anyhow::ensure!(
        matches!(mode.as_str(), "new" | "restore") && matches!(variant.as_str(), "male" | "female"),
        "invalid walkthrough mode/variant"
    );
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("project-test-{mode}-{variant}.json"));
    let mut runtime = Runtime::new(Project::load(manifest)?)?;
    runtime.set_user_data_root(PathBuf::from(&args[3]))?;
    desktop_2d_gameplay::configure(runtime.app_mut())?;
    let mut audio = AudioEngine::offline(AudioConfig::default())?;
    runtime.set_audio(audio.control())?;
    let ticks = if mode == "new" { 360 } else { 120 };
    for tick in 0..ticks {
        let mut input = ActionFrame::default();
        if mode == "new" {
            let actions: &[&str] = match tick {
                5 => &["move_right"],
                6 if !args.iter().any(|a| a == "--omit-confirm") => &["ui_confirm", "interact"],
                8 => &["ui_down"],
                9 => &["ui_confirm"],
                16 | 18 => &["ui_back"],
                _ => &[],
            };
            for action in actions {
                input.pressed.insert((*action).into());
                input.held.insert((*action).into());
                input.values.insert((*action).into(), 1.0);
            }
        }
        runtime.tick(1.0 / 60.0, input)?;
        audio.mix(&mut [0.0; 1600])?;
    }
    let marker = format!(
        "DESKTOP2D_{}PASS:{variant}",
        if mode == "new" {
            "WALKTHROUGH_"
        } else {
            "RESTART_"
        }
    );
    let snapshot = runtime.snapshot();
    engine_core::anyhow::ensure!(
        snapshot.logs.iter().any(|s| s == &marker),
        "walkthrough did not reach {marker}"
    );
    runtime.stop()?;
    engine_core::anyhow::ensure!(audio.voice_count() == 0, "scene teardown leaked voices");
    Ok(
        serde_json::json!({"ok":true,"snapshot":snapshot,"audioVoicesAfterStop":audio.voice_count()}),
    )
}
fn main() {
    match run() {
        Ok(value) => println!("{value}"),
        Err(error) => {
            println!(
                "{}",
                serde_json::json!({"ok":false,"error":format!("{error:#}")})
            );
            std::process::exit(1);
        }
    }
}
