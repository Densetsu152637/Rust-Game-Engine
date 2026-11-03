//! Host-service integration: initialises Steam before graphics, pumps callbacks once
//! per runtime tick, delivers Steam events to the game and releases Steam after the
//! GPU is idle.
//!
//! The plugin holds no Steam types beyond [`SteamBackend`]; the desktop runner only
//! sees `engine_project::HostService`.

use bevy_ecs::prelude::Resource;
use engine_project::{
    EngineEvent, HostService, HostServiceExit, Runtime, SteamManifest, emit_event,
};
use serde_json::{Value, json};

use std::sync::{Arc, Mutex};

use engine_project::runtime::Persistence;

use crate::cloud::CloudSync;
use crate::session::{SessionState, SteamSession};
use crate::types::{AppId, SteamEvent, SteamStatus};

/// Process exit code when `steam.required` is set and Steam is not available.
pub const REQUIRED_EXIT_CODE: i32 = 4;

/// The Steam status, published to the game world as a resource.
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct SteamRuntimeStatus(pub SteamStatus);

/// The status the most recent [`SteamPlugin`] tick in this process observed.
static PUBLISHED: Mutex<Option<SteamStatus>> = Mutex::new(None);

fn publish_status(status: &SteamStatus) {
    *PUBLISHED.lock().unwrap_or_else(|p| p.into_inner()) = Some(status.clone());
}

/// The Steam status a [`SteamPlugin`] last observed in this process, or `None` when no
/// plugin has started here. Read-only diagnostics (the `steam_status` MCP tool) use it so
/// they never initialise or call Steam themselves.
pub fn published_status() -> Option<SteamStatus> {
    PUBLISHED.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// Steam as a [`HostService`], configured from the manifest `steam` block.
pub struct SteamPlugin {
    session: Option<SteamSession>,
    manifest: SteamManifest,
}

impl SteamPlugin {
    /// Creates the plugin for one runner generation.
    pub fn new(session: SteamSession, manifest: SteamManifest) -> Self {
        Self {
            session: Some(session),
            manifest,
        }
    }
}

impl HostService for SteamPlugin {
    fn name(&self) -> &str {
        "steam"
    }

    fn before_graphics(&mut self, runtime: &mut Runtime) -> Result<(), HostServiceExit> {
        let Some(session) = &self.session else {
            return Ok(());
        };
        let mut state = session.lock();
        if !state.started {
            state.started = true;
            let app_id = AppId(self.manifest.app_id);
            // Only a required game relaunches itself through Steam; an optional one
            // just tries `init` and keeps running with a soft status. An
            // unavailable backend refuses the call, which is also the soft path.
            if self.manifest.required && state.backend.restart_app_if_necessary(app_id) == Ok(true)
            {
                state.relaunching = true;
            } else {
                let status = state.backend.init(Some(app_id));
                if status.is_ready() {
                    let SessionState { backend, stats, .. } = &mut *state;
                    // Stats are requested up front; a failure surfaces on first use.
                    let _ = stats.request(backend.as_mut());
                    for (key, value) in &self.manifest.rich_presence {
                        let _ = state.backend.set_rich_presence(key, Some(value));
                    }
                }
            }
        }
        if state.relaunching {
            return Err(HostServiceExit {
                code: 0,
                message: "Steam is relaunching the game; exiting".into(),
            });
        }
        let status = state.backend.status();
        publish_status(&status);
        if self.manifest.required && !status.is_ready() {
            return Err(HostServiceExit {
                code: REQUIRED_EXIT_CODE,
                message: format!("Steam is required but unavailable: {}", describe(&status)),
            });
        }
        if self.manifest.cloud && status.is_ready() {
            // Pull before the game loads anything, then mirror every later save.
            let store = runtime
                .app()
                .world()
                .get_resource::<Persistence>()
                .map(|p| p.0.clone());
            if let Some(store) = store {
                let cloud = CloudSync::new(store.clone());
                let report = cloud.sync_all(state.backend.as_mut());
                state.pending.extend(report.events.iter().cloned());
                state.cloud_report = Some(report);
                // The store owns the listener, so the listener must not own the store
                // (that would be a reference cycle): it holds a weak store, builds the
                // mirror per save, and removes itself once the session is gone.
                let weak = session.downgrade();
                let weak_store = Arc::downgrade(&store);
                store.set_save_listener(Some(Arc::new(move |slot: &str| {
                    let Some(store) = weak_store.upgrade() else {
                        return;
                    };
                    match weak.upgrade() {
                        Some(session) => session.push_cloud(&CloudSync::new(store), slot),
                        None => store.set_save_listener(None),
                    }
                })));
            }
        }
        drop(state);
        let world = runtime.app_mut().world_mut();
        world.insert_resource(SteamRuntimeStatus(status));
        // The game-facing handle: achievements, stats and (later) cloud services.
        world.insert_resource(session.clone());
        Ok(())
    }

    fn before_tick(&mut self, runtime: &mut Runtime, tick: u64) {
        let Some(session) = &self.session else {
            return;
        };
        let events = {
            let mut state = session.lock();
            let mut events = std::mem::take(&mut state.pending);
            let SessionState { backend, stats, .. } = &mut *state;
            events.extend(backend.run_callbacks());
            // A failed store is retried after the throttle interval.
            let _ = stats.tick(backend.as_mut(), tick);
            events
        };
        for event in events {
            if let SteamEvent::StatusChanged(status) = &event {
                runtime
                    .app_mut()
                    .world_mut()
                    .insert_resource(SteamRuntimeStatus(status.clone()));
            }
            if matches!(event, SteamEvent::ShutdownRequested) {
                runtime.app_mut().request_stop();
            }
            let (name, payload) = event_json(&event);
            // A full event queue drops Steam notifications rather than the game.
            let _ = emit_event(
                runtime.app_mut().world_mut(),
                EngineEvent {
                    name: name.to_owned(),
                    source: None,
                    target: None,
                    payload,
                },
            );
        }
    }

    fn after_gpu_idle(&mut self) {
        if let Some(session) = self.session.take() {
            // Persist batched stat changes before releasing this generation's hold.
            let mut state = session.lock();
            let SessionState { backend, stats, .. } = &mut *state;
            let _ = stats.flush(backend.as_mut());
        }
    }
}

fn describe(status: &SteamStatus) -> String {
    match status {
        SteamStatus::NotCompiled => "this build has no Steam support".into(),
        SteamStatus::NotRunning => "the Steam client is not running".into(),
        SteamStatus::InitFailed(why) => format!("initialisation failed: {why}"),
        SteamStatus::Ready { .. } => "ready".into(),
    }
}

/// JSON form of a status, for run reports and event payloads. SteamIDs are strings
/// because they exceed 2^53.
pub fn status_json(status: &SteamStatus) -> Value {
    match status {
        SteamStatus::NotCompiled => json!({"state": "not_compiled"}),
        SteamStatus::NotRunning => json!({"state": "not_running"}),
        SteamStatus::InitFailed(why) => json!({"state": "init_failed", "message": why}),
        SteamStatus::Ready { app_id, user } => json!({
            "state": "ready",
            "app_id": app_id.0,
            "user": {"id": user.id.0.to_string(), "name": user.name},
        }),
    }
}

/// Engine event name and payload for a Steam event (`steam.<name>`).
pub fn event_json(event: &SteamEvent) -> (&'static str, Value) {
    match event {
        SteamEvent::StatusChanged(status) => ("steam.status_changed", status_json(status)),
        SteamEvent::ShutdownRequested => ("steam.shutdown_requested", json!({})),
        SteamEvent::OverlayToggled(active) => ("steam.overlay_toggled", json!({"active": active})),
        SteamEvent::PersonaChanged(user) => {
            ("steam.persona_changed", json!({"user": user.0.to_string()}))
        }
        SteamEvent::ConnectionChanged { connected } => {
            ("steam.connection_changed", json!({"connected": connected}))
        }
        SteamEvent::AchievementUnlocked(name) => {
            ("steam.achievement_unlocked", json!({"name": name}))
        }
        SteamEvent::StatsReceived => ("steam.stats_received", json!({})),
        SteamEvent::StatsStored => ("steam.stats_stored", json!({})),
        SteamEvent::CloudConflict { name } => ("steam.cloud_conflict", json!({"name": name})),
        SteamEvent::CloudSyncFailed { slot, message } => (
            "steam.cloud_sync_failed",
            json!({"slot": slot, "message": message}),
        ),
        SteamEvent::LobbyCreated(id) => ("steam.lobby_created", json!({"lobby": id.0.to_string()})),
        SteamEvent::LobbyJoined(id) => ("steam.lobby_joined", json!({"lobby": id.0.to_string()})),
        SteamEvent::LobbyMemberJoined { lobby, user } => (
            "steam.lobby_member_joined",
            json!({"lobby": lobby.0.to_string(), "user": user.0.to_string()}),
        ),
        SteamEvent::LobbyMemberLeft { lobby, user } => (
            "steam.lobby_member_left",
            json!({"lobby": lobby.0.to_string(), "user": user.0.to_string()}),
        ),
        SteamEvent::WorkshopItemInstalled(id) => (
            "steam.workshop_item_installed",
            json!({"item": id.0.to_string()}),
        ),
        SteamEvent::ControllerConnected(id) => (
            "steam.controller_connected",
            json!({"controller": id.0.to_string()}),
        ),
        SteamEvent::ControllerDisconnected(id) => (
            "steam.controller_disconnected",
            json!({"controller": id.0.to_string()}),
        ),
    }
}
