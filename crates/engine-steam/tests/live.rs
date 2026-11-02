//! Live Steam tests: `cargo test -p engine-steam --features steam -- --ignored core`.
//!
//! They use App ID 480 (Spacewar) and skip, passing, when no Steam client runs.
//! The test executable needs `steam_api64.dll` (Windows) next to it; `build.rs`
//! copies it there.
#![cfg(feature = "steam")]

use engine_steam::*;

/// Only one initialised Steam client may exist per process, so live tests take turns.
static LIVE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Initialises App 480, or prints a skip message and returns `None`. The guard
/// serialises tests; it must outlive the backend, so it is declared first.
fn steam_live() -> Option<(std::sync::MutexGuard<'static, ()>, RealBackend)> {
    let guard = LIVE.lock().unwrap_or_else(|p| p.into_inner());
    let mut backend = RealBackend::new();
    match backend.init(Some(AppId(480))) {
        SteamStatus::Ready { .. } => Some((guard, backend)),
        other => {
            println!("skipped: Steam not running ({other:?})");
            None
        }
    }
}

#[test]
#[ignore = "needs a running Steam client"]
fn core() {
    let Some((_guard, mut steam)) = steam_live() else {
        // Uninitialised calls must fail softly, never panic.
        let mut backend = RealBackend::new();
        assert!(backend.auth_session_ticket().is_err());
        assert!(backend.activate_overlay("friends").is_err());
        assert!(backend.run_callbacks().is_empty());
        return;
    };
    let SteamStatus::Ready { app_id, user } = steam.status() else {
        panic!("steam_live returned a non-ready backend");
    };
    assert_eq!(app_id, AppId(480));
    assert_ne!(user.id, SteamId(0));
    println!("signed in as {} ({})", user.name, user.id.0);

    // Second init in one process is refused, not undefined.
    let mut second = RealBackend::new();
    assert!(matches!(
        second.init(Some(AppId(480))),
        SteamStatus::InitFailed(_)
    ));

    assert_eq!(steam.restart_app_if_necessary(AppId(480)), Ok(false));
    // Steam returns an empty ticket right after init; it works once callbacks have run.
    let mut ticket = steam.auth_session_ticket();
    for _ in 0..30 {
        if ticket.is_ok() {
            break;
        }
        let _ = steam.run_callbacks();
        std::thread::sleep(std::time::Duration::from_millis(100));
        ticket = steam.auth_session_ticket();
    }
    let ticket = ticket.expect("auth ticket");
    assert!(!ticket.data.is_empty());
    steam.cancel_auth_ticket(ticket.handle).expect("cancel");
    assert!(matches!(
        steam.cancel_auth_ticket(ticket.handle),
        Err(SteamError::NotFound(_))
    ));

    steam
        .set_rich_presence("status", Some("engine-steam live test"))
        .expect("set");
    steam.set_rich_presence("status", None).expect("clear");
    assert!(matches!(
        steam.set_rich_presence("a\0b", Some("x")),
        Err(SteamError::InvalidName(_))
    ));
    let _ = steam.run_callbacks();
}

/// Read-only: lists what Spacewar exposes to this account and reads one stat. It
/// never writes, so the account is untouched.
#[test]
#[ignore = "needs a running Steam client"]
fn stats() {
    let Some((_guard, mut steam)) = steam_live() else {
        return;
    };
    steam.request_current_stats().expect("request");
    let mut received = false;
    for _ in 0..50 {
        if steam.run_callbacks().contains(&SteamEvent::StatsReceived) {
            received = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    println!("stats received callback: {received}");
    for name in ["NumGames", "NumWins", "NumLosses", "FeetTraveled"] {
        println!("stat {name}: {:?}", steam.stat(name));
    }
    for name in ["ACH_WIN_ONE_GAME", "ACH_WIN_100_GAMES"] {
        println!("achievement {name}: {:?}", steam.achievement(name));
    }
    // An unknown name is a soft error, never a panic.
    assert!(steam.stat("definitely_not_a_stat").is_err());
    assert!(steam.achievement("definitely_not_an_achievement").is_err());
}

/// Round trip through Steam Cloud on App 480: write, list, timestamp, read, delete.
/// When Cloud is disabled for the app or account, checks the degrade path instead.
#[test]
#[ignore = "needs a running Steam client"]
fn cloud() {
    let Some((_guard, mut steam)) = steam_live() else {
        return;
    };
    let app = steam.cloud_enabled_for_app().expect("app flag");
    let account = steam.cloud_enabled_for_account().expect("account flag");
    println!("cloud enabled: app={app} account={account}");
    let name = "saves/engine_steam_live.json";
    let payload = br#"{"schema":1,"content_version":1,"data":{"live":true}}"#;
    if !(app && account) {
        assert_eq!(
            steam.cloud_write(name, payload),
            Err(SteamError::CloudDisabled)
        );
        println!("degrade path verified: writes are refused, the game stays local-only");
        return;
    }
    steam.cloud_write(name, payload).expect("write");
    assert!(steam.cloud_list().expect("list").iter().any(|n| n == name));
    let stamp = steam.cloud_timestamp(name).expect("timestamp");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    println!("cloud timestamp {stamp}, now {now}");
    assert!((now - stamp).abs() < 600);
    assert_eq!(steam.cloud_read(name).expect("read"), payload);
    assert_eq!(
        steam.cloud_write("../x", payload),
        Err(SteamError::InvalidName("../x".into()))
    );
    steam.cloud_delete(name).expect("delete");
    assert!(matches!(
        steam.cloud_timestamp(name),
        Err(SteamError::NotFound(_))
    ));
}
