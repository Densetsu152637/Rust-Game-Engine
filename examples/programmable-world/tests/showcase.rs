use engine_project::{
    ActionFrame,
    ui_tree::{UiRoot, UiWidget},
};
use programmable_world::{DemoEvents, Score};

#[test]
fn authored_scene_runs_ui_callbacks_physics_animation_navigation_and_render_extraction() {
    let mut runtime = programmable_world::load().unwrap();
    runtime
        .tick(
            1.0 / 60.0,
            ActionFrame {
                pressed: ["ui_click".into()].into(),
                pointer: [60.0, 80.0],
                ..Default::default()
            },
        )
        .unwrap();
    let hud = runtime.entity("hud").unwrap();
    assert_eq!(
        runtime.app().world().get::<Score>(hud).unwrap().points,
        0,
        "UI callbacks dispatch on the next tick"
    );
    runtime.tick(1.0 / 60.0, ActionFrame::default()).unwrap();
    assert_eq!(runtime.app().world().get::<Score>(hud).unwrap().points, 1);
    let ui = runtime.app().world().get::<UiRoot>(hud).unwrap();
    assert!(
        ui.tree.nodes.iter().any(
            |n| matches!(&n.widget,UiWidget::Label{text} if n.id=="score" && text=="SCORE: 1")
        )
    );
    for _ in 0..28 {
        runtime.tick(1.0 / 60.0, ActionFrame::default()).unwrap();
    }
    assert!(
        runtime.position("falling").unwrap()[1] < 2.0,
        "gravity moved the dynamic body"
    );
    assert!(
        (runtime.position("animated").unwrap()[1] - 1.2).abs() < 0.02,
        "clip sampling moved the animated cube"
    );
    assert!(
        runtime.position("navigator").unwrap()[0] > 1.9,
        "waypoint follower advanced"
    );
    let scene = runtime.extract_scene_frame().unwrap();
    assert!(scene.camera3d.is_some());
    assert_eq!(scene.meshes.len(), 4);
    assert!(scene.meshes.iter().all(|m| m.mesh.indices.len() == 36));
    assert!(
        scene
            .sprites
            .draws
            .iter()
            .any(|d| d.id.starts_with("game-ui:") && d.texture.is_some())
    );
    assert!(
        !scene
            .sprites
            .draws
            .iter()
            .any(|d| d.id.starts_with("mesh:")),
        "3D meshes must not also be drawn in the 2D pass"
    );
    for _ in 0..211 {
        runtime.tick(1.0 / 60.0, ActionFrame::default()).unwrap();
    }
    let events = runtime.app().world().resource::<DemoEvents>();
    assert!(events.animation_markers >= 2);
    assert_eq!(events.navigation_arrivals, 1);
    assert!(events.physics_contacts >= 1);
    let snapshot = runtime.try_snapshot().unwrap();
    let hud = snapshot
        .scene
        .entities
        .iter()
        .find(|e| e.id == "hud")
        .unwrap();
    assert_eq!(hud.components.custom["game.Score"].data["points"], 1);
    assert_eq!(runtime.position("navigator").unwrap(), [2.5, 0.4, 1.0]);
    runtime.stop().unwrap();
}
