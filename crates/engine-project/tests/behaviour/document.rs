//! Revision-checked scene documents: edits, undo/redo, atomic save, reload and
//! isolated play projects.
use super::common::*;
use engine_project::{ActionFrame, EditOperation, MeshRenderer, Project, Runtime, SceneDocument};
use std::fs;

/// The project exactly as it is on disk (no fixture rewrite).
fn disk(p: &Proj) -> Project {
    Project::load(p.manifest_path()).unwrap()
}

fn doc(p: &Proj) -> SceneDocument {
    SceneDocument::new(disk(p))
}

fn go(x: f32) -> EditOperation {
    EditOperation::SetPosition {
        entity_id: "player".into(),
        position: [x, 0.0, 0.0],
    }
}

fn player_x(d: &SceneDocument) -> f32 {
    d.snapshot()
        .entities
        .iter()
        .find(|e| e.id == "player")
        .unwrap()
        .components
        .transform
        .as_ref()
        .unwrap()
        .position[0]
}

fn move_to(d: &mut SceneDocument, x: f32) -> String {
    let revision = d.revision().to_owned();
    d.apply(go(x), &revision).unwrap()
}

fn mesh_renderer(mesh: &str, texture: Option<&str>) -> MeshRenderer {
    MeshRenderer {
        version: 1,
        mesh: mesh.into(),
        texture: texture.map(str::to_owned),
    }
}

// ---- opening ----------------------------------------------------------------------------------

#[test]
fn a_fresh_document_is_clean_and_not_playing() {
    let d = doc(&Proj::new());
    assert!(!d.is_dirty() && !d.is_playing());
    assert_eq!(d.revision().len(), 64);
}

#[test]
fn a_fresh_document_snapshots_the_project_scene() {
    let p = Proj::new();
    let d = doc(&p);
    assert_eq!(d.snapshot(), &disk(&p).scene);
    assert_eq!(d.revision(), disk(&p).scene.revision());
}

#[test]
fn documents_load_from_a_manifest_path() {
    let p = Proj::new();
    p.flush();
    let d = SceneDocument::load(p.manifest_path()).unwrap();
    assert_eq!(d.project().name, "Behaviour Project");
}

#[test]
fn loading_an_invalid_manifest_reports_the_project_error() {
    let p = Proj::new();
    p.write("project.json", "{");
    expect_code(
        SceneDocument::load(p.manifest_path()),
        "project.document.malformed",
    );
}

#[test]
fn separate_documents_do_not_share_edits() {
    let p = Proj::new();
    let (mut a, b) = (doc(&p), doc(&p));
    move_to(&mut a, 5.0);
    assert_eq!(player_x(&b), 0.0);
    assert!(!b.is_dirty());
}

// ---- edits ----------------------------------------------------------------------------------------

#[test]
fn set_position_updates_the_snapshot_revision_and_dirty_flag() {
    let mut d = doc(&Proj::new());
    let before = d.revision().to_owned();
    let after = move_to(&mut d, 3.0);
    assert_eq!(player_x(&d), 3.0);
    assert_ne!(after, before);
    assert_eq!(d.revision(), after);
    assert!(d.is_dirty());
}

#[test]
fn identical_edits_produce_identical_revisions() {
    let p = Proj::new();
    let (mut a, mut b) = (doc(&p), doc(&p));
    assert_eq!(move_to(&mut a, 2.5), move_to(&mut b, 2.5));
}

#[test]
fn moving_an_entity_back_restores_the_original_revision_and_cleanliness() {
    let mut d = doc(&Proj::new());
    let original = d.revision().to_owned();
    move_to(&mut d, 4.0);
    assert!(d.is_dirty());
    let back = move_to(&mut d, 0.0);
    assert_eq!(back, original);
    assert!(!d.is_dirty());
}

#[test]
fn editing_an_unknown_entity_is_refused_without_side_effects() {
    let mut d = doc(&Proj::new());
    let revision = d.revision().to_owned();
    let op = EditOperation::SetPosition {
        entity_id: "ghost".into(),
        position: [1.0; 3],
    };
    expect_code(d.apply(op, &revision), "scene.entity.unknown");
    assert_eq!(d.revision(), revision);
    assert!(!d.is_dirty());
}

#[test]
fn positioning_an_entity_without_a_transform_is_refused() {
    let mut d = doc(&Proj::new());
    let revision = d.revision().to_owned();
    let op = EditOperation::SetPosition {
        entity_id: "camera".into(),
        position: [1.0; 3],
    };
    expect_code(d.apply(op, &revision), "scene.transform.missing");
}

#[test]
fn non_finite_positions_are_refused_by_validation() {
    let mut d = doc(&Proj::new());
    let revision = d.revision().to_owned();
    let op = EditOperation::SetPosition {
        entity_id: "player".into(),
        position: [f32::NAN, 0.0, 0.0],
    };
    expect_code(d.apply(op, &revision), "scene.component.invalid");
    assert_eq!(player_x(&d), 0.0);
}

#[test]
fn a_registered_script_can_be_attached_and_detached() {
    let mut d = doc(&Proj::new());
    let r = d.revision().to_owned();
    let r = d
        .apply(
            EditOperation::SetScript {
                entity_id: "player".into(),
                asset: Some("player".into()),
            },
            &r,
        )
        .unwrap();
    assert_eq!(
        d.snapshot().entities[0]
            .components
            .script
            .as_ref()
            .unwrap()
            .asset,
        "player"
    );
    d.apply(
        EditOperation::SetScript {
            entity_id: "player".into(),
            asset: None,
        },
        &r,
    )
    .unwrap();
    assert!(d.snapshot().entities[0].components.script.is_none());
}

#[test]
fn scripts_can_be_attached_to_entities_without_transforms() {
    let mut d = doc(&Proj::new());
    let r = d.revision().to_owned();
    d.apply(
        EditOperation::SetScript {
            entity_id: "camera".into(),
            asset: Some("other".into()),
        },
        &r,
    )
    .unwrap();
}

#[test]
fn attaching_an_unknown_script_asset_is_refused() {
    let mut d = doc(&Proj::new());
    let r = d.revision().to_owned();
    let op = EditOperation::SetScript {
        entity_id: "player".into(),
        asset: Some("ghost".into()),
    };
    expect_code(d.apply(op, &r), "project.asset.unknown");
    assert!(d.snapshot().entities[0].components.script.is_none());
}

#[test]
fn attaching_a_non_script_asset_is_refused() {
    let mut d = doc(&Proj::new());
    let r = d.revision().to_owned();
    let op = EditOperation::SetScript {
        entity_id: "player".into(),
        asset: Some("texture".into()),
    };
    expect_code(d.apply(op, &r), "project.asset.kind");
}

#[test]
fn a_mesh_renderer_can_be_set_and_cleared() {
    let mut d = doc(&Proj::new());
    let r = d.revision().to_owned();
    let set = EditOperation::SetMeshRenderer {
        entity_id: "player".into(),
        value: Some(mesh_renderer("mesh", Some("texture"))),
    };
    let r = d.apply(set, &r).unwrap();
    assert_eq!(
        d.snapshot().entities[0]
            .components
            .mesh_renderer
            .as_ref()
            .unwrap()
            .texture
            .as_deref(),
        Some("texture")
    );
    d.apply(
        EditOperation::SetMeshRenderer {
            entity_id: "player".into(),
            value: None,
        },
        &r,
    )
    .unwrap();
    assert!(d.snapshot().entities[0].components.mesh_renderer.is_none());
}

fn bad_mesh_edit(value: MeshRenderer, expected: &str) {
    let mut d = doc(&Proj::new());
    let r = d.revision().to_owned();
    expect_code(
        d.apply(
            EditOperation::SetMeshRenderer {
                entity_id: "player".into(),
                value: Some(value),
            },
            &r,
        ),
        expected,
    );
    assert!(!d.is_dirty());
}

table! { bad_mesh_edit;
    mesh_renderer_with_unknown_mesh_is_refused => (mesh_renderer("ghost", None), "project.asset.unknown");
    mesh_renderer_with_a_texture_as_mesh_is_refused => (mesh_renderer("texture", None), "project.asset.kind");
    mesh_renderer_with_a_mesh_as_texture_is_refused => (mesh_renderer("mesh", Some("mesh")), "project.asset.kind");
    mesh_renderer_with_unknown_texture_is_refused => (mesh_renderer("mesh", Some("ghost")), "project.asset.unknown");
    mesh_renderer_with_future_version_is_refused => (MeshRenderer { version: 2, ..mesh_renderer("mesh", None) }, "scene.component.invalid");
    mesh_renderer_with_empty_mesh_id_is_refused => (mesh_renderer("", None), "scene.component.invalid");
}

#[test]
fn stale_revisions_are_rejected_with_a_conflict() {
    let mut d = doc(&Proj::new());
    let first = d.revision().to_owned();
    move_to(&mut d, 1.0);
    expect_code(d.apply(go(2.0), &first), "scene.revision.conflict");
    assert_eq!(player_x(&d), 1.0);
}

#[test]
fn garbage_revisions_are_rejected() {
    let mut d = doc(&Proj::new());
    expect_code(
        d.apply(go(1.0), "not-a-revision"),
        "scene.revision.conflict",
    );
    expect_code(d.undo("x"), "scene.revision.conflict");
    expect_code(d.redo("x"), "scene.revision.conflict");
    expect_code(d.save("x"), "scene.revision.conflict");
    expect_code(d.begin_play("x"), "scene.revision.conflict");
}

// ---- undo and redo ------------------------------------------------------------------------------------

#[test]
fn undo_and_redo_with_nothing_recorded_are_refused() {
    let mut d = doc(&Proj::new());
    let r = d.revision().to_owned();
    expect_code(d.undo(&r), "scene.undo.empty");
    expect_code(d.redo(&r), "scene.redo.empty");
}

#[test]
fn undo_restores_the_previous_revision_and_redo_returns() {
    let mut d = doc(&Proj::new());
    let original = d.revision().to_owned();
    let edited = move_to(&mut d, 2.0);
    assert_eq!(d.undo(&edited).unwrap(), original);
    assert_eq!(player_x(&d), 0.0);
    assert!(!d.is_dirty());
    assert_eq!(d.redo(&original).unwrap(), edited);
    assert_eq!(player_x(&d), 2.0);
    assert!(d.is_dirty());
}

#[test]
fn undo_unwinds_a_chain_in_reverse_order() {
    let mut d = doc(&Proj::new());
    for x in [1.0, 2.0, 3.0] {
        move_to(&mut d, x);
    }
    let mut seen = Vec::new();
    for _ in 0..3 {
        let r = d.revision().to_owned();
        d.undo(&r).unwrap();
        seen.push(player_x(&d));
    }
    assert_eq!(seen, vec![2.0, 1.0, 0.0]);
    let r = d.revision().to_owned();
    expect_code(d.undo(&r), "scene.undo.empty");
}

#[test]
fn redo_replays_a_chain_in_order() {
    let mut d = doc(&Proj::new());
    for x in [1.0, 2.0, 3.0] {
        move_to(&mut d, x);
    }
    for _ in 0..3 {
        let r = d.revision().to_owned();
        d.undo(&r).unwrap();
    }
    let mut seen = Vec::new();
    for _ in 0..3 {
        let r = d.revision().to_owned();
        d.redo(&r).unwrap();
        seen.push(player_x(&d));
    }
    assert_eq!(seen, vec![1.0, 2.0, 3.0]);
}

#[test]
fn a_new_edit_discards_the_redo_branch() {
    let mut d = doc(&Proj::new());
    move_to(&mut d, 1.0);
    let r = d.revision().to_owned();
    d.undo(&r).unwrap();
    move_to(&mut d, 9.0);
    let r = d.revision().to_owned();
    expect_code(d.redo(&r), "scene.redo.empty");
}

#[test]
fn failed_edits_do_not_disturb_the_redo_branch() {
    let mut d = doc(&Proj::new());
    move_to(&mut d, 1.0);
    let r = d.revision().to_owned();
    d.undo(&r).unwrap();
    let r = d.revision().to_owned();
    let bad = EditOperation::SetPosition {
        entity_id: "ghost".into(),
        position: [0.0; 3],
    };
    assert!(d.apply(bad, &r).is_err());
    d.redo(&r).unwrap();
    assert_eq!(player_x(&d), 1.0);
}

#[test]
fn stale_undo_is_a_conflict_that_keeps_history() {
    let mut d = doc(&Proj::new());
    let first = d.revision().to_owned();
    move_to(&mut d, 1.0);
    expect_code(d.undo(&first), "scene.revision.conflict");
    assert_eq!(player_x(&d), 1.0);
    let r = d.revision().to_owned();
    d.undo(&r).unwrap();
}

#[test]
fn history_is_capped_at_1024_steps() {
    let mut d = doc(&Proj::new());
    for i in 1..=1030 {
        move_to(&mut d, i as f32);
    }
    for _ in 0..1024 {
        let r = d.revision().to_owned();
        d.undo(&r).unwrap();
    }
    let r = d.revision().to_owned();
    expect_code(d.undo(&r), "scene.undo.empty");
    assert_eq!(player_x(&d), 6.0);
}

#[test]
fn undoing_an_attached_script_removes_it_again() {
    let mut d = doc(&Proj::new());
    let r = d.revision().to_owned();
    let r2 = d
        .apply(
            EditOperation::SetScript {
                entity_id: "player".into(),
                asset: Some("player".into()),
            },
            &r,
        )
        .unwrap();
    d.undo(&r2).unwrap();
    assert!(d.snapshot().entities[0].components.script.is_none());
}

// ---- save and reload -------------------------------------------------------------------------------------------

#[test]
fn saving_writes_the_scene_and_marks_the_document_clean() {
    let p = Proj::new();
    let mut d = doc(&p);
    let r = move_to(&mut d, 7.0);
    assert_eq!(d.save(&r).unwrap(), r);
    assert!(!d.is_dirty());
    assert_eq!(
        disk(&p).scene.entities[0]
            .components
            .transform
            .as_ref()
            .unwrap()
            .position[0],
        7.0
    );
}

#[test]
fn saved_scene_files_are_pretty_printed_json() {
    let p = Proj::new();
    let mut d = doc(&p);
    let r = move_to(&mut d, 7.0);
    d.save(&r).unwrap();
    let text = String::from_utf8(p.read("scene.json")).unwrap();
    assert!(text.contains("\n  \"entities\""));
}

#[test]
fn a_reloaded_document_has_the_revision_that_was_saved() {
    let p = Proj::new();
    let mut d = doc(&p);
    let r = move_to(&mut d, 7.0);
    d.save(&r).unwrap();
    assert_eq!(doc(&p).revision(), r);
}

#[test]
fn saving_updates_the_documents_project() {
    let p = Proj::new();
    let mut d = doc(&p);
    let r = move_to(&mut d, 7.0);
    d.save(&r).unwrap();
    assert_eq!(&d.project().scene, d.snapshot());
}

#[test]
fn saving_with_a_stale_revision_does_not_touch_the_file() {
    let p = Proj::new();
    let mut d = doc(&p);
    let stale = d.revision().to_owned();
    move_to(&mut d, 7.0);
    let before = p.read("scene.json");
    expect_code(d.save(&stale), "scene.revision.conflict");
    assert_eq!(p.read("scene.json"), before);
    assert!(d.is_dirty());
}

#[test]
fn saving_leaves_no_temporary_files() {
    let p = Proj::new();
    let before: Vec<_> = fs::read_dir(p.root())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    let mut d = doc(&p);
    let r = move_to(&mut d, 1.0);
    d.save(&r).unwrap();
    let mut after: Vec<_> = fs::read_dir(p.root())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    let mut expected = before;
    expected.sort();
    after.sort();
    assert_eq!(after, expected);
}

#[test]
fn saving_twice_is_idempotent() {
    let p = Proj::new();
    let mut d = doc(&p);
    let r = move_to(&mut d, 1.0);
    d.save(&r).unwrap();
    let once = p.read("scene.json");
    d.save(&r).unwrap();
    assert_eq!(p.read("scene.json"), once);
}

#[test]
fn undoing_after_a_save_makes_the_document_dirty_again() {
    let p = Proj::new();
    let mut d = doc(&p);
    let r = move_to(&mut d, 1.0);
    d.save(&r).unwrap();
    let back = d.undo(&r).unwrap();
    assert!(d.is_dirty());
    d.save(&back).unwrap();
    assert!(!d.is_dirty());
    assert_eq!(
        doc(&p).snapshot().entities[0]
            .components
            .transform
            .as_ref()
            .unwrap()
            .position[0],
        0.0
    );
}

#[test]
fn saving_fails_cleanly_when_the_scene_file_vanished() {
    let p = Proj::new();
    let mut d = doc(&p);
    let r = move_to(&mut d, 1.0);
    fs::remove_file(p.root().join("scene.json")).unwrap();
    expect_code(d.save(&r), "project.asset.missing");
    assert!(d.is_dirty());
}

#[test]
fn saving_only_touches_the_scene_file() {
    let p = Proj::new();
    let manifest = p.read("project.json");
    let script = p.read("player.rs");
    let mut d = doc(&p);
    let r = move_to(&mut d, 1.0);
    d.save(&r).unwrap();
    assert_eq!(p.read("project.json"), manifest);
    assert_eq!(p.read("player.rs"), script);
}

#[test]
fn reload_of_a_clean_document_picks_up_disk_changes_and_clears_history() {
    let mut p = Proj::new();
    let mut d = doc(&p);
    move_to(&mut d, 1.0);
    let r = d.revision().to_owned();
    d.save(&r).unwrap();
    p.entity("player")["components"]["Transform"]["position"] = serde_json::json!([9, 0, 0]);
    p.flush();
    d.reload(false).unwrap();
    assert_eq!(player_x(&d), 9.0);
    let r = d.revision().to_owned();
    expect_code(d.undo(&r), "scene.undo.empty");
}

#[test]
fn reload_refuses_to_discard_unsaved_work_by_default() {
    let p = Proj::new();
    let mut d = doc(&p);
    move_to(&mut d, 1.0);
    expect_code(d.reload(false), "scene.unsaved");
    assert_eq!(player_x(&d), 1.0);
}

#[test]
fn reload_with_discard_returns_to_the_disk_state() {
    let p = Proj::new();
    let mut d = doc(&p);
    move_to(&mut d, 1.0);
    d.reload(true).unwrap();
    assert_eq!(player_x(&d), 0.0);
    assert!(!d.is_dirty());
}

#[test]
fn reload_keeps_the_document_when_the_disk_state_is_broken() {
    let p = Proj::new();
    let mut d = doc(&p);
    move_to(&mut d, 1.0);
    p.write("scene.json", "{");
    expect_code(d.reload(true), "project.document.malformed");
    assert_eq!(player_x(&d), 1.0);
    assert!(d.is_dirty());
}

#[test]
fn reload_is_refused_while_playing() {
    let p = Proj::new();
    let mut d = doc(&p);
    let r = d.revision().to_owned();
    d.begin_play(&r).unwrap();
    expect_code(d.reload(true), "scene.play.active");
    d.end_play();
    d.reload(true).unwrap();
}

// ---- play isolation ----------------------------------------------------------------------------------------------

#[test]
fn begin_play_hands_out_a_project_with_the_edited_scene() {
    let p = Proj::new();
    let mut d = doc(&p);
    let r = move_to(&mut d, 6.0);
    let play: Project = d.begin_play(&r).unwrap();
    assert!(d.is_playing());
    assert_eq!(
        play.scene.entities[0]
            .components
            .transform
            .as_ref()
            .unwrap()
            .position[0],
        6.0
    );
    assert_eq!(
        disk(&p).scene.entities[0]
            .components
            .transform
            .as_ref()
            .unwrap()
            .position[0],
        0.0
    );
}

#[test]
fn begin_play_twice_is_refused_until_play_ends() {
    let mut d = doc(&Proj::new());
    let r = d.revision().to_owned();
    d.begin_play(&r).unwrap();
    expect_code(d.begin_play(&r), "scene.play.active");
    d.end_play();
    assert!(!d.is_playing());
    d.begin_play(&r).unwrap();
}

#[test]
fn begin_play_with_a_stale_revision_is_a_conflict() {
    let mut d = doc(&Proj::new());
    let stale = d.revision().to_owned();
    move_to(&mut d, 1.0);
    expect_code(d.begin_play(&stale), "scene.revision.conflict");
    assert!(!d.is_playing());
}

#[test]
fn mutating_the_play_project_never_changes_the_document() {
    let mut d = doc(&Proj::new());
    let r = d.revision().to_owned();
    let mut play = d.begin_play(&r).unwrap();
    play.scene.entities[0]
        .components
        .transform
        .as_mut()
        .unwrap()
        .position[0] = 99.0;
    assert_eq!(player_x(&d), 0.0);
    assert_eq!(d.revision(), r);
}

#[test]
fn end_play_without_play_is_harmless() {
    let mut d = doc(&Proj::new());
    d.end_play();
    assert!(!d.is_playing());
}

#[test]
fn a_play_session_runs_the_edited_scene_without_touching_the_authored_state() {
    let p = Proj::new();
    let mut d = doc(&p);
    let r = move_to(&mut d, 6.0);
    let on_disk = p.read("scene.json");
    let play = d.begin_play(&r).unwrap();
    let mut rt = Runtime::new(play).unwrap();
    rt.move_entity("player", [2.0, 0.0]).unwrap();
    rt.tick(0.1, ActionFrame::default()).unwrap();
    assert_eq!(rt.position("player").unwrap()[0], 8.0);
    rt.stop().unwrap();
    d.end_play();
    assert_eq!(player_x(&d), 6.0);
    assert_eq!(d.revision(), r);
    assert_eq!(p.read("scene.json"), on_disk);
}

#[test]
fn editing_continues_normally_after_play_ends() {
    let mut d = doc(&Proj::new());
    let r = d.revision().to_owned();
    d.begin_play(&r).unwrap();
    d.end_play();
    move_to(&mut d, 2.0);
    assert_eq!(player_x(&d), 2.0);
}

// ---- structural edits (entity create / replace / delete) --------------------------------------

fn extra(id: &str) -> engine_project::SceneEntity {
    serde_json::from_value(ent(
        id,
        serde_json::json!({"Transform":{"position":[1,2,3]}}),
    ))
    .unwrap()
}

fn ids(d: &SceneDocument) -> Vec<String> {
    d.snapshot().entities.iter().map(|e| e.id.clone()).collect()
}

#[test]
fn create_entity_appends_and_changes_the_revision() {
    let mut d = doc(&Proj::new());
    let before = d.revision().to_owned();
    let after = d
        .apply(
            EditOperation::CreateEntity {
                entity: extra("crate"),
            },
            &before,
        )
        .unwrap();
    assert_ne!(before, after);
    assert_eq!(ids(&d).last().unwrap(), "crate");
    assert!(d.is_dirty());
}

#[test]
fn create_entity_rejects_duplicate_ids_and_keeps_the_scene() {
    let mut d = doc(&Proj::new());
    let before = d.revision().to_owned();
    expect_code(
        d.apply(
            EditOperation::CreateEntity {
                entity: extra("player"),
            },
            &before,
        ),
        "scene.entity.id.duplicate",
    );
    assert_eq!(d.revision(), before);
    assert!(!d.is_dirty());
}

#[test]
fn create_entity_is_validated_against_the_project_catalog() {
    let mut d = doc(&Proj::new());
    let before = d.revision().to_owned();
    let mut bad = extra("ghost");
    bad.components.script = Some(engine_project::Script {
        version: 1,
        asset: "missing".into(),
    });
    expect_code(
        d.apply(EditOperation::CreateEntity { entity: bad }, &before),
        "project.asset.unknown",
    );
    assert_eq!(d.revision(), before);
}

#[test]
fn replace_entity_swaps_all_components() {
    let mut d = doc(&Proj::new());
    let before = d.revision().to_owned();
    let mut entity = extra("player");
    entity.name = "Renamed".into();
    d.apply(EditOperation::ReplaceEntity { entity }, &before)
        .unwrap();
    let player = d
        .snapshot()
        .entities
        .iter()
        .find(|e| e.id == "player")
        .unwrap();
    assert_eq!(player.name, "Renamed");
    assert!(player.components.sprite_renderer.is_none());
    assert_eq!(
        player.components.transform.as_ref().unwrap().position,
        [1.0, 2.0, 3.0]
    );
}

#[test]
fn replace_entity_requires_an_existing_id() {
    let mut d = doc(&Proj::new());
    let before = d.revision().to_owned();
    expect_code(
        d.apply(
            EditOperation::ReplaceEntity {
                entity: extra("nobody"),
            },
            &before,
        ),
        "scene.entity.unknown",
    );
}

#[test]
fn delete_entity_removes_it_and_can_be_undone() {
    let mut d = doc(&Proj::new());
    let before = d.revision().to_owned();
    d.apply(
        EditOperation::DeleteEntity {
            entity_id: "camera".into(),
        },
        &before,
    )
    .unwrap();
    assert_eq!(ids(&d), ["player"]);
    let after = d.revision().to_owned();
    d.undo(&after).unwrap();
    assert_eq!(ids(&d), ["player", "camera"]);
}

#[test]
fn delete_entity_rejects_dangling_camera_follow_and_unknown_ids() {
    let mut d = doc(&Proj::new());
    let before = d.revision().to_owned();
    // The camera follows the player, so deleting the follow target is invalid.
    expect_code(
        d.apply(
            EditOperation::DeleteEntity {
                entity_id: "player".into(),
            },
            &before,
        ),
        "scene.camera.follow",
    );
    expect_code(
        d.apply(
            EditOperation::DeleteEntity {
                entity_id: "nobody".into(),
            },
            &before,
        ),
        "scene.entity.unknown",
    );
    assert_eq!(d.revision(), before);
}

#[test]
fn structural_edits_still_obey_revision_checks() {
    let mut d = doc(&Proj::new());
    expect_code(
        d.apply(
            EditOperation::CreateEntity {
                entity: extra("late"),
            },
            "stale",
        ),
        "scene.revision.conflict",
    );
}

#[test]
fn for_scene_edits_and_saves_a_non_startup_scene() {
    let mut p = Proj::new();
    p.add_scene(
        "scene:b",
        "b.json",
        scene_json(
            "room:b",
            vec![ent(
                "b1",
                serde_json::json!({"Transform":{"position":[0,0,0]}}),
            )],
        ),
    );
    let project = p.project();
    let other = engine_project::load_scene(&project.root, std::path::Path::new("b.json")).unwrap();
    let mut d = SceneDocument::for_scene(&project, other);
    let r = d.revision().to_owned();
    d.apply(
        EditOperation::CreateEntity {
            entity: extra("b2"),
        },
        &r,
    )
    .unwrap();
    let r = d.revision().to_owned();
    d.save(&r).unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&p.read("b.json")).unwrap();
    assert_eq!(saved["entities"].as_array().unwrap().len(), 2);
    let startup: serde_json::Value = serde_json::from_slice(&p.read("scene.json")).unwrap();
    assert_eq!(
        startup["scene_id"], "room:a",
        "the startup scene is untouched"
    );
}
