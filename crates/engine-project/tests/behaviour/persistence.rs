//! Saves and settings: envelopes, versioned migration, backups and recovery,
//! bounded JSON, interrupted atomic writes, slot locks and link refusal.
use super::common::*;
use engine_project::{Error, Result, persistence::*};
use serde_json::{Map, Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

type Data = Map<String, Value>;

fn options(dir: &Path) -> PersistenceOptions {
    PersistenceOptions::new(dir.to_owned(), "game".into())
}

fn store(dir: &Path) -> PersistenceStore {
    PersistenceStore::new(options(dir)).unwrap()
}

fn store_with(dir: &Path, change: impl FnOnce(&mut PersistenceOptions)) -> PersistenceStore {
    let mut o = options(dir);
    change(&mut o);
    PersistenceStore::new(o).unwrap()
}

fn saves(dir: &Path) -> PathBuf {
    dir.join("game/saves")
}

fn primary(dir: &Path, slot: &str) -> PathBuf {
    saves(dir).join(format!("{slot}.json"))
}

fn backup(dir: &Path, slot: &str) -> PathBuf {
    saves(dir).join(format!("{slot}.json.bak"))
}

fn write_envelope(path: &Path, schema: u64, content: u64, data: Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        serde_json::to_vec(&json!({"schema": schema, "content_version": content, "data": data}))
            .unwrap(),
    )
    .unwrap();
}

fn n(value: i64) -> Data {
    map(json!({ "n": value }))
}

fn level(store: &PersistenceStore, slot: &str) -> i64 {
    store.load(slot, None).unwrap()["n"].as_i64().unwrap()
}

fn leftovers(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = fs::read_dir(saves(dir))
        .map(|d| {
            d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    found.retain(|name| !name.ends_with(".lock"));
    found.sort();
    found
}

// ---- migrations used by the tests ---------------------------------------------------------

fn stamp(schema: u32, content: u32, mut data: Data) -> Result<Data> {
    data.insert("migrated_from".into(), json!([schema, content]));
    Ok(data)
}

fn chain(schema: u32, content: u32, mut data: Data) -> Result<Data> {
    if schema < 2
        && let Some(hp) = data.remove("hp")
    {
        data.insert("health".into(), hp);
    }
    if schema < 3 {
        data.insert("slots".into(), json!([]));
    }
    if content < 2 {
        data.insert("flag_a".into(), json!(false));
    }
    if content < 3 {
        data.insert("flag_b".into(), json!(true));
    }
    data.insert("origin".into(), json!(format!("{schema}.{content}")));
    Ok(data)
}

fn failing(_: u32, _: u32, _: Data) -> Result<Data> {
    Err(Error::new(
        "fixture.migrate",
        "deliberate migration failure",
    ))
}

fn panicking(_: u32, _: u32, _: Data) -> Result<Data> {
    panic!("deliberate migration panic")
}

fn too_deep(_: u32, _: u32, _: Data) -> Result<Data> {
    let mut value = json!(1);
    for _ in 0..40 {
        value = json!({ "d": value });
    }
    Ok(map(value))
}

fn too_big(_: u32, _: u32, _: Data) -> Result<Data> {
    Ok(map(json!({ "pad": "x".repeat(4000) })))
}

fn newest(dir: &Path) -> PersistenceStore {
    store_with(dir, |o| {
        o.schema_version = 3;
        o.content_version = 3;
    })
}

// ---- options ----------------------------------------------------------------------------------

fn bad_options(change: impl FnOnce(&mut PersistenceOptions)) {
    let dir = tempfile::tempdir().unwrap();
    let mut o = options(dir.path());
    change(&mut o);
    expect_code(PersistenceStore::new(o), "persistence.options.invalid");
}

table! { bad_options;
    options_reject_empty_project_ids => (|o| o.project_id = String::new());
    options_reject_project_ids_with_separators => (|o| o.project_id = "a/b".into());
    options_reject_project_ids_with_backslashes => (|o| o.project_id = "a\\b".into());
    options_reject_project_ids_with_spaces => (|o| o.project_id = "a b".into());
    options_reject_dot_dot_project_ids => (|o| o.project_id = "..".into());
    options_reject_non_ascii_project_ids => (|o| o.project_id = "jeu\u{e9}".into());
    options_reject_overlong_project_ids => (|o| o.project_id = "p".repeat(65));
    options_reject_zero_schema_version => (|o| o.schema_version = 0);
    options_reject_zero_content_version => (|o| o.content_version = 0);
    options_reject_zero_byte_limit => (|o| o.max_bytes = 0);
    options_reject_byte_limit_above_64_mib => (|o| o.max_bytes = 64 * 1024 * 1024 + 1);
    options_reject_zero_depth => (|o| o.max_depth = 0);
    options_reject_depth_above_64 => (|o| o.max_depth = 65);
}

fn good_options(change: impl FnOnce(&mut PersistenceOptions)) {
    let dir = tempfile::tempdir().unwrap();
    let mut o = options(dir.path());
    change(&mut o);
    PersistenceStore::new(o).unwrap();
}

table! { good_options;
    options_accept_64_character_project_ids => (|o| o.project_id = "p".repeat(64));
    options_accept_dashes_and_underscores => (|o| o.project_id = "My-Game_01".into());
    options_accept_the_64_mib_byte_limit => (|o| o.max_bytes = 64 * 1024 * 1024);
    options_accept_depth_one => (|o| o.max_depth = 1);
    options_accept_depth_64 => (|o| o.max_depth = 64);
    options_accept_large_schema_numbers => (|o| { o.schema_version = u32::MAX; o.content_version = u32::MAX; });
}

#[test]
fn default_options_are_version_one_with_one_mebibyte() {
    let o = PersistenceOptions::new(PathBuf::from("x"), "game".into());
    assert_eq!((o.schema_version, o.content_version), (1, 1));
    assert_eq!((o.max_bytes, o.max_depth), (1024 * 1024, 32));
    assert!(o.fail.is_none());
}

#[test]
fn store_exposes_the_options_it_was_built_with() {
    let dir = tempfile::tempdir().unwrap();
    let s = store_with(dir.path(), |o| o.max_depth = 7);
    assert_eq!(s.options().max_depth, 7);
    assert_eq!(s.options().project_id, "game");
}

// ---- project identity ------------------------------------------------------------------------------

#[test]
fn project_id_of_the_empty_name_is_the_fnv_offset_basis() {
    assert_eq!(project_id(""), "project-14695981039346656037");
}

#[test]
fn project_id_matches_published_fnv_1a_vectors() {
    assert_eq!(project_id("a"), "project-12638187200555641996");
    assert_eq!(project_id("foobar"), "project-9625390261332436968");
}

#[test]
fn project_id_is_stable_for_a_real_project_name() {
    assert_eq!(
        project_id("Behaviour Project"),
        "project-9293127966734626609"
    );
}

#[test]
fn project_id_distinguishes_similar_names() {
    assert_ne!(project_id("Game"), project_id("game"));
    assert_ne!(project_id("Game"), project_id("Game "));
}

#[test]
fn project_id_is_deterministic_and_unicode_safe() {
    assert_eq!(project_id("Ωmega 十"), project_id("Ωmega 十"));
}

#[test]
fn project_ids_are_valid_store_identifiers() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["", "x", "Ωmega 十 🎮", "a/b\\c"] {
        let o = PersistenceOptions::new(dir.path().to_owned(), project_id(name));
        PersistenceStore::new(o).unwrap();
    }
}

#[test]
fn default_user_data_root_rejects_unsafe_project_ids() {
    expect_code(
        PersistenceStore::default_user_data_root("../x"),
        "persistence.project.invalid",
    );
    expect_code(
        PersistenceStore::default_user_data_root(""),
        "persistence.project.invalid",
    );
}

#[test]
fn default_user_data_root_is_absolute_and_engine_scoped_when_available() {
    match PersistenceStore::default_user_data_root("project-1") {
        Ok(root) => {
            assert!(root.is_absolute());
            assert!(root.ends_with("CPPGameEngine"));
        }
        Err(e) => assert_eq!(e.diagnostics[0].code, "persistence.root.unavailable"),
    }
}

// ---- slot names ----------------------------------------------------------------------------------------

fn bad_slot(slot: &str) {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    expect_code(s.save(slot, Data::new(), None), "persistence.slot.invalid");
    expect_code(s.load(slot, None), "persistence.slot.invalid");
    expect_code(s.load_backup(slot, None), "persistence.slot.invalid");
    expect_code(s.recover_backup(slot, None), "persistence.slot.invalid");
    assert!(
        !dir.path()
            .join("game")
            .join("saves")
            .join(format!("{slot}.json"))
            .exists()
    );
}

table! { bad_slot;
    slot_empty_is_invalid => ("");
    slot_with_forward_slash_is_invalid => ("a/b");
    slot_with_backslash_is_invalid => ("a\\b");
    slot_dot_dot_is_invalid => ("..");
    slot_single_dot_is_invalid => (".");
    slot_with_extension_is_invalid => ("a.b");
    slot_with_space_is_invalid => ("a b");
    slot_with_accent_is_invalid => ("caf\u{e9}");
    slot_with_colon_is_invalid => ("slot:1");
    slot_with_nul_is_invalid => ("a\0b");
    slot_over_64_characters_is_invalid => (&"s".repeat(65));
    slot_traversal_is_invalid => ("../../escape");
}

#[test]
fn slot_names_with_dashes_underscores_and_digits_work() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.save("A_b-C9", n(1), None).unwrap();
    assert_eq!(level(&s, "A_b-C9"), 1);
}

#[test]
fn slot_names_of_exactly_64_characters_work() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let slot = "s".repeat(64);
    s.save(&slot, n(2), None).unwrap();
    assert_eq!(level(&s, &slot), 2);
}

#[test]
fn rejected_slot_names_never_create_directories() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let _ = s.save("../x", Data::new(), None);
    assert!(!dir.path().join("game").exists());
}

// ---- save, load, layout -------------------------------------------------------------------------------------

#[test]
fn saved_data_round_trips_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let data = map(json!({
        "name": "Ωmega 十 🎮", "big": i64::MAX, "neg": -42, "pi": 0.1, "none": null, "flag": true,
        "list": [1, "two", [3], {"four": 4}], "empty": {}, "": "empty key", "quote\"key": "line\nbreak"
    }));
    s.save("slot", data.clone(), None).unwrap();
    assert_eq!(s.load("slot", None).unwrap(), data);
}

#[test]
fn nested_envelope_looking_data_is_not_mistaken_for_an_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let data = map(json!({"schema": 99, "content_version": 99, "data": {"x": 1}}));
    s.save("slot", data.clone(), None).unwrap();
    assert_eq!(s.load("slot", None).unwrap(), data);
}

#[test]
fn empty_data_is_valid() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.save("slot", Data::new(), None).unwrap();
    assert!(s.load("slot", None).unwrap().is_empty());
}

#[test]
fn loading_a_slot_that_was_never_saved_reports_missing() {
    let dir = tempfile::tempdir().unwrap();
    let error = store(dir.path()).load("slot", None).unwrap_err();
    assert_eq!(error.diagnostics[0].code, "persistence.missing");
    assert!(error.diagnostics[0].file.ends_with("slot.json"));
}

#[test]
fn each_save_replaces_the_primary() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    for i in 1..=5 {
        s.save("slot", n(i), None).unwrap();
        assert_eq!(level(&s, "slot"), i);
    }
}

#[test]
fn on_disk_envelope_has_exactly_the_documented_members() {
    let dir = tempfile::tempdir().unwrap();
    store_with(dir.path(), |o| {
        o.schema_version = 4;
        o.content_version = 9;
    })
    .save("slot", n(1), None)
    .unwrap();
    let value: Value =
        serde_json::from_slice(&fs::read(primary(dir.path(), "slot")).unwrap()).unwrap();
    let mut keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, vec!["content_version", "data", "schema"]);
    assert_eq!(
        (value["schema"].as_u64(), value["content_version"].as_u64()),
        (Some(4), Some(9))
    );
}

#[test]
fn saves_land_in_the_project_saves_directory() {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path()).save("slot", n(1), None).unwrap();
    assert!(primary(dir.path(), "slot").is_file());
    assert!(saves(dir.path()).join("slot.json.lock").is_file());
}

#[test]
fn settings_land_in_the_project_settings_directory() {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path()).save_settings(n(1), None).unwrap();
    assert!(dir.path().join("game/settings/preferences.json").is_file());
}

#[test]
fn settings_round_trip_and_start_missing() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    expect_code(s.load_settings(None), "persistence.missing");
    s.save_settings(map(json!({"volume": 0.5})), None).unwrap();
    assert_eq!(s.load_settings(None).unwrap()["volume"], 0.5);
}

#[test]
fn a_save_slot_named_preferences_does_not_touch_settings() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.save_settings(n(1), None).unwrap();
    s.save("preferences", n(2), None).unwrap();
    assert_eq!(s.load_settings(None).unwrap()["n"], 1);
    assert_eq!(level(&s, "preferences"), 2);
}

#[test]
fn settings_keep_their_own_backup() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.save_settings(n(1), None).unwrap();
    s.save_settings(n(2), None).unwrap();
    assert!(
        dir.path()
            .join("game/settings/preferences.json.bak")
            .is_file()
    );
    assert!(!backup(dir.path(), "preferences").exists());
}

#[test]
fn slots_are_independent() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.save("a", n(1), None).unwrap();
    s.save("b", n(2), None).unwrap();
    s.save("a", n(3), None).unwrap();
    assert_eq!((level(&s, "a"), level(&s, "b")), (3, 2));
    assert!(!backup(dir.path(), "b").exists());
}

#[test]
fn projects_sharing_a_root_are_isolated() {
    let dir = tempfile::tempdir().unwrap();
    let a = PersistenceStore::new(PersistenceOptions::new(
        dir.path().to_owned(),
        "alpha".into(),
    ))
    .unwrap();
    let b = PersistenceStore::new(PersistenceOptions::new(
        dir.path().to_owned(),
        "beta".into(),
    ))
    .unwrap();
    a.save("slot", n(1), None).unwrap();
    expect_code(b.load("slot", None), "persistence.missing");
}

#[test]
fn distinct_roots_are_isolated() {
    let (d1, d2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    store(d1.path()).save("slot", n(1), None).unwrap();
    expect_code(store(d2.path()).load("slot", None), "persistence.missing");
}

#[test]
fn a_root_that_does_not_exist_yet_is_created_on_first_save() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("deep/er/root");
    PersistenceStore::new(PersistenceOptions::new(root.clone(), "game".into()))
        .unwrap()
        .save("s", n(1), None)
        .unwrap();
    assert!(root.join("game/saves/s.json").is_file());
}

#[test]
fn a_new_store_reads_what_an_earlier_store_wrote() {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path()).save("slot", n(7), None).unwrap();
    assert_eq!(level(&store(dir.path()), "slot"), 7);
}

#[test]
fn many_sequential_saves_keep_the_latest_and_previous_pair() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    for i in 1..=50 {
        s.save("slot", n(i), None).unwrap();
    }
    assert_eq!(level(&s, "slot"), 50);
    assert_eq!(s.load_backup("slot", None).unwrap()["n"], 49);
}

#[test]
fn threads_saving_to_distinct_slots_do_not_interfere() {
    let dir = tempfile::tempdir().unwrap();
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let root = dir.path().to_owned();
            std::thread::spawn(move || {
                let s = store(&root);
                for round in 0..5 {
                    s.save(&format!("slot{i}"), n(i * 100 + round), None)
                        .unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let s = store(dir.path());
    for i in 0..8 {
        assert_eq!(level(&s, &format!("slot{i}")), i * 100 + 4);
    }
}

#[test]
fn contending_threads_either_save_or_see_a_busy_lock_and_never_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path()).save("shared", n(0), None).unwrap();
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let root = dir.path().to_owned();
            std::thread::spawn(move || {
                let s = store(&root);
                for round in 0..15 {
                    match s.save("shared", n(i * 100 + round), None) {
                        Ok(()) => {}
                        Err(e) => assert_eq!(e.diagnostics[0].code, "persistence.lock.busy"),
                    }
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let s = store(dir.path());
    assert!(s.load("shared", None).is_ok());
    assert!(s.load_backup("shared", None).is_ok());
}

// ---- backups -----------------------------------------------------------------------------------------------------

#[test]
fn the_first_save_has_no_backup() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.save("slot", n(1), None).unwrap();
    assert!(!backup(dir.path(), "slot").exists());
    expect_code(s.load_backup("slot", None), "persistence.missing");
}

#[test]
fn the_second_save_backs_up_the_first_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.save("slot", n(1), None).unwrap();
    let first = fs::read(primary(dir.path(), "slot")).unwrap();
    s.save("slot", n(2), None).unwrap();
    assert_eq!(fs::read(backup(dir.path(), "slot")).unwrap(), first);
    assert_eq!(s.load_backup("slot", None).unwrap()["n"], 1);
}

#[test]
fn the_backup_always_trails_the_primary_by_one_generation() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    for i in 1..=4 {
        s.save("slot", n(i), None).unwrap();
        if i > 1 {
            assert_eq!(s.load_backup("slot", None).unwrap()["n"], i - 1);
        }
    }
}

#[test]
fn saving_leaves_no_temporary_files_behind() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    for i in 1..=3 {
        s.save("slot", n(i), None).unwrap();
    }
    assert_eq!(leftovers(dir.path()), vec!["slot.json", "slot.json.bak"]);
}

#[test]
fn a_stale_temporary_file_does_not_disturb_loading_or_saving() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.save("slot", n(1), None).unwrap();
    fs::write(saves(dir.path()).join(".tmpXYZ123"), b"half written").unwrap();
    assert_eq!(level(&s, "slot"), 1);
    s.save("slot", n(2), None).unwrap();
    assert_eq!(level(&s, "slot"), 2);
}

// ---- interrupted writes ----------------------------------------------------------------------------------------------

fn injected(stage: PersistenceStage) -> impl FnOnce(&mut PersistenceOptions) {
    move |o| {
        o.fail = Some(match stage {
            PersistenceStage::Write => |s| s == PersistenceStage::Write,
            PersistenceStage::Flush => |s| s == PersistenceStage::Flush,
            PersistenceStage::Replace => |s| s == PersistenceStage::Replace,
        })
    }
}

fn interrupted_new_slot(stage: PersistenceStage, expected: &str) {
    let dir = tempfile::tempdir().unwrap();
    let broken = store_with(dir.path(), injected(stage));
    expect_code(broken.save("slot", n(1), None), expected);
    assert!(!primary(dir.path(), "slot").exists());
    assert!(leftovers(dir.path()).is_empty());
    expect_code(store(dir.path()).load("slot", None), "persistence.missing");
}

table! { interrupted_new_slot;
    write_failure_on_a_new_slot_leaves_nothing => (PersistenceStage::Write, "persistence.write.failed");
    flush_failure_on_a_new_slot_leaves_nothing => (PersistenceStage::Flush, "persistence.flush.failed");
    replace_failure_on_a_new_slot_leaves_nothing => (PersistenceStage::Replace, "persistence.replace.failed");
}

fn interrupted_existing_slot(stage: PersistenceStage, expected: &str) {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path()).save("slot", n(1), None).unwrap();
    let before = fs::read(primary(dir.path(), "slot")).unwrap();
    let broken = store_with(dir.path(), injected(stage));
    expect_code(broken.save("slot", n(2), None), expected);
    assert_eq!(fs::read(primary(dir.path(), "slot")).unwrap(), before);
    assert_eq!(level(&store(dir.path()), "slot"), 1);
    assert!(
        leftovers(dir.path())
            .iter()
            .all(|f| f.starts_with("slot.json"))
    );
}

table! { interrupted_existing_slot;
    write_failure_keeps_the_previous_snapshot => (PersistenceStage::Write, "persistence.write.failed");
    flush_failure_keeps_the_previous_snapshot => (PersistenceStage::Flush, "persistence.flush.failed");
    replace_failure_keeps_the_previous_snapshot => (PersistenceStage::Replace, "persistence.replace.failed");
}

fn interrupted_settings(stage: PersistenceStage, expected: &str) {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path()).save_settings(n(1), None).unwrap();
    let broken = store_with(dir.path(), injected(stage));
    expect_code(broken.save_settings(n(2), None), expected);
    assert_eq!(store(dir.path()).load_settings(None).unwrap()["n"], 1);
}

table! { interrupted_settings;
    write_failure_keeps_previous_settings => (PersistenceStage::Write, "persistence.write.failed");
    flush_failure_keeps_previous_settings => (PersistenceStage::Flush, "persistence.flush.failed");
    replace_failure_keeps_previous_settings => (PersistenceStage::Replace, "persistence.replace.failed");
}

#[test]
fn a_failed_save_releases_the_slot_lock_for_the_next_attempt() {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path()).save("slot", n(1), None).unwrap();
    let broken = store_with(dir.path(), injected(PersistenceStage::Replace));
    assert!(broken.save("slot", n(2), None).is_err());
    store(dir.path()).save("slot", n(3), None).unwrap();
    assert_eq!(level(&store(dir.path()), "slot"), 3);
}

#[test]
fn a_failed_replace_still_leaves_a_recoverable_pair() {
    let dir = tempfile::tempdir().unwrap();
    let healthy = store(dir.path());
    healthy.save("slot", n(1), None).unwrap();
    healthy.save("slot", n(2), None).unwrap();
    let broken = store_with(dir.path(), injected(PersistenceStage::Replace));
    assert!(broken.save("slot", n(3), None).is_err());
    assert_eq!(level(&healthy, "slot"), 2);
    assert!(healthy.load_backup("slot", None).is_ok());
}

#[test]
fn failed_recovery_leaves_a_corrupt_primary_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let healthy = store(dir.path());
    healthy.save("slot", n(1), None).unwrap();
    healthy.save("slot", n(2), None).unwrap();
    fs::write(primary(dir.path(), "slot"), b"torn write").unwrap();
    let broken = store_with(dir.path(), injected(PersistenceStage::Replace));
    expect_code(
        broken.recover_backup("slot", None),
        "persistence.replace.failed",
    );
    assert_eq!(
        fs::read(primary(dir.path(), "slot")).unwrap(),
        b"torn write"
    );
    healthy.recover_backup("slot", None).unwrap();
    assert_eq!(level(&healthy, "slot"), 1);
}

#[test]
fn a_torn_primary_is_detected_and_recoverable_from_the_backup() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.save("slot", n(1), None).unwrap();
    s.save("slot", n(2), None).unwrap();
    let whole = fs::read(primary(dir.path(), "slot")).unwrap();
    fs::write(primary(dir.path(), "slot"), &whole[..whole.len() / 2]).unwrap();
    expect_code(s.load("slot", None), "persistence.corrupt");
    s.recover_backup("slot", None).unwrap();
    assert_eq!(level(&s, "slot"), 1);
}

// ---- corrupt snapshots ----------------------------------------------------------------------------------------------------

fn corrupt_primary(bytes: &[u8], expected: &str) {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.save("slot", n(1), None).unwrap();
    fs::write(primary(dir.path(), "slot"), bytes).unwrap();
    expect_code(s.load("slot", None), expected);
    assert_eq!(
        fs::read(primary(dir.path(), "slot")).unwrap(),
        bytes,
        "loading must not modify the file"
    );
}

table! { corrupt_primary;
    empty_file_is_corrupt => (b"", "persistence.corrupt");
    truncated_object_is_corrupt => (b"{", "persistence.corrupt");
    null_document_is_corrupt => (b"null", "persistence.corrupt");
    array_document_is_corrupt => (b"[]", "persistence.corrupt");
    missing_schema_is_corrupt => (br#"{"content_version":1,"data":{}}"#, "persistence.corrupt");
    missing_content_version_is_corrupt => (br#"{"schema":1,"data":{}}"#, "persistence.corrupt");
    zero_schema_is_corrupt => (br#"{"schema":0,"content_version":1,"data":{}}"#, "persistence.corrupt");
    zero_content_version_is_corrupt => (br#"{"schema":1,"content_version":0,"data":{}}"#, "persistence.corrupt");
    string_schema_is_corrupt => (br#"{"schema":"1","content_version":1,"data":{}}"#, "persistence.corrupt");
    negative_schema_is_corrupt => (br#"{"schema":-1,"content_version":1,"data":{}}"#, "persistence.corrupt");
    schema_beyond_u32_is_corrupt => (br#"{"schema":4294967296,"content_version":1,"data":{}}"#, "persistence.corrupt");
    fractional_content_version_is_corrupt => (br#"{"schema":1,"content_version":1.5,"data":{}}"#, "persistence.corrupt");
    missing_data_is_corrupt => (br#"{"schema":1,"content_version":1}"#, "persistence.corrupt");
    array_data_is_corrupt => (br#"{"schema":1,"content_version":1,"data":[]}"#, "persistence.corrupt");
    null_data_is_corrupt => (br#"{"schema":1,"content_version":1,"data":null}"#, "persistence.corrupt");
    unknown_envelope_member_is_corrupt => (br#"{"schema":1,"content_version":1,"data":{},"extra":1}"#, "persistence.corrupt");
    binary_garbage_is_corrupt => (&[0xff, 0xfe, 0x00, 0x80], "persistence.corrupt");
    byte_order_mark_is_corrupt => (b"\xef\xbb\xbf{\"schema\":1,\"content_version\":1,\"data\":{}}", "persistence.corrupt");
    trailing_garbage_is_corrupt => (br#"{"schema":1,"content_version":1,"data":{}} x"#, "persistence.corrupt");
}

#[test]
fn a_minimal_valid_envelope_loads() {
    let dir = tempfile::tempdir().unwrap();
    write_envelope(&primary(dir.path(), "slot"), 1, 1, json!({}));
    assert!(store(dir.path()).load("slot", None).unwrap().is_empty());
}

fn newer_envelope(schema: u64, content: u64) {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    write_envelope(
        &primary(dir.path(), "slot"),
        schema,
        content,
        json!({"future": true}),
    );
    let before = fs::read(primary(dir.path(), "slot")).unwrap();
    expect_code(s.load("slot", None), "persistence.version.newer");
    expect_code(s.load("slot", Some(stamp)), "persistence.version.newer");
    expect_code(
        s.save("slot", n(1), Some(stamp)),
        "persistence.version.newer",
    );
    assert_eq!(fs::read(primary(dir.path(), "slot")).unwrap(), before);
}

table! { newer_envelope;
    newer_schema_alone_is_never_migrated => (2, 1);
    newer_content_alone_is_never_migrated => (1, 2);
    newer_schema_and_content_are_never_migrated => (9, 9);
    schema_at_u32_max_is_newer_not_corrupt => (4294967295, 1);
}

#[test]
fn newer_backups_are_refused_too() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    write_envelope(&backup(dir.path(), "slot"), 1, 2, json!({}));
    expect_code(
        s.load_backup("slot", Some(stamp)),
        "persistence.version.newer",
    );
}

#[test]
fn newer_data_is_not_validated_beyond_its_version() {
    let dir = tempfile::tempdir().unwrap();
    write_envelope(&primary(dir.path(), "slot"), 2, 1, json!([1, 2, 3]));
    expect_code(
        store(dir.path()).load("slot", None),
        "persistence.version.newer",
    );
}

#[test]
fn saving_over_a_corrupt_primary_is_refused_and_preserves_it() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.save("slot", n(1), None).unwrap();
    fs::write(primary(dir.path(), "slot"), b"corrupt").unwrap();
    expect_code(s.save("slot", n(2), None), "persistence.corrupt");
    assert_eq!(fs::read(primary(dir.path(), "slot")).unwrap(), b"corrupt");
}

#[test]
fn saving_with_a_corrupt_backup_is_refused_and_preserves_both_files() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.save("slot", n(1), None).unwrap();
    s.save("slot", n(2), None).unwrap();
    fs::write(backup(dir.path(), "slot"), b"corrupt").unwrap();
    let primary_bytes = fs::read(primary(dir.path(), "slot")).unwrap();
    expect_code(s.save("slot", n(3), None), "persistence.corrupt");
    assert_eq!(fs::read(backup(dir.path(), "slot")).unwrap(), b"corrupt");
    assert_eq!(
        fs::read(primary(dir.path(), "slot")).unwrap(),
        primary_bytes
    );
}

#[test]
fn loading_the_backup_of_a_corrupt_backup_reports_corruption() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(saves(dir.path())).unwrap();
    fs::write(backup(dir.path(), "slot"), b"{{").unwrap();
    expect_code(
        store(dir.path()).load_backup("slot", None),
        "persistence.corrupt",
    );
}

// ---- bounded JSON ------------------------------------------------------------------------------------------------------------

#[test]
fn save_over_the_byte_limit_is_refused_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let s = store_with(dir.path(), |o| o.max_bytes = 64);
    expect_code(
        s.save("slot", map(json!({"pad": "x".repeat(100)})), None),
        "persistence.size.exceeded",
    );
    assert!(!primary(dir.path(), "slot").exists());
}

#[test]
fn save_exactly_at_the_byte_limit_is_accepted() {
    let probe = tempfile::tempdir().unwrap();
    let data = map(json!({"k": "value"}));
    store(probe.path())
        .save("slot", data.clone(), None)
        .unwrap();
    let size = fs::metadata(primary(probe.path(), "slot")).unwrap().len();
    let dir = tempfile::tempdir().unwrap();
    store_with(dir.path(), |o| o.max_bytes = size)
        .save("slot", data.clone(), None)
        .unwrap();
    let tight = tempfile::tempdir().unwrap();
    expect_code(
        store_with(tight.path(), |o| o.max_bytes = size - 1).save("slot", data, None),
        "persistence.size.exceeded",
    );
}

#[test]
fn oversized_files_on_disk_are_refused_before_parsing() {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path()).save("slot", n(1), None).unwrap();
    let small = store_with(dir.path(), |o| o.max_bytes = 10);
    expect_code(small.load("slot", None), "project.file.too_large");
}

#[test]
fn a_failed_oversize_save_keeps_the_previous_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let s = store_with(dir.path(), |o| o.max_bytes = 120);
    s.save("slot", n(1), None).unwrap();
    assert!(
        s.save("slot", map(json!({"pad": "x".repeat(200)})), None)
            .is_err()
    );
    assert_eq!(level(&s, "slot"), 1);
}

fn depth_save(max_depth: usize, data: Value, ok: bool) {
    let dir = tempfile::tempdir().unwrap();
    let s = store_with(dir.path(), |o| o.max_depth = max_depth);
    let result = s.save("slot", map(data), None);
    if ok {
        result.unwrap();
        assert!(s.load("slot", None).is_ok());
    } else {
        expect_code(result, "persistence.depth.exceeded");
        assert!(!primary(dir.path(), "slot").exists());
    }
}

table! { depth_save;
    depth_three_allows_scalars_two_levels_down => (3, json!({"a": {"b": 1}}), true);
    depth_three_allows_empty_containers_at_the_limit => (3, json!({"a": {"b": {}}}), true);
    depth_three_refuses_scalars_below_the_limit => (3, json!({"a": {"b": {"c": 1}}}), false);
    depth_counts_arrays_like_objects => (3, json!({"a": [[1]]}), false);
    depth_one_allows_empty_data => (1, json!({}), true);
    depth_one_refuses_any_member_inside_data => (1, json!({"a": 1}), false);
    depth_default_allows_thirty_levels => (32, serde_json::from_str::<Value>(&format!("{}1{}", "{\"k\":".repeat(29), "}".repeat(29))).unwrap(), true);
    depth_default_refuses_forty_levels => (32, serde_json::from_str::<Value>(&format!("{}1{}", "{\"k\":".repeat(40), "}".repeat(40))).unwrap(), false);
}

#[test]
fn deeply_nested_files_are_refused_by_the_depth_limit() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let nested = format!("{}1{}", "[".repeat(50), "]".repeat(50));
    fs::create_dir_all(saves(dir.path())).unwrap();
    fs::write(
        primary(dir.path(), "slot"),
        format!(r#"{{"schema":1,"content_version":1,"data":{{"x":{nested}}}}}"#),
    )
    .unwrap();
    expect_code(s.load("slot", None), "persistence.depth.exceeded");
}

#[test]
fn pathologically_nested_files_are_refused_before_recursive_parsing() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let nested = format!("{}1{}", "[".repeat(5000), "]".repeat(5000));
    fs::create_dir_all(saves(dir.path())).unwrap();
    fs::write(
        primary(dir.path(), "slot"),
        format!(r#"{{"schema":1,"content_version":1,"data":{{"x":{nested}}}}}"#),
    )
    .unwrap();
    expect_code(s.load("slot", None), "persistence.depth.exceeded");
}

#[test]
fn brackets_inside_strings_do_not_count_as_nesting() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let data = map(
        json!({"text": "[".repeat(300) + &"{".repeat(300), "quoted": "\"[[[[[[\" and \\\\ [[[["}),
    );
    s.save("slot", data.clone(), None).unwrap();
    assert_eq!(s.load("slot", None).unwrap(), data);
}

#[test]
fn deep_unrecognised_future_files_are_not_misreported_as_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(saves(dir.path())).unwrap();
    let nested = format!("{}0{}", "[".repeat(140), "]".repeat(140));
    fs::write(
        primary(dir.path(), "slot"),
        format!(r#"{{"schema":2,"content_version":1,"data":{nested}}}"#),
    )
    .unwrap();
    expect_code(
        store(dir.path()).load("slot", None),
        "persistence.depth.exceeded",
    );
}

// ---- migration ---------------------------------------------------------------------------------------------------------------------

fn old_version(schema: u64, content: u64) {
    let dir = tempfile::tempdir().unwrap();
    let s = newest(dir.path());
    write_envelope(
        &primary(dir.path(), "slot"),
        schema,
        content,
        json!({"hp": 7, "name": "Ada"}),
    );
    let loaded = s.load("slot", Some(chain)).unwrap();
    assert_eq!(loaded["origin"], format!("{schema}.{content}"));
    assert_eq!(loaded["name"], "Ada");
    if schema < 2 {
        assert_eq!(loaded["health"], 7);
        assert!(!loaded.contains_key("hp"));
    } else {
        assert_eq!(loaded["hp"], 7);
        assert!(!loaded.contains_key("health"));
    }
    assert_eq!(loaded.contains_key("slots"), schema < 3);
    assert_eq!(loaded.get("flag_a"), (content < 2).then_some(&json!(false)));
    assert_eq!(loaded.get("flag_b"), (content < 3).then_some(&json!(true)));
    assert_eq!(
        s.load("slot", None)
            .err()
            .map(|e| e.diagnostics[0].code.clone()),
        Some("persistence.migration.required".into())
    );
}

table! { old_version;
    migrates_from_schema_1_content_1 => (1, 1);
    migrates_from_schema_1_content_2 => (1, 2);
    migrates_from_schema_1_content_3 => (1, 3);
    migrates_from_schema_2_content_1 => (2, 1);
    migrates_from_schema_2_content_2 => (2, 2);
    migrates_from_schema_2_content_3 => (2, 3);
    migrates_from_schema_3_content_1 => (3, 1);
    migrates_from_schema_3_content_2 => (3, 2);
}

#[test]
fn migration_receives_the_stored_versions() {
    let dir = tempfile::tempdir().unwrap();
    write_envelope(&primary(dir.path(), "slot"), 2, 5, json!({}));
    let s = store_with(dir.path(), |o| {
        o.schema_version = 3;
        o.content_version = 6;
    });
    assert_eq!(
        s.load("slot", Some(stamp)).unwrap()["migrated_from"],
        json!([2, 5])
    );
}

#[test]
fn current_version_files_never_invoke_the_migration() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.save("slot", n(1), None).unwrap();
    assert_eq!(s.load("slot", Some(failing)).unwrap()["n"], 1);
}

#[test]
fn older_files_without_a_migration_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    write_envelope(&primary(dir.path(), "slot"), 1, 1, json!({}));
    expect_code(
        newest(dir.path()).load("slot", None),
        "persistence.migration.required",
    );
}

#[test]
fn loading_with_a_migration_does_not_rewrite_the_file() {
    let dir = tempfile::tempdir().unwrap();
    write_envelope(&primary(dir.path(), "slot"), 1, 1, json!({"hp": 3}));
    let before = fs::read(primary(dir.path(), "slot")).unwrap();
    newest(dir.path()).load("slot", Some(chain)).unwrap();
    assert_eq!(fs::read(primary(dir.path(), "slot")).unwrap(), before);
}

#[test]
fn saving_after_migration_upgrades_the_primary_and_keeps_the_old_file_as_backup() {
    let dir = tempfile::tempdir().unwrap();
    write_envelope(&primary(dir.path(), "slot"), 1, 1, json!({"hp": 3}));
    let old_bytes = fs::read(primary(dir.path(), "slot")).unwrap();
    let s = newest(dir.path());
    let migrated = s.load("slot", Some(chain)).unwrap();
    s.save("slot", migrated, Some(chain)).unwrap();
    assert_eq!(fs::read(backup(dir.path(), "slot")).unwrap(), old_bytes);
    assert_eq!(s.load("slot", None).unwrap()["health"], 3);
    let value: Value =
        serde_json::from_slice(&fs::read(primary(dir.path(), "slot")).unwrap()).unwrap();
    assert_eq!(
        (value["schema"].as_u64(), value["content_version"].as_u64()),
        (Some(3), Some(3))
    );
}

#[test]
fn the_old_backup_needs_a_migration_to_be_read() {
    let dir = tempfile::tempdir().unwrap();
    write_envelope(&primary(dir.path(), "slot"), 1, 1, json!({"hp": 3}));
    let s = newest(dir.path());
    s.save("slot", n(1), Some(chain)).unwrap();
    expect_code(
        s.load_backup("slot", None),
        "persistence.migration.required",
    );
    assert_eq!(s.load_backup("slot", Some(chain)).unwrap()["health"], 3);
}

#[test]
fn saving_over_an_old_primary_requires_a_migration() {
    let dir = tempfile::tempdir().unwrap();
    write_envelope(&primary(dir.path(), "slot"), 1, 1, json!({"hp": 3}));
    let before = fs::read(primary(dir.path(), "slot")).unwrap();
    expect_code(
        newest(dir.path()).save("slot", n(1), None),
        "persistence.migration.required",
    );
    assert_eq!(fs::read(primary(dir.path(), "slot")).unwrap(), before);
}

#[test]
fn a_failing_migration_blocks_the_save_and_preserves_the_file() {
    let dir = tempfile::tempdir().unwrap();
    write_envelope(&primary(dir.path(), "slot"), 1, 1, json!({"hp": 3}));
    let before = fs::read(primary(dir.path(), "slot")).unwrap();
    expect_code(
        newest(dir.path()).save("slot", n(1), Some(failing)),
        "fixture.migrate",
    );
    assert_eq!(fs::read(primary(dir.path(), "slot")).unwrap(), before);
}

#[test]
fn migration_errors_propagate_with_their_own_code() {
    let dir = tempfile::tempdir().unwrap();
    write_envelope(&primary(dir.path(), "slot"), 1, 1, json!({}));
    expect_code(
        newest(dir.path()).load("slot", Some(failing)),
        "fixture.migrate",
    );
}

#[test]
fn migration_panics_are_contained() {
    let dir = tempfile::tempdir().unwrap();
    write_envelope(&primary(dir.path(), "slot"), 1, 1, json!({}));
    expect_code(
        newest(dir.path()).load("slot", Some(panicking)),
        "persistence.migration.failed",
    );
}

#[test]
fn migration_output_is_subject_to_the_depth_limit() {
    let dir = tempfile::tempdir().unwrap();
    write_envelope(&primary(dir.path(), "slot"), 1, 1, json!({}));
    expect_code(
        newest(dir.path()).load("slot", Some(too_deep)),
        "persistence.depth.exceeded",
    );
}

#[test]
fn migration_output_is_subject_to_the_byte_limit() {
    let dir = tempfile::tempdir().unwrap();
    write_envelope(&primary(dir.path(), "slot"), 1, 1, json!({}));
    let s = store_with(dir.path(), |o| {
        o.schema_version = 3;
        o.content_version = 3;
        o.max_bytes = 1024;
    });
    expect_code(
        s.load("slot", Some(too_big)),
        "persistence.migration.invalid",
    );
}

#[test]
fn settings_migrate_like_saves() {
    let dir = tempfile::tempdir().unwrap();
    write_envelope(
        &dir.path().join("game/settings/preferences.json"),
        1,
        1,
        json!({"hp": 1}),
    );
    let s = newest(dir.path());
    assert_eq!(s.load_settings(Some(chain)).unwrap()["origin"], "1.1");
    expect_code(s.load_settings(None), "persistence.migration.required");
}

// ---- recovery -----------------------------------------------------------------------------------------------------------------------

fn two_generations(dir: &Path) -> PersistenceStore {
    let s = store(dir);
    s.save("slot", n(1), None).unwrap();
    s.save("slot", n(2), None).unwrap();
    s
}

#[test]
fn recovery_restores_a_missing_primary_from_the_backup() {
    let dir = tempfile::tempdir().unwrap();
    let s = two_generations(dir.path());
    fs::remove_file(primary(dir.path(), "slot")).unwrap();
    expect_code(s.load("slot", None), "persistence.missing");
    s.recover_backup("slot", None).unwrap();
    assert_eq!(level(&s, "slot"), 1);
}

#[test]
fn recovery_restores_a_corrupt_primary_from_the_backup() {
    let dir = tempfile::tempdir().unwrap();
    let s = two_generations(dir.path());
    fs::write(primary(dir.path(), "slot"), b"\0\0\0").unwrap();
    s.recover_backup("slot", None).unwrap();
    assert_eq!(level(&s, "slot"), 1);
}

#[test]
fn recovery_of_a_healthy_primary_is_an_explicit_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let s = two_generations(dir.path());
    s.recover_backup("slot", None).unwrap();
    assert_eq!(level(&s, "slot"), 1);
}

#[test]
fn recovery_leaves_the_backup_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let s = two_generations(dir.path());
    let before = fs::read(backup(dir.path(), "slot")).unwrap();
    s.recover_backup("slot", None).unwrap();
    assert_eq!(fs::read(backup(dir.path(), "slot")).unwrap(), before);
}

#[test]
fn recovery_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let s = two_generations(dir.path());
    s.recover_backup("slot", None).unwrap();
    let once = fs::read(primary(dir.path(), "slot")).unwrap();
    s.recover_backup("slot", None).unwrap();
    assert_eq!(fs::read(primary(dir.path(), "slot")).unwrap(), once);
}

#[test]
fn recovered_primaries_use_the_current_envelope_version() {
    let dir = tempfile::tempdir().unwrap();
    write_envelope(&backup(dir.path(), "slot"), 1, 1, json!({"hp": 4}));
    let s = newest(dir.path());
    s.recover_backup("slot", Some(chain)).unwrap();
    let value: Value =
        serde_json::from_slice(&fs::read(primary(dir.path(), "slot")).unwrap()).unwrap();
    assert_eq!(
        (value["schema"].as_u64(), value["content_version"].as_u64()),
        (Some(3), Some(3))
    );
    assert_eq!(value["data"]["health"], 4);
}

#[test]
fn recovering_an_old_backup_without_a_migration_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    write_envelope(&backup(dir.path(), "slot"), 1, 1, json!({}));
    fs::write(primary(dir.path(), "slot"), b"corrupt").unwrap();
    expect_code(
        newest(dir.path()).recover_backup("slot", None),
        "persistence.migration.required",
    );
    assert_eq!(fs::read(primary(dir.path(), "slot")).unwrap(), b"corrupt");
}

#[test]
fn recovery_without_a_backup_reports_missing_and_keeps_the_primary() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.save("slot", n(1), None).unwrap();
    fs::write(primary(dir.path(), "slot"), b"corrupt").unwrap();
    expect_code(s.recover_backup("slot", None), "persistence.missing");
    assert_eq!(fs::read(primary(dir.path(), "slot")).unwrap(), b"corrupt");
}

#[test]
fn recovery_from_a_corrupt_backup_keeps_the_corrupt_primary() {
    let dir = tempfile::tempdir().unwrap();
    let s = two_generations(dir.path());
    fs::write(backup(dir.path(), "slot"), b"bad backup").unwrap();
    fs::write(primary(dir.path(), "slot"), b"bad primary").unwrap();
    expect_code(s.recover_backup("slot", None), "persistence.corrupt");
    assert_eq!(
        fs::read(primary(dir.path(), "slot")).unwrap(),
        b"bad primary"
    );
}

#[test]
fn recovery_refuses_a_newer_backup() {
    let dir = tempfile::tempdir().unwrap();
    write_envelope(&backup(dir.path(), "slot"), 1, 2, json!({}));
    fs::write(primary(dir.path(), "slot"), b"corrupt").unwrap();
    expect_code(
        store(dir.path()).recover_backup("slot", Some(stamp)),
        "persistence.version.newer",
    );
    assert_eq!(fs::read(primary(dir.path(), "slot")).unwrap(), b"corrupt");
}

#[test]
fn recovery_never_overwrites_an_old_primary_that_needs_migration() {
    let dir = tempfile::tempdir().unwrap();
    let s = newest(dir.path());
    s.save("slot", n(1), Some(chain)).unwrap();
    s.save("slot", n(2), Some(chain)).unwrap();
    write_envelope(&primary(dir.path(), "slot"), 1, 1, json!({"hp": 9}));
    let before = fs::read(primary(dir.path(), "slot")).unwrap();
    expect_code(
        s.recover_backup("slot", None),
        "persistence.migration.required",
    );
    assert_eq!(fs::read(primary(dir.path(), "slot")).unwrap(), before);
}

#[test]
fn recovery_refuses_a_newer_primary() {
    let dir = tempfile::tempdir().unwrap();
    let s = two_generations(dir.path());
    write_envelope(&primary(dir.path(), "slot"), 5, 5, json!({}));
    let before = fs::read(primary(dir.path(), "slot")).unwrap();
    expect_code(s.recover_backup("slot", None), "persistence.version.newer");
    assert_eq!(fs::read(primary(dir.path(), "slot")).unwrap(), before);
}

#[test]
fn recovery_respects_the_byte_limit_for_the_backup() {
    let dir = tempfile::tempdir().unwrap();
    two_generations(dir.path());
    fs::write(primary(dir.path(), "slot"), b"torn").unwrap();
    let tiny = store_with(dir.path(), |o| o.max_bytes = 20);
    expect_code(tiny.recover_backup("slot", None), "project.file.too_large");
}

#[test]
fn recovery_checks_the_slot_name() {
    let dir = tempfile::tempdir().unwrap();
    expect_code(
        store(dir.path()).recover_backup("../x", None),
        "persistence.slot.invalid",
    );
}

#[test]
fn saving_after_recovery_resumes_normal_generations() {
    let dir = tempfile::tempdir().unwrap();
    let s = two_generations(dir.path());
    fs::write(primary(dir.path(), "slot"), b"torn").unwrap();
    s.recover_backup("slot", None).unwrap();
    s.save("slot", n(3), None).unwrap();
    assert_eq!(level(&s, "slot"), 3);
    assert_eq!(s.load_backup("slot", None).unwrap()["n"], 1);
}

// ---- slot locks -----------------------------------------------------------------------------------------------------------------------------

fn lock_file(path: &Path) -> fs::File {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .unwrap();
    fs2::FileExt::try_lock_exclusive(&file).unwrap();
    file
}

fn busy_op(op: fn(&PersistenceStore) -> Result<()>) {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.save("slot", n(1), None).unwrap();
    let held = lock_file(&saves(dir.path()).join("slot.json.lock"));
    expect_code(op(&s), "persistence.lock.busy");
    fs2::FileExt::unlock(&held).unwrap();
    op(&s).unwrap();
}

table! { busy_op;
    a_held_lock_blocks_saving => (|s| s.save("slot", n(2), None));
    a_held_lock_blocks_loading => (|s| s.load("slot", None).map(|_| ()));
    a_held_lock_blocks_backup_loading => (|s| { let _ = s.save("other", n(1), None); s.save("slot", n(3), None).map(|_| ()).and_then(|_| s.load_backup("slot", None)).map(|_| ()) });
    a_held_lock_blocks_recovery => (|s| s.recover_backup("slot", None).or_else(|e| if e.diagnostics[0].code == "persistence.missing" { Ok(()) } else { Err(e) }));
}

#[test]
fn locks_are_per_slot() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.save("a", n(1), None).unwrap();
    let _held = lock_file(&saves(dir.path()).join("a.json.lock"));
    s.save("b", n(2), None).unwrap();
    assert_eq!(level(&s, "b"), 2);
}

#[test]
fn settings_have_their_own_lock() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.save_settings(n(1), None).unwrap();
    let held = lock_file(&dir.path().join("game/settings/preferences.json.lock"));
    expect_code(s.save_settings(n(2), None), "persistence.lock.busy");
    s.save("preferences", n(3), None).unwrap();
    fs2::FileExt::unlock(&held).unwrap();
}

// ---- links ------------------------------------------------------------------------------------------------------------------------------------

#[cfg(target_os = "macos")]
#[test]
fn macos_system_temporary_aliases_allow_new_roots_and_round_trip_saves() {
    for parent in ["/tmp", "/var/tmp"] {
        let dir = tempfile::tempdir_in(parent).unwrap();
        let root = dir.path().join("new/player-data");
        let s = store(&root);
        s.save("slot", n(7), None).unwrap();
        s.save_settings(n(9), None).unwrap();
        assert_eq!(level(&s, "slot"), 7);
        assert_eq!(s.load_settings(None).unwrap(), n(9));
        assert!(primary(&root, "slot").is_file());
    }
}

#[test]
fn an_arbitrary_symlinked_root_ancestor_is_refused_before_creating_directories() {
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let link = dir.path().join("ancestor-link");
    if !try_symlink_dir(elsewhere.path(), &link) {
        eprintln!("symlink creation unavailable; test skipped");
        return;
    }
    let s = store(&link.join("new/player-data"));
    expect_code(s.save("slot", n(1), None), "persistence.path.link");
    assert!(fs::read_dir(elsewhere.path()).unwrap().next().is_none());
}

#[test]
fn a_symlinked_saves_directory_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("game")).unwrap();
    if !try_symlink_dir(elsewhere.path(), &saves(dir.path())) {
        eprintln!("symlink creation unavailable; test skipped");
        return;
    }
    expect_code(
        store(dir.path()).save("slot", n(1), None),
        "persistence.path.link",
    );
    assert!(fs::read_dir(elsewhere.path()).unwrap().next().is_none());
}

#[test]
fn a_symlinked_slot_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let target = elsewhere.path().join("real.json");
    fs::write(&target, b"{}").unwrap();
    fs::create_dir_all(saves(dir.path())).unwrap();
    if !try_symlink_file(&target, &primary(dir.path(), "slot")) {
        eprintln!("symlink creation unavailable; test skipped");
        return;
    }
    expect_code(
        store(dir.path()).load("slot", None),
        "persistence.path.link",
    );
    assert_eq!(fs::read(&target).unwrap(), b"{}");
}

#[test]
fn a_symlinked_backup_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let target = elsewhere.path().join("real.json");
    fs::write(&target, b"{}").unwrap();
    fs::create_dir_all(saves(dir.path())).unwrap();
    if !try_symlink_file(&target, &backup(dir.path(), "slot")) {
        eprintln!("symlink creation unavailable; test skipped");
        return;
    }
    expect_code(
        store(dir.path()).save("slot", n(1), None),
        "persistence.path.link",
    );
}

#[test]
fn a_symlinked_root_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let link = dir.path().join("root-link");
    if !try_symlink_dir(elsewhere.path(), &link) {
        eprintln!("symlink creation unavailable; test skipped");
        return;
    }
    let s = PersistenceStore::new(PersistenceOptions::new(link, "game".into())).unwrap();
    expect_code(s.save("slot", n(1), None), "persistence.path.link");
}
