//! Host-service integration: initialises Steam before graphics, pumps callbacks once
//! per runtime tick, delivers Steam events to the game and releases Steam after the
//! GPU is idle.
//!
//! The plugin holds no Steam types beyond [`SteamBackend`]; the desktop runner only
//! sees `engine_project::HostService`.

use std::sync::{Arc, Mutex, MutexGuard};

use bevy_ecs::prelude::Resource;
use engine_project::{
    EngineEvent, HostService, HostServiceExit, Runtime, SteamManifest, emit_event,
};
use serde_json::{Value, json};

use crate::backend::SteamBackend;
use crate::types::{AppId, SteamEvent, SteamStatus};

/// Process exit code when `steam.required` is set and Steam is not available.
pub const REQUIRED_EXIT_CODE: i32 = 4;

/// The Steam status, published to the game world as a resource.
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct SteamRuntimeStatus(pub SteamStatus);

struct SessionState {
    backend: Box<dyn SteamBackend + Send>,
    started: bool,
    relaunching: bool,
}

/// The process-wide owner of the Steam backend. Clone it into every runner
/// generation: the first `before_graphics` performs `restart_app_if_necessary`
/// and `init`, later generations reuse the result without calling Steam again.
/// The backend is dropped (Steam shuts down) when the last clone is dropped.
#[derive(Clone)]
pub struct SteamSession {
    inner: Arc<Mutex<SessionState>>,
}

impl SteamSession {
    /// Wraps a backend.
    pub fn new(backend: Box<dyn SteamBackend + Send>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SessionState {
                backend,
                started: false,
                relaunching: false,
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, SessionState> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The backend's current status.
    pub fn status(&self) -> SteamStatus {
        self.lock().backend.status()
    }
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
        if self.manifest.required && !status.is_ready() {
            return Err(HostServiceExit {
                code: REQUIRED_EXIT_CODE,
                message: format!("Steam is required but unavailable: {}", describe(&status)),
            });
        }
        runtime
            .app_mut()
            .world_mut()
            .insert_resource(SteamRuntimeStatus(status));
        Ok(())
    }

    fn before_tick(&mut self, runtime: &mut Runtime, _tick: u64) {
        let Some(session) = &self.session else {
            return;
        };
        let events = session.lock().backend.run_callbacks();
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
        // Releases this generation's hold on the session.
        self.session = None;
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
        SteamEvent::StatsStored => ("steam.stats_stored", json!({})),
        SteamEvent::CloudConflict { name } => ("steam.cloud_conflict", json!({"name": name})),
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
