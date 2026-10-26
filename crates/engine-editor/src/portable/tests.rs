use super::*;
struct Host;
impl PlayHost for Host {
    fn start(&mut self, _: engine_project::Project) -> Result<()> {
        Ok(())
    }
    fn stop(&mut self) -> Result<()> {
        Ok(())
    }
    fn poll(&mut self) -> Result<crate::PlayStatus> {
        Ok(crate::PlayStatus::Running)
    }
}
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("scene.json"),r#"{"schema":1,"scene_id":"scene","entities":[{"id":"object","name":"Object","components":{"Transform":{"position":[0,0,0]}}}]}"#).unwrap();
    std::fs::write(
        dir.path().join("project.json"),
        r#"{"schema":1,"name":"Editor","startup_scene":"scene.json","assets":[]}"#,
    )
    .unwrap();
    dir
}
fn pass(app: &mut PortableEditor, ctx: &egui::Context, mut events: Vec<egui::Event>) {
    use eframe::App;
    let modifiers = events
        .iter()
        .rev()
        .find_map(|event| match event {
            egui::Event::PointerButton { modifiers, .. } | egui::Event::Key { modifiers, .. } => {
                Some(*modifiers)
            }
            _ => None,
        })
        .unwrap_or_default();
    events.insert(0, egui::Event::ModifiersChanged(modifiers));
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 800.0),
        )),
        events,
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |ui| app.ui(ui, &mut eframe::Frame::_new_kittest()));
    // This input test has no egui GPU backend to consume texture updates.
    output.textures_delta.clear();
}
#[test]
fn real_egui_drag_commits_once_and_undo_restores_the_scene() {
    let dir = fixture();
    let mut controller = EditorController::new(Box::new(Host));
    controller
        .open(dir.path().join("project.json"), false)
        .unwrap();
    let mut app = PortableEditor::new(controller);
    app.renderer = Some(OffscreenSprites::headless(640, 360, RenderLimits::default()).unwrap());
    let ctx = egui::Context::default();
    pass(&mut app, &ctx, vec![]);
    pass(&mut app, &ctx, vec![]);
    assert!(app.image.is_some(), "{}", app.error);
    let center = app.viewport_rect.unwrap().center();
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    pass(
        &mut app,
        &ctx,
        vec![egui::Event::PointerMoved(center), button(center, true)],
    );
    pass(
        &mut app,
        &ctx,
        vec![egui::Event::PointerMoved(center + egui::vec2(10.0, 0.0))],
    );
    pass(
        &mut app,
        &ctx,
        vec![egui::Event::PointerMoved(center + egui::vec2(80.0, 0.0))],
    );
    assert_eq!(
        app.selected()
            .unwrap()
            .components
            .transform
            .unwrap()
            .position,
        [0.0; 3],
        "drag previews must not mutate authored state"
    );
    assert!(
        app.world_position(&app.selected().unwrap()).unwrap()[0] > 0.5,
        "the preview world must move during the held gesture"
    );
    pass(
        &mut app,
        &ctx,
        vec![button(center + egui::vec2(80.0, 0.0), false)],
    );
    assert!(
        app.selected()
            .unwrap()
            .components
            .transform
            .unwrap()
            .position[0]
            > 0.5,
        "{}",
        app.error
    );
    app.controller.undo().unwrap();
    assert_eq!(
        app.selected()
            .unwrap()
            .components
            .transform
            .unwrap()
            .position,
        [0.0; 3]
    );
    assert!(
        !app.controller.is_dirty(),
        "one gesture must produce one history entry"
    );
}

fn test_app(scene: &str) -> (tempfile::TempDir, PortableEditor, egui::Context) {
    let dir = fixture();
    std::fs::write(dir.path().join("scene.json"), scene).unwrap();
    std::fs::write(dir.path().join("triangle.json"),r#"{"vertices":[{"position":[-1,-1,0.5],"uv":[0,0],"color":[1,1,1,1]},{"position":[1,-1,0.5],"uv":[1,0],"color":[1,1,1,1]},{"position":[0,1,0.5],"uv":[0.5,1],"color":[1,1,1,1]}],"indices":[0,1,2]}"#).unwrap();
    std::fs::write(dir.path().join("project.json"),r#"{"schema":1,"name":"Editor","startup_scene":"scene.json","assets":[{"id":"triangle","kind":"mesh","path":"triangle.json"}]}"#).unwrap();
    let mut controller = EditorController::new(Box::new(Host));
    controller
        .open(dir.path().join("project.json"), false)
        .unwrap();
    let mut app = PortableEditor::new(controller);
    app.renderer = Some(OffscreenSprites::headless(640, 360, RenderLimits::default()).unwrap());
    let ctx = egui::Context::default();
    pass(&mut app, &ctx, vec![]);
    pass(&mut app, &ctx, vec![]);
    assert!(app.image.is_some(), "{}", app.error);
    (dir, app, ctx)
}
fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    }
}
fn drag_to(app: &mut PortableEditor, ctx: &egui::Context, from: egui::Pos2, to: egui::Pos2) {
    pass(
        app,
        ctx,
        vec![egui::Event::PointerMoved(from), button(from, true)],
    );
    pass(
        app,
        ctx,
        vec![egui::Event::PointerMoved(from.lerp(to, 0.25))],
    );
    pass(app, ctx, vec![egui::Event::PointerMoved(to)]);
}
const PAIR: &str = r#"{"schema":1,"scene_id":"scene","entities":[{"id":"a","name":"A","components":{"Transform":{"position":[-2,0,0]}}},{"id":"b","name":"B","components":{"Transform":{"position":[2,0,0]}}}]}"#;
#[test]
fn real_egui_multi_move_rotate_scale_each_commit_one_transaction() {
    use engine_editor_settings::GizmoMode;
    for mode in [GizmoMode::Translate, GizmoMode::Rotate, GizmoMode::Scale] {
        let (_dir, mut app, ctx) = test_app(PAIR);
        app.controller
            .select_many(vec!["a".into(), "b".into()])
            .unwrap();
        app.settings.gizmo.mode = mode;
        app.settings.snapping.enabled = true;
        let initial = app.controller.document().unwrap().snapshot().clone();
        let rect = app.viewport_rect.unwrap();
        let center = rect.center();
        let (from, to) = match mode {
            GizmoMode::Translate => (center, center + egui::vec2(63.0, -17.0)),
            GizmoMode::Rotate => (
                center + egui::vec2(76.5, 0.0),
                center + egui::vec2(0.0, -76.5),
            ),
            GizmoMode::Scale => (center, center + egui::vec2(100.0, 0.0)),
        };
        drag_to(&mut app, &ctx, from, to);
        assert!(
            matches!(app.interaction, Interaction::Gizmo(_)),
            "{mode:?}: {}",
            app.error
        );
        assert_eq!(*app.controller.document().unwrap().snapshot(), initial);
        pass(&mut app, &ctx, vec![button(to, false)]);
        let entities = &app.controller.document().unwrap().snapshot().entities;
        let a = entities[0].components.transform.as_ref().unwrap();
        let b = entities[1].components.transform.as_ref().unwrap();
        match mode {
            GizmoMode::Translate => {
                let dx = a.position[0] + 2.0;
                assert!(dx > 0.0 && (dx / 0.5 - (dx / 0.5).round()).abs() < 1e-5);
                assert!((b.position[0] - 2.0 - dx).abs() < 1e-5);
            }
            GizmoMode::Rotate => {
                assert!(a.position[0].abs() < 1e-4 && a.position[1] < -1.9);
                assert!(b.position[1] > 1.9);
            }
            GizmoMode::Scale => {
                assert!((a.scale[0] - 1.5).abs() < 1e-5);
                assert!((b.position[0] - 3.0).abs() < 1e-5);
            }
        }
        app.controller.undo().unwrap();
        assert_eq!(*app.controller.document().unwrap().snapshot(), initial);
        assert!(
            !app.controller.is_dirty(),
            "{mode:?} must have exactly one history entry"
        );
    }
}
#[test]
fn real_egui_escape_pointer_loss_and_stale_release_preserve_authored_state() {
    for outcome in 0..3 {
        let stale = outcome == 2;
        let (_dir, mut app, ctx) = test_app(PAIR);
        let from = app
            .view_math(app.viewport_rect.unwrap())
            .unwrap()
            .project(Vec3::new(-2.0, 0.0, 0.0))
            .unwrap();
        let to = from + egui::vec2(60.0, 0.0);
        drag_to(&mut app, &ctx, from, to);
        assert!(matches!(app.interaction, Interaction::Gizmo(_)));
        if stale {
            let mut entity = app.selected().unwrap();
            entity.name = "Changed externally".into();
            app.apply_entity(entity);
        } else {
            pass(
                &mut app,
                &ctx,
                if outcome == 1 {
                    vec![egui::Event::PointerGone]
                } else {
                    vec![egui::Event::Key {
                        key: egui::Key::Escape,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: Default::default(),
                    }]
                },
            );
        }
        pass(&mut app, &ctx, vec![button(to, false)]);
        assert_eq!(
            app.selected()
                .unwrap()
                .components
                .transform
                .unwrap()
                .position,
            [-2.0, 0.0, 0.0]
        );
        if stale {
            assert!(!app.error.is_empty());
            assert_eq!(app.selected().unwrap().name, "Changed externally");
        } else {
            assert!(!app.controller.is_dirty());
        }
    }
}

#[test]
fn real_egui_geometry_click_toggle_empty_and_marquee_selection() {
    let scene = PAIR
        .replace("[-2,0,0]", "[-4,0,0]")
        .replace("[2,0,0]", "[4,0,0]")
        .replace(
            "\"Transform\":",
            "\"MeshRenderer\":{\"mesh\":\"triangle\"},\"Transform\":",
        );
    let (_dir, mut app, ctx) = test_app(&scene);
    let rect = app.viewport_rect.unwrap();
    let view = app.view_math(rect).unwrap();
    let a = view.project(Vec3::new(-4.0, 0.0, 0.5)).unwrap();
    let b = view.project(Vec3::new(4.0, 0.0, 0.5)).unwrap();
    assert_eq!(
        app.pick(b, rect).as_deref(),
        Some("b"),
        "draw IDs must map to authored IDs"
    );
    pass(
        &mut app,
        &ctx,
        vec![egui::Event::PointerMoved(b), button(b, true)],
    );
    pass(&mut app, &ctx, vec![button(b, false)]);
    assert_eq!(app.controller.selection(), ["b"]);
    let modified = |pos, pressed| egui::Event::PointerButton {
        pos,
        pressed,
        button: egui::PointerButton::Primary,
        modifiers: egui::Modifiers {
            ctrl: true,
            ..Default::default()
        },
    };
    pass(
        &mut app,
        &ctx,
        vec![egui::Event::PointerMoved(a), modified(a, true)],
    );
    pass(&mut app, &ctx, vec![modified(a, false)]);
    assert_eq!(app.controller.selection(), ["b", "a"]);
    let empty = rect.center() + egui::vec2(0.0, 100.0);
    pass(
        &mut app,
        &ctx,
        vec![egui::Event::PointerMoved(empty), button(empty, true)],
    );
    pass(&mut app, &ctx, vec![button(empty, false)]);
    assert!(app.controller.selection().is_empty());
    // Clearing the inspector changes the side panel's minimum width. Draw the
    // marquee against the current displayed viewport after that layout settles.
    for _ in 0..4 {
        pass(&mut app, &ctx, vec![]);
    }
    let rect = app.viewport_rect.unwrap();
    let from = rect.center() - egui::vec2(150.0, 70.0);
    let to = rect.center() + egui::vec2(150.0, 70.0);
    assert_eq!(
        app.box_members(
            egui::Rect::from_two_pos(from, to),
            app.viewport_rect.unwrap()
        ),
        vec!["a", "b"],
        "before drag rect={:?}",
        app.viewport_rect
    );
    drag_to(&mut app, &ctx, from, to);
    assert_eq!(
        app.viewport_rect,
        Some(rect),
        "viewport layout must be settled before this selection test"
    );
    assert!(
        matches!(app.interaction, Interaction::Marquee(_)),
        "interaction did not start marquee; rect={:?}, error={}",
        app.viewport_rect,
        app.error
    );
    pass(&mut app, &ctx, vec![button(to, false)]);
    assert_eq!(
        app.controller.selection(),
        ["a", "b"],
        "rect={:?}, from={from:?}, to={to:?}, error={}",
        app.viewport_rect,
        app.error
    );
    assert!(
        !app.controller.is_dirty(),
        "selection must not enter scene history"
    );
}
#[test]
fn selected_parent_and_child_move_once_with_local_child_unchanged() {
    let scene = r#"{"schema":1,"scene_id":"scene","entities":[{"id":"parent","name":"Parent","components":{"Transform":{"position":[0,0,0],"rotation":[0,0,0.70710677,0.70710677],"scale":[2,2,2]}}},{"id":"child","name":"Child","parent":"parent","components":{"Transform":{"position":[2,0,0]}}}]}"#;
    let (_dir, mut app, ctx) = test_app(scene);
    app.controller
        .select_many(vec!["parent".into(), "child".into()])
        .unwrap();
    let (basis, starts) = app.gizmo_setup().unwrap();
    assert_eq!(starts.len(), 1);
    let from = app
        .view_math(app.viewport_rect.unwrap())
        .unwrap()
        .project(basis.pivot)
        .unwrap();
    let to = from + egui::vec2(60.0, 0.0);
    drag_to(&mut app, &ctx, from, to);
    pass(&mut app, &ctx, vec![button(to, false)]);
    let child = app.controller.document().unwrap().snapshot().entities[1]
        .components
        .transform
        .as_ref()
        .unwrap();
    assert_eq!(child.position, [2.0, 0.0, 0.0]);
    let parent = app.controller.document().unwrap().snapshot().entities[0]
        .components
        .transform
        .as_ref()
        .unwrap();
    assert!(parent.position[0] > 0.5);
    app.controller.undo().unwrap();
    assert!(!app.controller.is_dirty());
}
#[test]
fn navigation_maps_screen_motion_to_world_axes() {
    let view = ViewNavigation::default();
    assert_eq!(view.screen_delta([16.0, -16.0], 360.0), [1.0, 1.0, 0.0]);
    let view = ViewNavigation {
        three_dimensional: true,
        yaw: std::f32::consts::FRAC_PI_2,
        ..Default::default()
    };
    let movement = view.screen_delta([100.0, 0.0], 360.0);
    assert!(movement[0].abs() < 1e-5 && movement[2] < -1.0);
    let camera = CameraInstance3D {
        camera: Camera3D::default(),
        world: view.camera_world().to_cols_array_2d(),
    };
    let projected = engine_desktop::camera_matrix(&camera, 16.0 / 9.0).unwrap()
        * Vec3::from_array(view.center).extend(1.0);
    assert!(projected.x.abs() < 1e-5 && projected.y.abs() < 1e-5 && projected.w > 0.0);
}

#[test]
fn runtime_2d_depth_test_hides_far_geometry_from_click_and_marquee() {
    let scene = r#"{"schema":1,"scene_id":"scene","entities":[{"id":"a","name":"Near","components":{"Transform":{"position":[0,0,-0.3]},"MeshRenderer":{"mesh":"triangle"}}},{"id":"z","name":"Far","components":{"Transform":{"position":[0,0,0.3]},"MeshRenderer":{"mesh":"triangle"}}}]}"#;
    let (_dir, app, _ctx) = test_app(scene);
    let draws = &app.frame.as_ref().unwrap().sprites.draws;
    assert_eq!(draws.len(), 2);
    assert!(
        draws
            .iter()
            .all(|draw| draw.state.depth_test && draw.state.depth_write)
    );
    let rect = app.viewport_rect.unwrap();
    assert_eq!(app.pick(rect.center(), rect).as_deref(), Some("a"));
    assert_eq!(app.box_members(rect, rect), vec!["a"]);
}
#[test]
fn runtime_3d_coplanar_opaque_ids_match_renderer_less_equal_order() {
    for ids in [["a", "z"], ["z", "a"]] {
        let entities:Vec<_>=ids.into_iter().map(|id| serde_json::json!({"id":id,"name":id,"components":{"Transform":{"position":[0,0,0]},"MeshRenderer":{"mesh":"triangle"}}})).collect();
        let scene =
            serde_json::json!({"schema":1,"scene_id":"scene","entities":entities}).to_string();
        let (_dir, mut app, ctx) = test_app(&scene);
        app.navigation.three_dimensional = true;
        app.dirty_preview = true;
        pass(&mut app, &ctx, vec![]);
        assert_eq!(app.frame.as_ref().unwrap().meshes.len(), 2);
        let rect = app.viewport_rect.unwrap();
        assert_eq!(app.pick(rect.center(), rect).as_deref(), Some("z"));
        assert_eq!(app.box_members(rect, rect), vec!["z"]);
    }
}
