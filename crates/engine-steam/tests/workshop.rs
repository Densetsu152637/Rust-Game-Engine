//! Workshop service over the scriptable fake. Nothing here uploads anything.

mod common;

use std::sync::{Arc, Mutex};

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

const ME: SteamId = SteamId(76_561_198_000_000_001);
const OTHER: SteamId = SteamId(76_561_198_000_000_002);

fn ready() -> FakeBackend {
    FakeBackend::ready(AppId(480), "tester")
}

fn item(id: u64, owner: SteamId, visibility: ItemVisibility, tags: &[&str]) -> WorkshopItem {
    WorkshopItem {
        id: WorkshopItemId(id),
        title: format!("Item {id}"),
        description: String::new(),
        owner,
        visibility,
        tags: tags.iter().map(|t| (*t).to_owned()).collect(),
        time_created: id as u32,
        time_updated: id as u32,
        file_size: 10,
        votes_up: id as u32,
        votes_down: 0,
        subscriptions: 0,
        banned: false,
    }
}

struct Rig {
    fake: Arc<Mutex<FakeBackend>>,
    session: SteamSession,
    service: UgcService,
}

impl Rig {
    fn new(fake: FakeBackend) -> Self {
        let fake = Arc::new(Mutex::new(fake));
        let session = SteamSession::new(Box::new(Shared { fake: fake.clone() }));
        let service = UgcService::new(session.clone()).unwrap();
        Self {
            fake,
            session,
            service,
        }
    }

    /// Polls until `done` matches an event; returns everything seen.
    fn until(&mut self, what: &str, done: impl Fn(&UgcEvent) -> bool) -> Vec<UgcEvent> {
        let mut all = Vec::new();
        for _ in 0..20 {
            let events = self.service.poll();
            let hit = events.iter().any(&done);
            all.extend(events);
            if hit {
                return all;
            }
        }
        panic!("timed out waiting for {what}: {all:?}");
    }

    fn reply(&mut self, request: RequestId) -> UgcRaw {
        let events = self.until(
            "a reply",
            |e| matches!(e, UgcEvent::Reply(r) if raw_request(r) == Some(request)),
        );
        events
            .into_iter()
            .find_map(|e| match e {
                UgcEvent::Reply(r) if raw_request(&r) == Some(request) => Some(r),
                _ => None,
            })
            .unwrap()
    }

    fn page(&mut self, request: RequestId) -> QueryPage {
        match self.reply(request) {
            UgcRaw::Queried { result, .. } => result.unwrap(),
            other => panic!("not a query reply: {other:?}"),
        }
    }
}

fn raw_request(raw: &UgcRaw) -> Option<RequestId> {
    match raw {
        UgcRaw::Created { request, .. }
        | UgcRaw::Submitted { request, .. }
        | UgcRaw::Subscription { request, .. }
        | UgcRaw::Queried { request, .. } => Some(*request),
        _ => None,
    }
}

fn ids(page: &QueryPage) -> Vec<u64> {
    page.items.iter().map(|i| i.id.0).collect()
}

#[test]
fn create_update_submit_reports_progress_and_applies_the_update() {
    let mut rig = Rig::new(ready());
    let request = rig.service.create_item().unwrap();
    let UgcRaw::Created { result, .. } = rig.reply(request) else {
        panic!("not a create reply");
    };
    let (id, legal) = result.unwrap();
    assert!(!legal);
    assert_eq!(
        rig.fake
            .lock()
            .unwrap()
            .workshop_item(id)
            .unwrap()
            .visibility,
        ItemVisibility::Private,
        "a new item starts private"
    );

    let content = tempfile::tempdir().unwrap();
    let update = ItemUpdate::new()
        .title("My mod")
        .description("Does things")
        .visibility(ItemVisibility::FriendsOnly)
        .tags(&["map", "co-op"])
        .content_folder(content.path())
        .change_note("first");
    let submit = rig.service.submit_item(id, &update).unwrap();
    let events = rig.until("the submission", |e| {
        matches!(e, UgcEvent::Reply(UgcRaw::Submitted { .. }))
    });
    let stages: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            UgcEvent::UpdateProgress {
                request, progress, ..
            } if *request == submit => Some(progress.stage),
            _ => None,
        })
        .collect();
    assert_eq!(
        stages,
        vec![
            UpdateStage::PreparingConfig,
            UpdateStage::PreparingContent,
            UpdateStage::UploadingContent,
            UpdateStage::CommittingChanges
        ]
    );
    assert!(events.iter().any(|e| matches!(
        e,
        UgcEvent::UpdateProgress { progress, .. }
            if progress.stage == UpdateStage::UploadingContent && progress.done == 50 && progress.total == 100
    )));
    assert!(matches!(
        events.last(),
        Some(UgcEvent::Reply(UgcRaw::Submitted { result: Ok((done, _)), .. })) if *done == id
    ));

    let fake = rig.fake.lock().unwrap();
    let stored = fake.workshop_item(id).unwrap();
    assert_eq!(stored.title, "My mod");
    assert_eq!(stored.visibility, ItemVisibility::FriendsOnly);
    assert_eq!(stored.tags, vec!["map", "co-op"]);
    assert_eq!(fake.applied_updates().len(), 1);
    assert_eq!(
        fake.applied_updates()[0].1.change_note.as_deref(),
        Some("first")
    );
    drop(fake);
    // The submission is no longer tracked: no more progress events.
    assert!(rig.service.poll().is_empty());
}

#[test]
fn bad_updates_are_refused_before_reaching_steam() {
    let mut rig = Rig::new(ready().with_workshop_item(item(5, OTHER, ItemVisibility::Public, &[])));
    let id = WorkshopItemId(5);
    let mut bad = |u: ItemUpdate| {
        matches!(
            rig.service.submit_item(id, &u),
            Err(SteamError::Rejected(_))
        )
    };
    assert!(bad(ItemUpdate::new().title("")));
    assert!(bad(ItemUpdate::new().title(&"x".repeat(129))));
    assert!(bad(ItemUpdate::new().description(&"x".repeat(8000))));
    assert!(bad(ItemUpdate::new().tags(&["a,b"])));
    assert!(bad(ItemUpdate::new().tags(&[""])));
    assert!(bad(ItemUpdate::new().title("a\0b")));
    assert!(bad(
        ItemUpdate::new().content_folder("definitely/not/a/folder")
    ));
    assert!(bad(ItemUpdate::new().preview("definitely/not/a/file.png")));
    assert_eq!(rig.fake.lock().unwrap().call_count("ugc_submit_item"), 0);

    // Someone else's item: the backend answers with a failed submission.
    let request = rig
        .service
        .submit_item(id, &ItemUpdate::new().title("Mine now"))
        .unwrap();
    let UgcRaw::Submitted { result, .. } = rig.reply(request) else {
        panic!("not a submit reply");
    };
    assert!(result.is_err());
    assert!(rig.fake.lock().unwrap().applied_updates().is_empty());
}

#[test]
fn subscribe_and_unsubscribe() {
    let mut rig = Rig::new(ready().with_workshop_item(item(7, OTHER, ItemVisibility::Public, &[])));
    let id = WorkshopItemId(7);
    let request = rig.service.subscribe(id).unwrap();
    assert_eq!(
        rig.reply(request),
        UgcRaw::Subscription {
            request,
            item: id,
            subscribed: true,
            result: Ok(())
        }
    );
    assert_eq!(rig.service.subscribed().unwrap(), vec![id]);
    assert!(rig.service.item_state(id).unwrap().subscribed);

    let request = rig.service.unsubscribe(id).unwrap();
    assert!(matches!(
        rig.reply(request),
        UgcRaw::Subscription {
            subscribed: false,
            result: Ok(()),
            ..
        }
    ));
    assert!(rig.service.subscribed().unwrap().is_empty());

    let request = rig.service.subscribe(WorkshopItemId(999)).unwrap();
    assert!(matches!(
        rig.reply(request),
        UgcRaw::Subscription { result: Err(_), .. }
    ));
}

#[test]
fn queries_filter_sort_and_page() {
    let mut fake = ready();
    for id in 1..=60 {
        let tags: &[&str] = if id % 2 == 0 { &["map"] } else { &["mod"] };
        fake = fake.with_workshop_item(item(id, OTHER, ItemVisibility::Public, tags));
    }
    fake = fake
        .with_workshop_item(item(100, OTHER, ItemVisibility::Private, &["map"]))
        .with_workshop_item(item(101, ME, ItemVisibility::Private, &["map"]));
    let mut rig = Rig::new(fake);

    // Public: most voted first, 50 per page, private items never listed.
    let request = rig.service.query_public(&PublicQuery::default()).unwrap();
    let page = rig.page(request);
    assert_eq!(page.total, 60);
    assert_eq!(page.items.len(), 50);
    assert_eq!(ids(&page)[..3], [60, 59, 58]);
    let request = rig
        .service
        .query_public(&PublicQuery {
            page: 2,
            ..PublicQuery::default()
        })
        .unwrap();
    assert_eq!(ids(&rig.page(request)), (1..=10).rev().collect::<Vec<_>>());

    let request = rig
        .service
        .query_public(&PublicQuery {
            required_tags: vec!["map".into()],
            sort: PublicSort::Newest,
            ..PublicQuery::default()
        })
        .unwrap();
    let page = rig.page(request);
    assert_eq!(page.total, 30);
    assert!(page.items.iter().all(|i| i.tags == ["map"]));

    let request = rig
        .service
        .query_public(&PublicQuery {
            sort: PublicSort::TextSearch,
            search: Some("item 42".into()),
            ..PublicQuery::default()
        })
        .unwrap();
    assert_eq!(ids(&rig.page(request)), vec![42]);

    // By id: someone else's private item is hidden, our own is visible, unknown ids vanish.
    let request = rig
        .service
        .query_items(&[
            WorkshopItemId(3),
            WorkshopItemId(100),
            WorkshopItemId(101),
            WorkshopItemId(5000),
        ])
        .unwrap();
    assert_eq!(ids(&rig.page(request)), vec![3, 101]);

    // Subscribed: nothing yet (an empty page), then the subscribed items.
    let request = rig.service.query_subscribed().unwrap();
    assert!(rig.page(request).items.is_empty());
    let request = rig.service.subscribe(WorkshopItemId(9)).unwrap();
    rig.reply(request);
    let request = rig.service.query_subscribed().unwrap();
    assert_eq!(ids(&rig.page(request)), vec![9]);

    // Invalid queries never reach the backend.
    let before = rig.fake.lock().unwrap().call_count("ugc_query");
    assert!(rig.service.query_items(&[]).is_err());
    let many: Vec<_> = (0..51).map(WorkshopItemId).collect();
    assert!(rig.service.query_items(&many).is_err());
    assert!(
        rig.service
            .query_public(&PublicQuery {
                page: 0,
                ..PublicQuery::default()
            })
            .is_err()
    );
    assert!(
        rig.service
            .query_public(&PublicQuery {
                sort: PublicSort::TextSearch,
                ..PublicQuery::default()
            })
            .is_err()
    );
    assert_eq!(rig.fake.lock().unwrap().call_count("ugc_query"), before);
}

fn installed(id: u64, needs_update: bool) -> InstalledItem {
    InstalledItem {
        id: WorkshopItemId(id),
        path: format!("workshop/content/480/{id}").into(),
        size_bytes: 1000,
        timestamp: 1_700_000_000,
        needs_update,
    }
}

#[test]
fn installed_info_requires_a_subscription() {
    let mut rig = Rig::new(
        ready()
            .with_workshop_item(item(7, OTHER, ItemVisibility::Public, &[]))
            .with_installed_item(installed(7, true)),
    );
    let id = WorkshopItemId(7);
    assert_eq!(rig.service.installed(id).unwrap(), None);
    let request = rig.service.subscribe(id).unwrap();
    rig.reply(request);
    assert_eq!(rig.service.installed(id).unwrap(), Some(installed(7, true)));
    let state = rig.service.item_state(id).unwrap();
    assert!(state.subscribed && state.installed && state.needs_update && !state.downloading);
}

#[test]
fn download_reports_progress_then_completion() {
    let mut rig = Rig::new(
        ready()
            .with_workshop_item(item(7, OTHER, ItemVisibility::Public, &[]))
            .with_installed_item(installed(7, true))
            .with_download_polls(4),
    );
    let id = WorkshopItemId(7);
    // Not subscribed: refused.
    assert!(matches!(
        rig.service.download_item(id, true),
        Err(SteamError::Rejected(_))
    ));
    let request = rig.service.subscribe(id).unwrap();
    rig.reply(request);

    rig.service.download_item(id, true).unwrap();
    assert!(rig.service.item_state(id).unwrap().downloading);
    let events = rig.until("the download", |e| {
        matches!(e, UgcEvent::Reply(UgcRaw::Downloaded { .. }))
    });
    let progress: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            UgcEvent::DownloadProgress { done, total, .. } => Some((*done, *total)),
            _ => None,
        })
        .collect();
    assert_eq!(progress, vec![(250, 1000), (500, 1000), (750, 1000)]);
    assert_eq!(
        events.last(),
        Some(&UgcEvent::Reply(UgcRaw::Downloaded {
            item: id,
            result: Ok(())
        }))
    );
    assert_eq!(
        rig.session.run_callbacks(),
        vec![SteamEvent::WorkshopItemInstalled(id)]
    );
    let info = rig.service.installed(id).unwrap().unwrap();
    assert!(!info.needs_update);
    assert!(!rig.service.item_state(id).unwrap().downloading);
    assert!(rig.service.poll().is_empty());
}

#[test]
fn a_failed_download_is_reported_without_an_installed_event() {
    let mut rig = Rig::new(ready().with_workshop_item(item(8, OTHER, ItemVisibility::Public, &[])));
    let id = WorkshopItemId(8);
    let request = rig.service.subscribe(id).unwrap();
    rig.reply(request);
    rig.service.download_item(id, false).unwrap();
    let events = rig.until("the failure", |e| {
        matches!(e, UgcEvent::Reply(UgcRaw::Downloaded { .. }))
    });
    assert!(matches!(
        events.last(),
        Some(UgcEvent::Reply(UgcRaw::Downloaded { result: Err(_), .. }))
    ));
    assert!(rig.session.run_callbacks().is_empty());
}

#[test]
fn without_steam_the_service_is_unavailable() {
    let session = SteamSession::new(Box::new(NullBackend::with_status(SteamStatus::NotRunning)));
    assert!(matches!(
        UgcService::new(session),
        Err(SteamError::Unavailable(_))
    ));
    // A service whose Steam went away reports errors instead of panicking.
    let mut rig = Rig::new(ready());
    rig.fake.lock().unwrap().set_status(SteamStatus::NotRunning);
    assert!(matches!(
        rig.service.create_item(),
        Err(SteamError::Unavailable(_))
    ));
    assert!(matches!(
        rig.service.subscribe(WorkshopItemId(1)),
        Err(SteamError::Unavailable(_))
    ));
    assert!(rig.service.subscribed().is_err());
    assert!(rig.service.poll().is_empty());
}

#[test]
fn tag_and_text_limits_follow_steams_buffers() {
    let mut rig = Rig::new(ready().with_workshop_item(item(5, ME, ItemVisibility::Public, &[])));
    let id = WorkshopItemId(5);
    // 4 tags of 255 bytes with 3 separators is 1023 bytes: fine; a fifth pushes past 1024.
    let tag = "t".repeat(255);
    let four: Vec<&str> = vec![&tag; 4];
    assert!(
        rig.service
            .submit_item(id, &ItemUpdate::new().tags(&four))
            .is_ok()
    );
    let five: Vec<&str> = vec![&tag; 5];
    assert!(matches!(
        rig.service.submit_item(id, &ItemUpdate::new().tags(&five)),
        Err(SteamError::Rejected(_))
    ));
    // The description and change note limits include Steam's terminating NUL.
    assert!(
        rig.service
            .submit_item(id, &ItemUpdate::new().description(&"d".repeat(7999)))
            .is_ok()
    );
    assert!(
        rig.service
            .submit_item(id, &ItemUpdate::new().change_note(&"n".repeat(8000)))
            .is_err()
    );
}

#[test]
fn an_unanswered_submission_times_out_and_is_forgotten() {
    let mut rig = Rig::new(ready().with_workshop_item(item(5, ME, ItemVisibility::Public, &[])));
    rig.service.set_tracking_timeout(std::time::Duration::ZERO);
    let request = rig
        .service
        .submit_item(WorkshopItemId(5), &ItemUpdate::new().title("t"))
        .unwrap();
    let events = rig.service.poll();
    assert!(events.iter().any(|e| matches!(
        e,
        UgcEvent::Reply(UgcRaw::Submitted { request: r, result: Err(_) }) if *r == request
    )));
    // Tracking is gone: no more events, and the fake no longer advances it.
    assert!(rig.service.poll().is_empty());
}

#[test]
fn a_download_without_a_callback_ends_when_the_item_is_installed() {
    let mut rig = Rig::new(
        ready()
            .with_workshop_item(item(7, OTHER, ItemVisibility::Public, &[]))
            .with_installed_item(installed(7, false))
            .with_download_polls(2)
            .with_silent_downloads(),
    );
    let id = WorkshopItemId(7);
    let request = rig.service.subscribe(id).unwrap();
    rig.reply(request);
    rig.service.download_item(id, false).unwrap();
    let events = rig.until("the state-based completion", |e| {
        matches!(e, UgcEvent::Reply(UgcRaw::Downloaded { .. }))
    });
    assert_eq!(
        events.last(),
        Some(&UgcEvent::Reply(UgcRaw::Downloaded {
            item: id,
            result: Ok(())
        }))
    );
    assert_eq!(
        rig.session.run_callbacks(),
        vec![SteamEvent::WorkshopItemInstalled(id)]
    );
    assert!(rig.service.poll().is_empty(), "no longer tracked");
}
