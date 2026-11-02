//! Live Steam tests: `cargo test -p engine-steam --features steam -- --ignored core`.
//!
//! They use App ID 480 (Spacewar) and skip, passing, when no Steam client runs.
//! The test executable needs `steam_api64.dll` (Windows) next to it; `build.rs`
//! copies it there.
#![cfg(feature = "steam")]

use engine_steam::*;

/// Initialises App 480, or prints a skip message and returns `None`.
fn steam_live() -> Option<RealBackend> {
    let mut backend = RealBackend::new();
    match backend.init(Some(AppId(480))) {
        SteamStatus::Ready { .. } => Some(backend),
        other => {
            println!("skipped: Steam not running ({other:?})");
            None
        }
    }
}

#[test]
#[ignore = "needs a running Steam client"]
fn core() {
    let Some(mut steam) = steam_live() else {
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
