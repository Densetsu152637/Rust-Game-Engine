use engine_steam::*;

fn ready() -> FakeBackend {
    FakeBackend::ready(AppId(480), "tester")
}

#[cfg(not(feature = "steam"))]
#[test]
fn feature_off_everything_is_not_compiled() {
    const { assert!(!STEAM_COMPILED) };
    let mut backend = default_backend();
    assert_eq!(backend.status(), SteamStatus::NotCompiled);
    let expected = SteamError::Unavailable(UnavailableReason::NotCompiled);
    assert_eq!(
        backend.restart_app_if_necessary(AppId(480)),
        Err(expected.clone())
    );
    assert_eq!(backend.auth_session_ticket(), Err(expected.clone()));
    assert_eq!(backend.set_achievement("A", true), Err(expected.clone()));
    assert_eq!(backend.cloud_write("a.sav", b"x"), Err(expected.clone()));
    assert_eq!(
        backend.list_lobbies(&LobbyFilter::default()),
        Err(expected.clone())
    );
    assert_eq!(backend.subscribed_items(), Err(expected.clone()));
    assert_eq!(backend.controllers(), Err(expected));
    assert!(backend.run_callbacks().is_empty());
}

#[test]
fn null_backend_is_always_unavailable() {
    let mut null = NullBackend::with_status(SteamStatus::NotRunning);
    assert_eq!(null.status(), SteamStatus::NotRunning);
    assert_eq!(
        null.store_stats(),
        Err(SteamError::Unavailable(UnavailableReason::NotRunning))
    );
    // A ready status cannot make the null backend usable.
    let forced = NullBackend::with_status(ready().status());
    assert_eq!(forced.status(), SteamStatus::NotCompiled);
}

#[cfg(feature = "steam")]
#[test]
fn feature_on_stub_reports_init_failed() {
    const { assert!(STEAM_COMPILED) };
    let backend = default_backend();
    assert!(matches!(backend.status(), SteamStatus::InitFailed(_)));
}

#[test]
fn fake_unavailable_statuses_gate_calls() {
    let mut fake = FakeBackend::with_status(SteamStatus::InitFailed("boom".into()));
    assert_eq!(
        fake.set_rich_presence("k", Some("v")),
        Err(SteamError::Unavailable(UnavailableReason::InitFailed(
            "boom".into()
        )))
    );
}

#[test]
fn fake_achievements_unlock_once_and_persist_on_store() {
    let mut fake = ready().with_achievement("WIN");
    fake.set_achievement("WIN", true).unwrap();
    fake.set_achievement("WIN", true).unwrap();
    let events = fake.run_callbacks();
    assert_eq!(events, vec![SteamEvent::AchievementUnlocked("WIN".into())]);
    assert!(fake.stored_achievements().is_empty());
    fake.store_stats().unwrap();
    assert_eq!(fake.stored_achievements().get("WIN"), Some(&true));
    assert_eq!(
        fake.set_achievement("NOPE", true),
        Err(SteamError::NotFound("NOPE".into()))
    );
}

#[test]
fn fake_stats_reject_type_mismatch() {
    let mut fake = ready().with_stat("kills", StatValue::Int(1));
    fake.set_stat("kills", StatValue::Int(5)).unwrap();
    assert_eq!(fake.stat("kills"), Ok(StatValue::Int(5)));
    assert!(matches!(
        fake.set_stat("kills", StatValue::Float(1.0)),
        Err(SteamError::Rejected(_))
    ));
}

#[test]
fn fake_cloud_rules() {
    let mut fake = ready().with_cloud_quota(4);
    fake.cloud_write("slot/a.sav", b"abcd").unwrap();
    assert_eq!(
        fake.cloud_write("b.sav", b"x"),
        Err(SteamError::QuotaExceeded)
    );
    fake.cloud_write("slot/a.sav", b"ab").unwrap();
    assert_eq!(
        fake.cloud_write("../x", b"1"),
        Err(SteamError::InvalidName("../x".into()))
    );
    assert_eq!(fake.cloud_read("slot/a.sav"), Ok(b"ab".to_vec()));
    let mut off = ready().with_cloud_enabled(false);
    assert_eq!(off.cloud_write("a", b"1"), Err(SteamError::CloudDisabled));
}

#[test]
fn fake_lobbies_filter_and_limit() {
    let mut fake = ready();
    let id = fake.create_lobby(LobbyVisibility::Public, 1).unwrap();
    fake.set_lobby_data(id, "mode", "ctf").unwrap();
    let filter = LobbyFilter {
        string_equals: vec![("mode".into(), "ctf".into())],
        max_results: 0,
    };
    assert_eq!(fake.list_lobbies(&filter).unwrap().len(), 1);
    let other = LobbyFilter {
        string_equals: vec![("mode".into(), "dm".into())],
        max_results: 0,
    };
    assert!(fake.list_lobbies(&other).unwrap().is_empty());
    fake.leave_lobby(id).unwrap();
    assert_eq!(
        fake.join_lobby(id),
        Err(SteamError::NotFound("lobby 1".into()))
    );
}

#[test]
fn fake_records_call_order_and_events() {
    let mut fake = ready().with_restart_required(true);
    assert_eq!(fake.restart_app_if_necessary(AppId(480)), Ok(true));
    fake.run_callbacks();
    fake.run_callbacks();
    assert_eq!(fake.call_count("run_callbacks"), 2);
    assert_eq!(fake.calls()[0], "restart_app_if_necessary");
    fake.set_status(SteamStatus::NotRunning);
    assert_eq!(
        fake.run_callbacks(),
        vec![SteamEvent::StatusChanged(SteamStatus::NotRunning)]
    );
}

#[test]
fn fake_workshop_and_input() {
    let item = InstalledItem {
        id: WorkshopItemId(7),
        path: "mods/7".into(),
        size_bytes: 10,
    };
    let mut fake = ready()
        .with_installed_item(item.clone())
        .with_controller(ControllerId(1));
    assert_eq!(fake.installed_item(WorkshopItemId(7)), Ok(None));
    fake.subscribe_item(WorkshopItemId(7)).unwrap();
    assert_eq!(fake.installed_item(WorkshopItemId(7)), Ok(Some(item)));
    fake.activate_action_set(ControllerId(1), "menu").unwrap();
    assert_eq!(fake.action_set(ControllerId(1)), Some("menu"));
    assert!(fake.activate_action_set(ControllerId(2), "menu").is_err());
}
