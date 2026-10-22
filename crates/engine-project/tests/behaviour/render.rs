//! CPU render extraction: cameras, sprites, animation frames, snapping, mesh
//! renderers, UI draws, ordering and geometry caching.
use super::common::*;
use engine_project::{
    ActionFrame, Transform,
    runtime::{BlendMode, DrawSpace, RenderFrame, Runtime, SpriteFrame},
    ui::{PanelOptions, Rect, UiModel},
};
use serde_json::{Value, json};
use std::sync::Arc;

fn sprite(id: &str, x: f32, y: f32, extra: Value) -> Value {
    let mut sprite = json!({"texture": "texture", "size": [1, 1]});
    for (k, v) in extra.as_object().unwrap() {
        sprite[k] = v.clone();
    }
    ent(
        id,
        json!({"Transform": {"position": [x, y, 0]}, "SpriteRenderer": sprite}),
    )
}

fn sheet_player(extra: Value) -> Proj {
    let mut p = Proj::new();
    let mut sprite = json!({"texture": "sheet", "size": [1, 1]});
    for (k, v) in extra.as_object().unwrap() {
        sprite[k] = v.clone();
    }
    p.set_player(json!({"Transform": {"position": [0, 0, 0]}, "SpriteRenderer": sprite}));
    p
}

/// Keep only a free-standing camera so a test can lay out its own sprites.
fn clear_room(p: &mut Proj) {
    p.scene["entities"]
        .as_array_mut()
        .unwrap()
        .retain(|e| e["id"] == "camera");
    p.entity("camera")["components"]["Camera2D"] = json!({});
}

fn frame_of(rt: &Runtime) -> RenderFrame {
    rt.extract_frame().unwrap()
}

fn draw_ids(frame: &RenderFrame) -> Vec<String> {
    frame.draws.iter().map(|d| d.id.clone()).collect()
}

fn corner(frame: &RenderFrame, draw: &str, vertex: usize) -> [f32; 3] {
    frame
        .draws
        .iter()
        .find(|d| d.id == draw)
        .unwrap()
        .mesh
        .vertices[vertex]
        .position
}

fn uv(frame: &RenderFrame, draw: &str, vertex: usize) -> [f32; 2] {
    frame
        .draws
        .iter()
        .find(|d| d.id == draw)
        .unwrap()
        .mesh
        .vertices[vertex]
        .uv
}

fn tick(rt: &mut Runtime, dt: f32) {
    rt.tick(dt, ActionFrame::default()).unwrap();
}

// ---- sprites ------------------------------------------------------------------------------------

#[test]
fn a_sprite_entity_produces_one_world_draw_with_straight_alpha() {
    let rt = Proj::new().runtime();
    let frame = frame_of(&rt);
    assert_eq!(draw_ids(&frame), vec!["sprite:player"]);
    let draw = &frame.draws[0];
    assert_eq!(draw.space, DrawSpace::World);
    assert_eq!(draw.state.blend, BlendMode::StraightAlpha);
    assert!(!draw.state.depth_test && !draw.state.depth_write);
    assert!(draw.clip.is_none() && draw.texture.is_some());
}

#[test]
fn sprite_geometry_is_centred_on_the_transform_by_default() {
    let mut p = Proj::new();
    p.entity("player")["components"]["Transform"]["position"] = json!([3, 2, 0]);
    let frame = frame_of(&p.runtime());
    assert_eq!(corner(&frame, "sprite:player", 0), [2.5, 1.5, 0.0]);
    assert_eq!(corner(&frame, "sprite:player", 2), [3.5, 2.5, 0.0]);
}

#[test]
fn sprite_pivot_and_size_shape_the_quad() {
    let mut p = Proj::new();
    p.set_player(json!({"Transform": {"position": [1, 1, 0]}, "SpriteRenderer": {"texture": "texture", "size": [4, 2], "pivot": [0, 0]}}));
    let frame = frame_of(&p.runtime());
    assert_eq!(corner(&frame, "sprite:player", 0), [1.0, 1.0, 0.0]);
    assert_eq!(corner(&frame, "sprite:player", 2), [5.0, 3.0, 0.0]);
}

#[test]
fn sprite_depth_comes_from_the_transform() {
    let mut p = Proj::new();
    p.entity("player")["components"]["Transform"]["position"] = json!([0, 0, 7]);
    assert_eq!(corner(&frame_of(&p.runtime()), "sprite:player", 1)[2], 7.0);
}

#[test]
fn sprite_tint_reaches_the_vertices() {
    let mut p = Proj::new();
    p.entity("player")["components"]["SpriteRenderer"]["tint"] = json!([0.5, 0.25, 1, 0.75]);
    let frame = frame_of(&p.runtime());
    assert_eq!(
        frame.draws[0].mesh.vertices[3].color,
        [0.5, 0.25, 1.0, 0.75]
    );
}

#[test]
fn hidden_sprites_are_not_drawn() {
    let mut p = Proj::new();
    p.entity("player")["components"]["SpriteRenderer"]["visible"] = json!(false);
    assert!(frame_of(&p.runtime()).draws.is_empty());
}

#[test]
fn sprites_can_be_hidden_and_shown_again_at_runtime() {
    let mut rt = Proj::new().runtime();
    let e = rt.entity("player").unwrap();
    rt.app_mut()
        .world_mut()
        .get_mut::<engine_project::SpriteRenderer>(e)
        .unwrap()
        .visible = false;
    assert!(frame_of(&rt).draws.is_empty());
    rt.app_mut()
        .world_mut()
        .get_mut::<engine_project::SpriteRenderer>(e)
        .unwrap()
        .visible = true;
    assert_eq!(frame_of(&rt).draws.len(), 1);
}

#[test]
fn entities_without_a_transform_never_draw() {
    let mut p = Proj::new();
    p.add_entity(ent("bare", json!({"Script": {"asset": "player"}})));
    assert_eq!(frame_of(&p.runtime()).draws.len(), 1);
}

#[test]
fn source_rectangles_select_the_atlas_region() {
    let p = sheet_player(json!({"source": [1, 0, 2, 2]}));
    let frame = frame_of(&p.runtime());
    assert_eq!(uv(&frame, "sprite:player", 0), [0.25, 1.0]);
    assert_eq!(uv(&frame, "sprite:player", 2), [0.75, 0.0]);
}

#[test]
fn sprites_without_a_source_use_the_whole_texture() {
    let frame = frame_of(&sheet_player(json!({})).runtime());
    assert_eq!(uv(&frame, "sprite:player", 0), [0.0, 1.0]);
    assert_eq!(uv(&frame, "sprite:player", 2), [1.0, 0.0]);
}

#[test]
fn a_source_rectangle_outside_the_texture_fails_extraction() {
    let p = sheet_player(json!({"source": [3, 0, 2, 2]}));
    expect_code(p.runtime().extract_frame(), "asset.sprite.rectangle");
}

#[test]
fn sprites_sharing_a_texture_share_one_allocation() {
    let mut p = Proj::new();
    p.add_entity(sprite("second", 3.0, 0.0, json!({})));
    let frame = frame_of(&p.runtime());
    assert!(Arc::ptr_eq(
        frame.draws[0].texture.as_ref().unwrap(),
        frame.draws[1].texture.as_ref().unwrap()
    ));
}

#[test]
fn unknown_sprite_textures_fail_extraction_with_the_texture_id() {
    let mut rt = Proj::new().runtime();
    let e = rt.entity("player").unwrap();
    rt.app_mut()
        .world_mut()
        .get_mut::<engine_project::SpriteRenderer>(e)
        .unwrap()
        .texture = "ghost".into();
    let error = rt.extract_frame().unwrap_err();
    assert_eq!(error.diagnostics[0].code, "render.texture.unknown");
    assert!(error.diagnostics[0].message.contains("ghost"));
}

// ---- animation ---------------------------------------------------------------------------------------

const FOUR_FRAMES: [[u32; 4]; 4] = [[0, 0, 1, 1], [1, 0, 1, 1], [2, 0, 1, 1], [3, 0, 1, 1]];

fn animated(looping: bool) -> Runtime {
    sheet_player(json!({"frames": FOUR_FRAMES, "framesPerSecond": 4, "loop": looping})).runtime()
}

fn first_u(rt: &Runtime) -> f32 {
    uv(&frame_of(rt), "sprite:player", 0)[0]
}

#[test]
fn animation_starts_on_the_first_frame() {
    assert_eq!(first_u(&animated(true)), 0.0);
}

#[test]
fn animation_advances_one_frame_per_quarter_second_tick() {
    let mut rt = animated(true);
    let mut seen = Vec::new();
    for _ in 0..3 {
        tick(&mut rt, 0.25);
        seen.push(first_u(&rt));
    }
    assert_eq!(seen, vec![0.25, 0.5, 0.75]);
}

#[test]
fn looping_animation_wraps_around() {
    let mut rt = animated(true);
    for _ in 0..5 {
        tick(&mut rt, 0.25);
    }
    assert_eq!(first_u(&rt), 0.25);
}

#[test]
fn one_shot_animation_holds_the_last_frame() {
    let mut rt = animated(false);
    for _ in 0..9 {
        tick(&mut rt, 0.25);
    }
    assert_eq!(first_u(&rt), 0.75);
}

#[test]
fn animation_time_is_the_sum_of_simulated_deltas() {
    let mut a = animated(true);
    let mut b = animated(true);
    for _ in 0..8 {
        tick(&mut a, 0.125);
    }
    tick(&mut b, 1.0);
    assert_eq!(first_u(&a), first_u(&b));
}

#[test]
fn sprite_frame_component_overrides_the_clock() {
    let mut rt = animated(true);
    let e = rt.entity("player").unwrap();
    rt.app_mut()
        .world_mut()
        .entity_mut(e)
        .insert(SpriteFrame(2));
    tick(&mut rt, 0.25);
    assert_eq!(first_u(&rt), 0.5);
}

#[test]
fn sprite_frame_beyond_the_list_clamps_to_the_last_frame() {
    let mut rt = animated(true);
    let e = rt.entity("player").unwrap();
    rt.app_mut()
        .world_mut()
        .entity_mut(e)
        .insert(SpriteFrame(99));
    assert_eq!(first_u(&rt), 0.75);
}

#[test]
fn zero_fps_animation_stays_on_the_first_frame() {
    let mut rt = sheet_player(json!({"frames": FOUR_FRAMES, "framesPerSecond": 0})).runtime();
    tick(&mut rt, 5.0);
    assert_eq!(first_u(&rt), 0.0);
}

// ---- cameras and snapping ---------------------------------------------------------------------------------

#[test]
fn default_camera_is_320_by_180_at_16_pixels_per_unit() {
    let mut p = Proj::new();
    p.scene["entities"]
        .as_array_mut()
        .unwrap()
        .retain(|e| e["id"] != "camera");
    let camera = frame_of(&p.runtime()).camera;
    assert_eq!(
        (camera.logical_size, camera.pixels_per_unit, camera.center),
        ([320, 180], 16.0, [0.0, 0.0])
    );
}

#[test]
fn camera_center_follows_the_target_entity() {
    let mut rt = Proj::new().runtime();
    let e = rt.entity("player").unwrap();
    rt.app_mut()
        .world_mut()
        .get_mut::<Transform>(e)
        .unwrap()
        .position = [4.0, -2.0, 0.0];
    assert_eq!(frame_of(&rt).camera.center, [4.0, -2.0]);
}

#[test]
fn camera_without_follow_keeps_its_authored_center() {
    let mut p = Proj::new();
    p.entity("camera")["components"]["Camera2D"] = json!({"center": [8, 9]});
    assert_eq!(frame_of(&p.runtime()).camera.center, [8.0, 9.0]);
}

#[test]
fn camera_keeps_its_center_when_the_followed_entity_is_gone() {
    let mut p = Proj::new();
    p.entity("camera")["components"]["Camera2D"] = json!({"follow": "player", "center": [8, 9]});
    let mut rt = p.runtime();
    let e = rt.entity("player").unwrap();
    rt.app_mut().world_mut().despawn(e);
    assert_eq!(frame_of(&rt).camera.center, [8.0, 9.0]);
}

#[test]
fn custom_camera_settings_pass_through() {
    let mut p = Proj::new();
    p.entity("camera")["components"]["Camera2D"] =
        json!({"logicalSize": [64, 48], "pixelsPerUnit": 8, "pixelSnap": false});
    let camera = frame_of(&p.runtime()).camera;
    assert_eq!(
        (
            camera.logical_size,
            camera.pixels_per_unit,
            camera.pixel_snap
        ),
        ([64, 48], 8.0, false)
    );
}

fn snapped(x: f32, ppu: f32, snap: bool) -> f32 {
    let mut p = Proj::new();
    p.entity("player")["components"]["Transform"]["position"] = json!([x, 0, 0]);
    p.entity("camera")["components"]["Camera2D"] = json!({"pixelsPerUnit": ppu, "pixelSnap": snap});
    corner(&frame_of(&p.runtime()), "sprite:player", 0)[0] + 0.5
}

#[test]
fn pixel_snap_rounds_down_to_the_nearest_pixel() {
    assert_eq!(snapped(0.04, 10.0, true), 0.0);
}

#[test]
fn pixel_snap_rounds_up_to_the_nearest_pixel() {
    assert!(near(snapped(0.06, 10.0, true), 0.1));
}

#[test]
fn pixel_snap_rounds_negative_positions_symmetrically() {
    assert!(near(snapped(-0.06, 10.0, true), -0.1));
}

#[test]
fn pixel_snap_can_be_disabled() {
    assert!(near(snapped(0.04, 10.0, false), 0.04));
}

#[test]
fn pixel_snap_leaves_whole_pixel_positions_alone() {
    assert!(near(snapped(0.5, 16.0, true), 0.5));
}

#[test]
fn snapping_applies_to_sprites_not_to_the_camera_center() {
    let mut p = Proj::new();
    p.entity("player")["components"]["Transform"]["position"] = json!([0.04, 0, 0]);
    p.entity("camera")["components"]["Camera2D"] = json!({"pixelsPerUnit": 10, "follow": "player"});
    assert!(near(frame_of(&p.runtime()).camera.center[0], 0.04));
}

// ---- ordering ----------------------------------------------------------------------------------------------------

#[test]
fn draws_are_ordered_by_layer_then_id() {
    let mut p = Proj::new();
    clear_room(&mut p);
    p.add_entity(sprite("b", 0.0, 0.0, json!({"layer": 0})));
    p.add_entity(sprite("a", 5.0, 0.0, json!({"layer": 0})));
    p.add_entity(sprite("top", 9.0, 0.0, json!({"layer": 5})));
    p.add_entity(sprite("low", 7.0, 0.0, json!({"layer": -1})));
    assert_eq!(
        draw_ids(&frame_of(&p.runtime())),
        vec!["sprite:low", "sprite:a", "sprite:b", "sprite:top"]
    );
}

#[test]
fn draw_order_is_independent_of_spawn_order() {
    let build = |reverse: bool| {
        let mut p = Proj::new();
        clear_room(&mut p);
        let mut items = vec![
            sprite("x", 0.0, 0.0, json!({})),
            sprite("y", 3.0, 0.0, json!({})),
            sprite("z", 6.0, 0.0, json!({})),
        ];
        if reverse {
            items.reverse();
        }
        for item in items {
            p.add_entity(item);
        }
        draw_ids(&frame_of(&p.runtime()))
    };
    assert_eq!(build(false), build(true));
}

#[test]
fn ui_draws_come_after_all_world_draws_whatever_their_layers() {
    let mut rt = Proj::new().runtime();
    let mut ui = rt.app_mut().world_mut().resource_mut::<UiModel>();
    ui.open(PanelOptions {
        id: "hud".into(),
        font_asset: "font".into(),
        text: "a".into(),
        layer: -50,
        ..Default::default()
    })
    .unwrap();
    let frame = frame_of(&rt);
    assert_eq!(draw_ids(&frame), vec!["sprite:player", "ui:hud"]);
    assert_eq!(frame.draws[1].space, DrawSpace::Ui);
}

#[test]
fn ui_panels_are_ordered_by_layer() {
    let mut rt = Proj::new().runtime();
    let mut ui = rt.app_mut().world_mut().resource_mut::<UiModel>();
    for (id, layer) in [("one", 3), ("two", 1), ("three", 2)] {
        ui.open(PanelOptions {
            id: id.into(),
            font_asset: "font".into(),
            text: "a".into(),
            layer,
            ..Default::default()
        })
        .unwrap();
    }
    let ids = draw_ids(&frame_of(&rt));
    assert_eq!(&ids[1..], &["ui:two", "ui:three", "ui:one"]);
}

// ---- mesh renderers ---------------------------------------------------------------------------------------------------

fn meshed(texture: Option<&str>) -> Proj {
    let mut p = Proj::new();
    let mut renderer = json!({"mesh": "mesh"});
    if let Some(t) = texture {
        renderer["texture"] = json!(t);
    }
    p.add_entity(ent(
        "solid",
        json!({"Transform": {"position": [10, 20, 30]}, "MeshRenderer": renderer}),
    ));
    p
}

#[test]
fn mesh_renderers_draw_opaque_with_depth() {
    let frame = frame_of(&meshed(None).runtime());
    let draw = frame.draws.iter().find(|d| d.id == "mesh:solid").unwrap();
    assert_eq!(draw.state.blend, BlendMode::Opaque);
    assert!(draw.state.depth_test && draw.state.depth_write);
    assert_eq!((draw.layer, draw.space), (0, DrawSpace::World));
}

#[test]
fn mesh_vertices_are_translated_by_the_transform() {
    let frame = frame_of(&meshed(None).runtime());
    assert_eq!(corner(&frame, "mesh:solid", 0), [10.0, 20.0, 30.0]);
    assert_eq!(corner(&frame, "mesh:solid", 1), [11.0, 20.0, 30.0]);
    assert_eq!(corner(&frame, "mesh:solid", 2), [10.0, 21.0, 30.0]);
}

#[test]
fn mesh_indices_and_uvs_come_from_the_asset() {
    let frame = frame_of(&meshed(None).runtime());
    let draw = frame.draws.iter().find(|d| d.id == "mesh:solid").unwrap();
    assert_eq!(draw.mesh.indices, vec![0, 1, 2]);
    assert_eq!(draw.mesh.vertices[1].uv, [1.0, 0.0]);
}

#[test]
fn mesh_textures_are_optional() {
    let plain = frame_of(&meshed(None).runtime());
    assert!(
        plain
            .draws
            .iter()
            .find(|d| d.id == "mesh:solid")
            .unwrap()
            .texture
            .is_none()
    );
    let textured = frame_of(&meshed(Some("texture")).runtime());
    assert!(
        textured
            .draws
            .iter()
            .find(|d| d.id == "mesh:solid")
            .unwrap()
            .texture
            .is_some()
    );
}

#[test]
fn the_shared_mesh_asset_is_not_modified_by_extraction() {
    let mut p = meshed(None);
    p.add_entity(ent(
        "other",
        json!({"Transform": {"position": [0, 0, 0]}, "MeshRenderer": {"mesh": "mesh"}}),
    ));
    let frame = frame_of(&p.runtime());
    assert_eq!(corner(&frame, "mesh:other", 0), [0.0, 0.0, 0.0]);
    assert_eq!(corner(&frame, "mesh:solid", 0), [10.0, 20.0, 30.0]);
}

#[test]
fn unknown_meshes_fail_extraction() {
    let mut rt = meshed(None).runtime();
    let e = rt.entity("solid").unwrap();
    rt.app_mut()
        .world_mut()
        .get_mut::<engine_project::MeshRenderer>(e)
        .unwrap()
        .mesh = "ghost".into();
    expect_code(rt.extract_frame(), "render.mesh.unknown");
}

#[test]
fn unknown_mesh_textures_fail_extraction() {
    let mut rt = meshed(Some("texture")).runtime();
    let e = rt.entity("solid").unwrap();
    rt.app_mut()
        .world_mut()
        .get_mut::<engine_project::MeshRenderer>(e)
        .unwrap()
        .texture = Some("ghost".into());
    expect_code(rt.extract_frame(), "render.texture.unknown");
}

// ---- UI draws ------------------------------------------------------------------------------------------------------------

fn with_panel(options: PanelOptions) -> Runtime {
    let mut rt = Proj::new().runtime();
    rt.app_mut()
        .world_mut()
        .resource_mut::<UiModel>()
        .open(options)
        .unwrap();
    rt
}

fn labelled(text: &str) -> PanelOptions {
    PanelOptions {
        id: "menu".into(),
        font_asset: "font".into(),
        text: text.into(),
        rect: Rect {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 50.0,
        },
        ..Default::default()
    }
}

fn ui_draw(frame: &RenderFrame) -> &engine_project::runtime::Draw {
    frame.draws.iter().find(|d| d.id == "ui:menu").unwrap()
}

#[test]
fn panel_text_becomes_one_ui_draw_clipped_to_the_panel() {
    let frame = frame_of(&with_panel(labelled("ab")));
    let draw = ui_draw(&frame);
    assert_eq!(draw.space, DrawSpace::Ui);
    assert_eq!(draw.clip, Some([10.0, 20.0, 100.0, 50.0]));
    assert_eq!(draw.mesh.vertices.len(), 8);
}

#[test]
fn panel_text_uses_the_fonts_atlas_texture() {
    let rt = with_panel(labelled("a"));
    let frame = frame_of(&rt);
    let texture = ui_draw(&frame).texture.as_ref().unwrap();
    assert_eq!((texture.width, texture.height), (8, 8));
}

#[test]
fn panel_text_starts_at_the_panel_origin_and_advances_by_glyph_metrics() {
    let frame = frame_of(&with_panel(labelled("ab")));
    let draw = ui_draw(&frame);
    assert_eq!(draw.mesh.vertices[3].position[..2], [10.0, 20.0]);
    assert_eq!(draw.mesh.vertices[7].position[..2], [15.0, 20.0]);
}

#[test]
fn panel_scroll_moves_the_text_upwards() {
    let mut rt = with_panel(labelled(&"a\n".repeat(10)));
    rt.app_mut()
        .world_mut()
        .resource_mut::<UiModel>()
        .scroll("menu", 20.0)
        .unwrap();
    let frame = frame_of(&rt);
    assert_eq!(ui_draw(&frame).mesh.vertices[3].position[1], 0.0);
}

#[test]
fn panels_without_a_font_cannot_be_drawn() {
    let rt = with_panel(PanelOptions {
        font_asset: String::new(),
        ..labelled("a")
    });
    expect_code(rt.extract_frame(), "render.font.unknown");
}

#[test]
fn panels_with_an_unknown_font_cannot_be_drawn() {
    let rt = with_panel(PanelOptions {
        font_asset: "ghost".into(),
        ..labelled("a")
    });
    expect_code(rt.extract_frame(), "render.font.unknown");
}

#[test]
fn choices_are_rendered_below_the_text_with_a_selection_marker() {
    let rt = with_panel(PanelOptions {
        choices: vec!["x".into(), "y".into()],
        ..labelled("t")
    });
    let frame = frame_of(&rt);
    assert_eq!(ui_draw(&frame).mesh.vertices.len(), 16);
}

#[test]
fn moving_the_selection_regenerates_the_text_geometry() {
    let mut rt = with_panel(PanelOptions {
        choices: vec!["x".into(), "y".into()],
        ..labelled("t")
    });
    let before = frame_of(&rt);
    rt.tick(
        0.1,
        ActionFrame {
            pressed: ["ui_down".to_string()].into(),
            held: ["ui_down".to_string()].into(),
            ..Default::default()
        },
    )
    .unwrap();
    let after = frame_of(&rt);
    assert!(!Arc::ptr_eq(&ui_draw(&before).mesh, &ui_draw(&after).mesh));
}

#[test]
fn closed_panels_disappear_from_the_frame() {
    let mut rt = with_panel(labelled("a"));
    rt.app_mut()
        .world_mut()
        .resource_mut::<UiModel>()
        .close("menu");
    assert!(!draw_ids(&frame_of(&rt)).contains(&"ui:menu".to_string()));
}

// ---- caching ---------------------------------------------------------------------------------------------------------------

#[test]
fn unchanged_geometry_is_shared_between_frames() {
    let rt = Proj::new().runtime();
    let (a, b) = (frame_of(&rt), frame_of(&rt));
    assert!(Arc::ptr_eq(&a.draws[0].mesh, &b.draws[0].mesh));
}

#[test]
fn moving_an_entity_replaces_its_cached_geometry() {
    let mut rt = Proj::new().runtime();
    let a = frame_of(&rt);
    rt.move_entity("player", [1.0, 0.0]).unwrap();
    let b = frame_of(&rt);
    assert!(!Arc::ptr_eq(&a.draws[0].mesh, &b.draws[0].mesh));
    assert_eq!(corner(&b, "sprite:player", 0)[0], 0.5);
}

#[test]
fn only_changed_entities_lose_their_cached_geometry() {
    let mut p = Proj::new();
    p.add_entity(sprite("still", 5.0, 0.0, json!({})));
    let mut rt = p.runtime();
    let a = frame_of(&rt);
    rt.move_entity("player", [1.0, 0.0]).unwrap();
    let b = frame_of(&rt);
    let mesh =
        |f: &RenderFrame, id: &str| f.draws.iter().find(|d| d.id == id).unwrap().mesh.clone();
    assert!(Arc::ptr_eq(
        &mesh(&a, "sprite:still"),
        &mesh(&b, "sprite:still")
    ));
    assert!(!Arc::ptr_eq(
        &mesh(&a, "sprite:player"),
        &mesh(&b, "sprite:player")
    ));
}

#[test]
fn scene_transitions_invalidate_the_geometry_cache() {
    let mut p = Proj::new();
    p.add_scene(
        "scene:b",
        "b.json",
        scene_json("room:b", vec![marker("door_b", 0.0, 0.0)]),
    );
    let mut rt = p.runtime();
    let before = frame_of(&rt);
    rt.request_scene("scene:b", "door_b", "player").unwrap();
    tick(&mut rt, 0.1);
    let after = frame_of(&rt);
    assert!(!Arc::ptr_eq(&before.draws[0].mesh, &after.draws[0].mesh));
    assert_eq!(
        corner(&before, "sprite:player", 0),
        corner(&after, "sprite:player", 0)
    );
}

#[test]
fn extraction_does_not_mutate_the_world() {
    let rt = Proj::new().runtime();
    let before = rt.snapshot().scene.revision();
    for _ in 0..3 {
        frame_of(&rt);
    }
    assert_eq!(rt.snapshot().scene.revision(), before);
}
