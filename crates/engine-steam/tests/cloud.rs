use engine_project::persistence::{PersistenceOptions, PersistenceStore};
use engine_steam::*;
use serde_json::{Map, Value, json};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn store(dir: &Path, content_version: u32) -> Arc<PersistenceStore> {
    let mut options = PersistenceOptions::new(dir.to_owned(), "game".into());
    options.content_version = content_version;
    Arc::new(PersistenceStore::new(options).unwrap())
}

fn data(n: i64) -> Map<String, Value> {
    json!({ "n": n }).as_object().unwrap().clone()
}

fn bytes(content_version: u32, n: i64) -> Vec<u8> {
    serde_json::to_vec(&json!({"schema": 1, "content_version": content_version, "data": {"n": n}}))
        .unwrap()
}

fn ready() -> FakeBackend {
    FakeBackend::ready(AppId(480), "tester")
}

fn set_local_time(store: &PersistenceStore, slot: &str, seconds: u64) {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(store.slot_path(slot).unwrap())
        .unwrap();
    file.set_modified(UNIX_EPOCH + Duration::from_secs(seconds))
        .unwrap();
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[test]
fn startup_pull_installs_a_cloud_only_slot() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), 1);
    let mut fake = ready().with_cloud_file("saves/a.json", &bytes(1, 7));
    let report = CloudSync::new(store.clone()).sync_all(&mut fake);
    assert_eq!(report.slots, [("a".to_owned(), SyncAction::Pulled)]);
    assert_eq!(store.load("a", None).unwrap(), data(7));
}

#[test]
fn local_only_slots_are_pushed() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), 1);
    store.save("a", data(3), None).unwrap();
    let mut fake = ready();
    let report = CloudSync::new(store.clone()).sync_all(&mut fake);
    assert_eq!(report.slots, [("a".to_owned(), SyncAction::Pushed)]);
    assert_eq!(
        fake.cloud_file("saves/a.json").unwrap(),
        std::fs::read(store.slot_path("a").unwrap()).unwrap()
    );
    // A second sync finds both sides equal.
    let again = CloudSync::new(store).sync_all(&mut fake);
    assert_eq!(again.slots, [("a".to_owned(), SyncAction::Unchanged)]);
}

#[test]
fn higher_content_version_wins_in_either_direction() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), 2);
    store.save("a", data(1), None).unwrap(); // content_version 2 locally
    let mut fake = ready().with_cloud_file("saves/a.json", &bytes(1, 99));
    let report = CloudSync::new(store.clone()).sync_all(&mut fake);
    assert_eq!(
        report.slots[0].1,
        SyncAction::Pushed,
        "local v2 beats cloud v1"
    );
    assert_eq!(
        fake.cloud_file("saves/a.json").unwrap(),
        std::fs::read(store.slot_path("a").unwrap()).unwrap()
    );

    let dir = tempfile::tempdir().unwrap();
    let low = {
        let mut options = PersistenceOptions::new(dir.path().to_owned(), "game".into());
        options.content_version = 2;
        Arc::new(PersistenceStore::new(options).unwrap())
    };
    std::fs::create_dir_all(low.slot_path("a").unwrap().parent().unwrap()).unwrap();
    std::fs::write(low.slot_path("a").unwrap(), bytes(1, 5)).unwrap();
    let mut fake = ready().with_cloud_file("saves/a.json", &bytes(2, 6));
    let report = CloudSync::new(low.clone()).sync_all(&mut fake);
    assert_eq!(
        report.slots[0].1,
        SyncAction::Pulled,
        "cloud v2 beats local v1"
    );
    assert_eq!(low.load("a", None).unwrap(), data(6));
}

#[test]
fn equal_versions_fall_back_to_the_newer_modification_time() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), 1);
    store.save("a", data(1), None).unwrap();
    set_local_time(&store, "a", 5_000);
    let mut fake = ready().with_cloud_file_at("saves/a.json", &bytes(1, 2), 9_000);
    let report = CloudSync::new(store.clone()).sync_all(&mut fake);
    assert_eq!(report.slots[0].1, SyncAction::Pulled, "cloud is newer");
    assert_eq!(store.load("a", None).unwrap(), data(2));

    set_local_time(&store, "a", 20_000);
    let report = CloudSync::new(store.clone()).sync_all(&mut fake);
    // The pulled bytes equal the cloud bytes, so nothing is left to do.
    assert_eq!(report.slots[0].1, SyncAction::Unchanged);
    store.save("a", data(3), None).unwrap();
    set_local_time(&store, "a", 30_000);
    let local_bytes = std::fs::read(store.slot_path("a").unwrap()).unwrap();
    let report = CloudSync::new(store).sync_all(&mut fake);
    assert_eq!(report.slots[0].1, SyncAction::Pushed, "local is newer");
    assert_eq!(fake.cloud_file("saves/a.json").unwrap(), local_bytes);
}

#[test]
fn a_full_tie_keeps_local_and_raises_cloud_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), 1);
    store.save("a", data(1), None).unwrap();
    set_local_time(&store, "a", 7_000);
    let mut fake = ready().with_cloud_file_at("saves/a.json", &bytes(1, 2), 7_000);
    let report = CloudSync::new(store.clone()).sync_all(&mut fake);
    assert_eq!(report.slots[0].1, SyncAction::Conflict);
    assert_eq!(
        report.events,
        [SteamEvent::CloudConflict { name: "a".into() }]
    );
    assert_eq!(store.load("a", None).unwrap(), data(1));
    assert_eq!(fake.cloud_file("saves/a.json").unwrap(), bytes(1, 2));
}

#[test]
fn unsafe_names_are_rejected_both_ways() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), 1);
    let sync = CloudSync::new(store.clone());
    let mut fake = ready()
        .with_cloud_file("saves/../x.json", &bytes(1, 1))
        .with_cloud_file("../x", &bytes(1, 1))
        .with_cloud_file("saves/a.b.json", &bytes(1, 1));
    let report = sync.sync_all(&mut fake);
    assert!(report.slots.is_empty() && report.failures.is_empty());
    assert_eq!(
        sync.push_slot(&mut fake, "../x"),
        Err(SteamError::InvalidName(
            "Slot must be a bounded safe identifier".into()
        ))
    );
    assert_eq!(fake.call_count("cloud_write"), 0);
    assert_eq!(
        fake.cloud_read("../x"),
        Err(SteamError::InvalidName("../x".into()))
    );
}

#[test]
fn corrupt_or_newer_cloud_copies_never_overwrite_local() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), 1);
    store.save("a", data(1), None).unwrap();
    set_local_time(&store, "a", 1_000);
    let mut fake = ready()
        .with_cloud_file_at("saves/a.json", b"garbage", 9_000)
        .with_cloud_file_at("saves/b.json", &bytes(5, 1), 9_000);
    let report = CloudSync::new(store.clone()).sync_all(&mut fake);
    assert_eq!(report.failures.len(), 2);
    assert!(
        report
            .events
            .iter()
            .all(|e| matches!(e, SteamEvent::CloudSyncFailed { .. }))
    );
    assert_eq!(store.load("a", None).unwrap(), data(1));
    assert!(store.slot_header("b").unwrap().is_none());
}

#[test]
fn quota_exceeded_is_reported_and_local_is_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), 1);
    store.save("a", data(1), None).unwrap();
    let mut fake = ready().with_cloud_quota(4);
    let sync = CloudSync::new(store.clone());
    assert_eq!(
        sync.push_slot(&mut fake, "a"),
        Err(SteamError::QuotaExceeded)
    );
    let report = sync.sync_all(&mut fake);
    assert_eq!(report.failures.len(), 1);
    assert!(report.failures[0].1.contains("quota"));
    assert_eq!(store.load("a", None).unwrap(), data(1));
}

#[test]
fn cloud_disabled_for_app_or_account_degrades_to_local_only() {
    for fake in [
        ready().with_cloud_enabled(false),
        ready().with_cloud_account_enabled(false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path(), 1);
        store.save("a", data(1), None).unwrap();
        let mut fake = fake.with_cloud_file("saves/z.json", &bytes(1, 1));
        let sync = CloudSync::new(store.clone());
        let report = sync.sync_all(&mut fake);
        assert!(report.local_only && report.slots.is_empty());
        assert_eq!(
            sync.push_slot(&mut fake, "a"),
            Err(SteamError::CloudDisabled)
        );
        assert_eq!(fake.call_count("cloud_write"), 0);
        assert!(store.slot_header("z").unwrap().is_none());
        assert!(now() > 0);
    }
}

#[test]
fn without_steam_the_sync_is_local_only() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), 1);
    let mut null = NullBackend::not_compiled();
    assert!(CloudSync::new(store).sync_all(&mut null).local_only);
}

#[test]
fn a_corrupt_local_slot_is_healed_from_a_valid_cloud_copy() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), 1);
    store.save("a", data(1), None).unwrap();
    let path = store.slot_path("a").unwrap();
    std::fs::write(&path, b"{ not json").unwrap();
    let mut fake = ready().with_cloud_file("saves/a.json", &bytes(1, 7));
    let report = CloudSync::new(store.clone()).sync_all(&mut fake);
    assert_eq!(report.slots, vec![("a".to_owned(), SyncAction::Pulled)]);
    assert!(report.failures.is_empty());
    assert_eq!(store.load("a", None).unwrap(), data(7));
    // The damaged file is kept as the backup.
    assert_eq!(
        std::fs::read(path.with_extension("json.bak")).unwrap(),
        b"{ not json"
    );
}

#[test]
fn a_corrupt_local_slot_is_never_uploaded() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path(), 1);
    store.save("a", data(1), None).unwrap();
    std::fs::write(store.slot_path("a").unwrap(), b"{ not json").unwrap();
    let mut fake = ready();
    let report = CloudSync::new(store).sync_all(&mut fake);
    assert_eq!(report.failures.len(), 1);
    assert!(fake.cloud_file("saves/a.json").is_none());
}
