//! Installed Workshop items mounted as read-only content roots, through the fake backend
//! and the engine's `services::resolve_asset`. Nothing here touches the real Steam.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bevy_ecs::prelude::World;
use engine_project::Project;
use engine_project::runtime::ProjectResource;
use engine_project::services::resolve_asset;
use engine_steam::*;

struct Shared {
    fake: Arc<Mutex<FakeBackend>>,
}

impl SteamBackend for Shared {
    fn status(&self) -> SteamStatus {
        self.fake.lock().unwrap().status()
    }
    common::forward_ugc!();
}

const GOOD: WorkshopItemId = WorkshopItemId(12345);
const BAD: WorkshopItemId = WorkshopItemId(777);
const OTHER: SteamId = SteamId(76_561_198_000_000_002);

fn catalog_item(id: WorkshopItemId) -> WorkshopItem {
    WorkshopItem {
        id,
        title: format!("Item {}", id.0),
        description: String::new(),
        owner: OTHER,
        visibility: ItemVisibility::Public,
        tags: Vec::new(),
        time_created: 1,
        time_updated: 1,
        file_size: 10,
        votes_up: 0,
        votes_down: 0,
        subscriptions: 0,
        banned: false,
    }
}

/// An item folder as Steam installs it: `<content>/<id>`.
fn write_item(content: &Path, id: WorkshopItemId, assets: &str) -> PathBuf {
    let dir = content.join(id.0.to_string());
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("tex.ppm"), "P3\n1 1\n255\n255 255 255\n").unwrap();
    fs::write(dir.join("tex2.ppm"), "P3\n1 1\n255\n0 0 0\n").unwrap();
    fs::write(
        dir.join("content.json"),
        format!(r#"{{"schema":1,"assets":{assets}}}"#),
    )
    .unwrap();
    dir
}

fn installed(id: WorkshopItemId, path: PathBuf) -> InstalledItem {
    InstalledItem {
        id,
        path,
        size_bytes: 100,
        timestamp: 1,
        needs_update: false,
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    content: PathBuf,
    fake: Arc<Mutex<FakeBackend>>,
    session: SteamSession,
    service: UgcService,
    mods: WorkshopContent,
    project: Project,
    world: World,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let content = dir.path().join("workshop/content/480");
        fs::create_dir_all(&content).unwrap();
        let good = write_item(
            &content,
            GOOD,
            r#"[{"id":"tex","kind":"texture","path":"tex.ppm"}]"#,
        );
        // An item that tries to ship a script: refused as a whole.
        let bad = write_item(
            &content,
            BAD,
            r#"[{"id":"s","kind":"script","path":"tex.ppm"}]"#,
        );
        let project_dir = dir.path().join("project");
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(
            project_dir.join("project.json"),
            r#"{"schema":1,"name":"t","startup_scene":"scene.json","assets":[]}"#,
        )
        .unwrap();
        fs::write(
            project_dir.join("scene.json"),
            r#"{"schema":1,"scene_id":"room","entities":[]}"#,
        )
        .unwrap();
        let project = Project::load(project_dir.join("project.json")).unwrap();
        let fake = Arc::new(Mutex::new(
            FakeBackend::ready(AppId(480), "tester")
                .with_workshop_item(catalog_item(GOOD))
                .with_workshop_item(catalog_item(BAD))
                .with_installed_item(installed(GOOD, good))
                .with_installed_item(installed(BAD, bad))
                .with_download_polls(1),
        ));
        let session = SteamSession::new(Box::new(Shared { fake: fake.clone() }));
        let service = UgcService::new(session.clone()).unwrap();
        let mods = WorkshopContent::new(project.content_roots.clone());
        let mut world = World::new();
        world.insert_resource(ProjectResource(Arc::new(project.clone())));
        Self {
            _dir: dir,
            content,
            fake,
            session,
            service,
            mods,
            project,
            world,
        }
    }

    /// Polls until `done` matches, feeding every poll to the content glue.
    fn pump(&mut self, done: impl Fn(&UgcEvent) -> bool) -> Vec<WorkshopContentEvent> {
        let mut out = Vec::new();
        for _ in 0..20 {
            let events = self.service.poll();
            out.extend(self.mods.process(&self.service, &events));
            if events.iter().any(&done) {
                return out;
            }
        }
        panic!("timed out");
    }

    fn subscribe(&mut self, item: WorkshopItemId) -> Vec<WorkshopContentEvent> {
        self.service.subscribe(item).unwrap();
        self.pump(|e| matches!(e, UgcEvent::Reply(UgcRaw::Subscription { .. })))
    }

    fn download(&mut self, item: WorkshopItemId) -> Vec<WorkshopContentEvent> {
        self.service.download_item(item, false).unwrap();
        self.pump(|e| matches!(e, UgcEvent::Reply(UgcRaw::Downloaded { .. })))
    }

    fn resolve(&self, item: WorkshopItemId, id: &str) -> engine_project::Result<PathBuf> {
        resolve_asset(&self.world, &WorkshopContent::asset_id(item, id))
    }
}

#[test]
fn an_installed_item_mounts_and_its_asset_resolves_through_the_runtime_service() {
    let mut rig = Rig::new();
    assert!(
        rig.subscribe(GOOD).is_empty(),
        "subscribing alone mounts nothing"
    );
    assert!(rig.resolve(GOOD, "tex").is_err());

    let events = rig.download(GOOD);
    assert!(matches!(
        events.as_slice(),
        [WorkshopContentEvent::Mounted { item, .. }] if *item == GOOD
    ));
    let file = rig.resolve(GOOD, "tex").unwrap();
    assert_eq!(
        file,
        fs::canonicalize(rig.content.join("12345/tex.ppm")).unwrap()
    );
    // The project's own lookup agrees (shared handle).
    assert_eq!(
        rig.project.resolve_asset("@workshop-12345/tex").unwrap(),
        file
    );
}

#[test]
fn an_updated_item_replaces_its_mount() {
    let mut rig = Rig::new();
    rig.subscribe(GOOD);
    let WorkshopContentEvent::Mounted {
        revision: first, ..
    } = rig.download(GOOD)[0].clone()
    else {
        panic!("not mounted");
    };
    assert!(rig.resolve(GOOD, "tex2").is_err());

    // Steam rewrites the folder with a new version.
    write_item(
        &rig.content,
        GOOD,
        r#"[{"id":"tex","kind":"texture","path":"tex.ppm"},{"id":"tex2","kind":"texture","path":"tex2.ppm"}]"#,
    );
    let events = rig.download(GOOD);
    let [WorkshopContentEvent::Replaced { revision, .. }] = events.as_slice() else {
        panic!("not replaced: {events:?}");
    };
    assert!(*revision > first);
    assert!(rig.resolve(GOOD, "tex2").is_ok());
}

#[test]
fn unsubscribing_unmounts() {
    let mut rig = Rig::new();
    rig.subscribe(GOOD);
    rig.download(GOOD);
    assert!(rig.resolve(GOOD, "tex").is_ok());
    rig.service.unsubscribe(GOOD).unwrap();
    let events = rig.pump(|e| matches!(e, UgcEvent::Reply(UgcRaw::Subscription { .. })));
    assert_eq!(
        events,
        vec![WorkshopContentEvent::Unmounted {
            item: GOOD,
            reason: None
        }]
    );
    assert!(rig.resolve(GOOD, "tex").is_err());
    assert!(rig.project.content_roots.mounts().is_empty());
}

#[test]
fn an_invalid_item_is_skipped_with_an_event_and_does_not_stop_others() {
    let mut rig = Rig::new();
    rig.subscribe(BAD);
    let events = rig.download(BAD);
    let [WorkshopContentEvent::Skipped { item, reason }] = events.as_slice() else {
        panic!("not skipped: {events:?}");
    };
    assert_eq!(*item, BAD);
    assert!(reason.contains("content_root.kind"), "{reason}");
    assert!(rig.project.content_roots.mounts().is_empty());

    rig.subscribe(GOOD);
    rig.download(GOOD);
    assert!(rig.resolve(GOOD, "tex").is_ok());
}

#[test]
fn a_malformed_install_path_is_skipped() {
    // The folder Steam reports must be the item's own folder; a root equal to its parent
    // (a bare content directory) is refused.
    let mut rig = Rig::new();
    let root = rig.content.clone();
    fs::write(root.join("content.json"), r#"{"schema":1,"assets":[]}"#).unwrap();
    rig.fake
        .lock()
        .unwrap()
        .set_installed_path(GOOD, root.join("sub").join(".."));
    rig.subscribe(GOOD);
    let events = rig.download(GOOD);
    assert!(matches!(
        events.as_slice(),
        [WorkshopContentEvent::Skipped { .. }]
    ));
}

#[test]
fn subscribed_installed_items_are_mounted_at_startup() {
    let mut rig = Rig::new();
    rig.subscribe(GOOD);
    rig.subscribe(BAD);
    // A fresh glue (a new run) mounts what is already installed.
    let mut fresh = WorkshopContent::new(rig.project.content_roots.clone());
    let events = fresh.mount_subscribed(&rig.service);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, WorkshopContentEvent::Mounted { item, .. } if *item == GOOD))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, WorkshopContentEvent::Skipped { item, .. } if *item == BAD))
    );
    assert!(rig.resolve(GOOD, "tex").is_ok());
    let _ = &rig.session;
}

#[test]
fn an_update_that_fails_validation_unmounts_the_stale_version() {
    let mut rig = Rig::new();
    rig.subscribe(GOOD);
    rig.download(GOOD);
    assert!(rig.resolve(GOOD, "tex").is_ok());
    // The new version ships a script: it is refused, and the old mount must not stay.
    write_item(
        &rig.content,
        GOOD,
        r#"[{"id":"tex","kind":"texture","path":"tex.ppm"},{"id":"s","kind":"script","path":"tex2.ppm"}]"#,
    );
    let events = rig.download(GOOD);
    let [WorkshopContentEvent::Unmounted { item, reason }] = events.as_slice() else {
        panic!("not unmounted: {events:?}");
    };
    assert_eq!(*item, GOOD);
    assert!(reason.as_deref().unwrap().contains("content_root.kind"));
    assert!(rig.resolve(GOOD, "tex").is_err());
    assert!(rig.project.content_roots.mounts().is_empty());
}

#[test]
fn reconcile_picks_up_an_unsubscribe_made_outside_the_game() {
    let mut rig = Rig::new();
    rig.subscribe(GOOD);
    rig.download(GOOD);
    assert!(
        rig.mods.reconcile(&rig.service).is_empty(),
        "still subscribed"
    );
    // The Steam client unsubscribes behind the game's back.
    rig.fake.lock().unwrap().ugc_subscribe(GOOD, false).unwrap();
    let events = rig.mods.reconcile(&rig.service);
    assert!(matches!(
        events.as_slice(),
        [WorkshopContentEvent::Unmounted { item, reason: Some(_) }] if *item == GOOD
    ));
    assert!(rig.resolve(GOOD, "tex").is_err());

    // mount_subscribed reconciles first: subscribe again, mount, drop the subscription,
    // and a fresh startup pass removes the stale mount.
    rig.fake.lock().unwrap().ugc_subscribe(GOOD, true).unwrap();
    rig.mods.mount_subscribed(&rig.service);
    assert!(rig.resolve(GOOD, "tex").is_ok());
    rig.fake.lock().unwrap().ugc_subscribe(GOOD, false).unwrap();
    let events = rig.mods.mount_subscribed(&rig.service);
    assert!(matches!(
        events.as_slice(),
        [WorkshopContentEvent::Unmounted { .. }]
    ));
    assert!(rig.resolve(GOOD, "tex").is_err());
}

#[test]
fn a_transiently_empty_subscription_list_unmounts_everything_until_the_next_startup_pass() {
    let mut rig = Rig::new();
    rig.subscribe(GOOD);
    rig.download(GOOD);
    assert!(rig.resolve(GOOD, "tex").is_ok());
    // Steam reports no subscriptions for a moment (documented behaviour): reconcile
    // trusts the list and unmounts the item.
    rig.fake.lock().unwrap().ugc_subscribe(GOOD, false).unwrap();
    let events = rig.mods.reconcile(&rig.service);
    assert!(matches!(
        events.as_slice(),
        [WorkshopContentEvent::Unmounted { item, .. }] if *item == GOOD
    ));
    assert!(rig.resolve(GOOD, "tex").is_err());
    // The list recovers; the installed folder is mounted again by mount_subscribed.
    rig.fake.lock().unwrap().ugc_subscribe(GOOD, true).unwrap();
    let events = rig.mods.mount_subscribed(&rig.service);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, WorkshopContentEvent::Mounted { item, .. } if *item == GOOD))
    );
    assert!(rig.resolve(GOOD, "tex").is_ok());
}
