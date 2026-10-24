//! A reproducible authored 3D scene, retained HUD, and native callbacks.
use engine_core::{bevy_ecs, prelude::*};
use engine_project::{
    EngineEvent, Runtime,
    ui_tree::{UiRoot, UiWidget},
};
use serde::{Deserialize, Serialize};

#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Score {
    pub points: u32,
}

#[derive(Resource, Default, Debug)]
pub struct DemoEvents {
    pub animation_markers: usize,
    pub navigation_arrivals: usize,
    pub physics_contacts: usize,
}

/// The native compiler calls the same entrypoint used by the executable/tests.
pub fn configure(app: &mut EngineApp) -> Result<()> {
    engine_project::register_component::<Score>(app, "game.Score", 1)?;
    engine_project::register_event_handler(app, "add_score", add_score)?;
    engine_project::register_event_handler(app, "observe_demo", observe_demo)?;
    app.world_mut().insert_resource(DemoEvents::default());
    Ok(())
}

fn add_score(world: &mut World, owner: Entity, event: &EngineEvent) -> engine_project::Result<()> {
    if event.name != "ui.clicked" || event.payload["action"] != "add_score" {
        return Ok(());
    }
    let points = {
        let mut score = world.get_mut::<Score>(owner).ok_or_else(|| {
            engine_project::Error::new("demo.score.missing", "HUD requires game.Score")
        })?;
        score.points = score.points.saturating_add(1).min(100);
        score.points
    };
    let mut ui = world.get_mut::<UiRoot>(owner).ok_or_else(|| {
        engine_project::Error::new("demo.ui.missing", "HUD requires engine.UiRoot")
    })?;
    for node in &mut ui.tree.nodes {
        match (&*node.id, &mut node.widget) {
            ("score", UiWidget::Label { text }) => *text = format!("SCORE: {points}"),
            ("score-meter", UiWidget::Progress { value, .. }) => *value = points as f32,
            _ => {}
        }
    }
    Ok(())
}

fn observe_demo(world: &mut World, _: Entity, event: &EngineEvent) -> engine_project::Result<()> {
    let mut events = world.resource_mut::<DemoEvents>();
    match event.name.as_str() {
        "animation.event" => events.animation_markers += 1,
        "navigation.arrived" => events.navigation_arrivals += 1,
        "physics.contact.start" => events.physics_contacts += 1,
        _ => {}
    }
    Ok(())
}

pub fn load() -> Result<Runtime> {
    let project = engine_project::Project::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("project.json"),
    )?;
    let mut runtime = Runtime::new(project)?;
    configure(runtime.app_mut())?;
    Ok(runtime)
}

/// A GPU-free run with the same real input/event/simulation pipeline as the window.
pub fn run_headless() -> Result<()> {
    let mut runtime = load()?;
    runtime.tick(
        1.0 / 60.0,
        engine_project::ActionFrame {
            pressed: ["ui_click".into()].into(),
            pointer: [60.0, 80.0],
            ..Default::default()
        },
    )?;
    for _ in 0..240 {
        runtime.tick(1.0 / 60.0, engine_project::ActionFrame::default())?;
    }
    let hud = runtime
        .entity("hud")
        .ok_or_else(|| engine_core::anyhow::anyhow!("Missing HUD"))?;
    let score = runtime
        .app()
        .world()
        .get::<Score>(hud)
        .ok_or_else(|| engine_core::anyhow::anyhow!("Missing Score"))?;
    let scene = runtime.extract_scene_frame()?;
    println!(
        "Score: {}; 3D meshes: {}; HUD draws: {}; events: {:?}",
        score.points,
        scene.meshes.len(),
        scene.sprites.draws.len(),
        runtime.app().world().resource::<DemoEvents>()
    );
    runtime.stop()?;
    Ok(())
}
