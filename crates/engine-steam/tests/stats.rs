use engine_steam::*;

fn ready() -> FakeBackend {
    FakeBackend::ready(AppId(480), "tester")
        .with_achievement("WIN")
        .with_stat("games", StatValue::Int(0))
        .with_stat("speed", StatValue::Float(1.5))
}

#[test]
fn unlock_is_idempotent_and_reports_the_first_unlock_only() {
    let mut fake = ready();
    let mut stats = StatsService::default();
    assert_eq!(stats.unlock(&mut fake, "WIN"), Ok(true));
    assert_eq!(stats.unlock(&mut fake, "WIN"), Ok(false));
    assert_eq!(fake.call_count("set_achievement"), 1);
    let unlocked = fake
        .run_callbacks()
        .into_iter()
        .filter(|e| matches!(e, SteamEvent::AchievementUnlocked(_)))
        .count();
    assert_eq!(unlocked, 1);
    assert_eq!(stats.is_unlocked(&mut fake, "WIN"), Ok(true));
    assert_eq!(
        stats.unlock(&mut fake, "NOPE"),
        Err(SteamError::NotFound("NOPE".into()))
    );
}

#[test]
fn progress_is_clamped_and_saturates() {
    let mut fake = ready();
    let mut stats = StatsService::default();
    assert_eq!(stats.set_progress(&mut fake, "games", 50, 10), Ok(10));
    assert_eq!(stats.set_progress(&mut fake, "games", -4, 10), Ok(0));
    assert_eq!(stats.add_progress(&mut fake, "games", 7, 10), Ok(7));
    assert_eq!(stats.add_progress(&mut fake, "games", i32::MAX, 10), Ok(10));
    assert_eq!(stats.add_progress(&mut fake, "games", -100, 10), Ok(0));
    assert!(matches!(
        stats.set_progress(&mut fake, "games", 1, -1),
        Err(SteamError::Rejected(_))
    ));
    assert!(matches!(
        stats.add_progress(&mut fake, "speed", 1, 10),
        Err(SteamError::Rejected(_))
    ));
    assert_eq!(stats.stat(&mut fake, "speed"), Ok(StatValue::Float(1.5)));
}

#[test]
fn stats_are_requested_before_the_first_read_or_write() {
    // The fake refuses reads made before the request.
    assert_eq!(ready().stat("games"), Err(SteamError::StatsNotRequested));
    let mut fake = ready();
    let mut stats = StatsService::default();
    stats.stat(&mut fake, "games").unwrap();
    stats.set_progress(&mut fake, "games", 3, 10).unwrap();
    stats.unlock(&mut fake, "WIN").unwrap();
    assert_eq!(fake.calls()[0], "request_current_stats");
    assert_eq!(fake.call_count("request_current_stats"), 1);
}

#[test]
fn stores_are_batched_and_throttled_by_ticks() {
    let mut fake = ready();
    let mut stats = StatsService::new(100);
    stats.set_progress(&mut fake, "games", 1, 10).unwrap();
    stats.unlock(&mut fake, "WIN").unwrap();
    assert_eq!(
        stats.tick(&mut fake, 5),
        Ok(true),
        "first store is immediate"
    );
    assert_eq!(fake.call_count("store_stats"), 1);
    assert_eq!(stats.tick(&mut fake, 6), Ok(false), "nothing is dirty");
    stats.set_progress(&mut fake, "games", 2, 10).unwrap();
    stats.set_progress(&mut fake, "games", 3, 10).unwrap();
    assert_eq!(stats.tick(&mut fake, 50), Ok(false), "throttled");
    assert!(stats.is_dirty());
    assert_eq!(stats.tick(&mut fake, 105), Ok(true));
    assert_eq!(fake.call_count("store_stats"), 2, "two changes, one store");
    assert_eq!(fake.stored_stats()["games"], StatValue::Int(3));
}

#[test]
fn unchanged_values_do_not_dirty_the_batch_and_flush_stores_now() {
    let mut fake = ready();
    let mut stats = StatsService::new(1000);
    stats
        .set_stat(&mut fake, "games", StatValue::Int(0))
        .unwrap();
    assert!(!stats.is_dirty());
    stats
        .set_stat(&mut fake, "games", StatValue::Int(4))
        .unwrap();
    assert_eq!(stats.tick(&mut fake, 0), Ok(true));
    stats
        .set_stat(&mut fake, "games", StatValue::Int(5))
        .unwrap();
    assert_eq!(stats.tick(&mut fake, 1), Ok(false));
    assert_eq!(stats.flush(&mut fake), Ok(true));
    assert_eq!(fake.stored_stats()["games"], StatValue::Int(5));
    assert_eq!(stats.flush(&mut fake), Ok(false));
}

#[test]
fn session_handle_is_safe_without_steam() {
    let session = SteamSession::new(Box::new(NullBackend::not_compiled()));
    let gone = SteamError::Unavailable(UnavailableReason::NotCompiled);
    assert_eq!(session.unlock_achievement("WIN"), Err(gone.clone()));
    assert_eq!(session.set_progress("games", 1, 2), Err(gone.clone()));
    assert_eq!(session.stat("games"), Err(gone));
    assert_eq!(session.store_stats_now(), Ok(false));
}

#[test]
fn session_handle_works_with_a_ready_backend() {
    let session = SteamSession::new(Box::new(ready()));
    assert_eq!(session.unlock_achievement("WIN"), Ok(true));
    assert_eq!(session.unlock_achievement("WIN"), Ok(false));
    assert_eq!(session.add_progress("games", 2, 5), Ok(2));
    assert_eq!(session.store_stats_now(), Ok(true));
}
