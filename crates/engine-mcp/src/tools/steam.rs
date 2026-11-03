//! `steam_status`: a read-only view of the project's Steam configuration and of the Steam
//! state this process has already established. It never initialises Steam, never calls the
//! Steam API and never reports tickets, tokens or other secrets.

use super::{Spec, tool};
use crate::{
    config::AccessLevel,
    registry::{Annotations, Tool, ToolContext, ToolOutcome, ToolResult},
    schema,
};
use engine_steam::{STEAM_COMPILED, SteamStatus, published_status};
use serde_json::{Map, Value, json};

pub(super) fn tools() -> Vec<Box<dyn Tool>> {
    vec![tool(
        Spec {
            name: "steam_status",
            title: "Steam status",
            description: "Report whether Steam support is compiled into this build, whether \
                the project declares a `steam` block (app id, required, cloud), and the Steam \
                state this process has established: `initialized`, `status` (`not_started`, \
                `not_compiled`, `not_running`, `init_failed` or `ready`) and the signed-in \
                user's persona name. Read-only: it never starts Steam or calls the Steam API, \
                and reports no tickets or secrets. A server that only inspects a project \
                reports `not_started`; the game's own run report carries the live status.",
            input: schema::object(vec![], &[]),
            output: schema::output(vec![
                (
                    "compiled",
                    schema::boolean("This build contains the real Steam backend.", false),
                ),
                (
                    "declared",
                    schema::boolean("The project manifest has a `steam` block.", false),
                ),
                (
                    "app_id",
                    json!({"type": ["integer", "null"],
                        "description": "App id from the manifest block, or null."}),
                ),
                ("required", json!({"type": ["boolean", "null"]})),
                ("cloud", json!({"type": ["boolean", "null"]})),
                (
                    "initialized",
                    schema::boolean("Steam is initialised in this process.", false),
                ),
                (
                    "status",
                    schema::string_enum(
                        "Steam state in this process.",
                        &[
                            "not_started",
                            "not_compiled",
                            "not_running",
                            "init_failed",
                            "ready",
                        ],
                    ),
                ),
                (
                    "user_name",
                    json!({"type": ["string", "null"],
                        "description": "Signed-in persona name when ready."}),
                ),
            ]),
            annotations: Annotations::READ_ONLY,
            access: AccessLevel::Read,
        },
        steam_status,
    )]
}

fn steam_status(cx: &ToolContext<'_>, _args: &Map<String, Value>) -> ToolOutcome {
    let project = cx.project()?;
    let status = published_status();
    let (state, user_name) = match &status {
        None => ("not_started", None),
        Some(SteamStatus::NotCompiled) => ("not_compiled", None),
        Some(SteamStatus::NotRunning) => ("not_running", None),
        Some(SteamStatus::InitFailed(_)) => ("init_failed", None),
        Some(SteamStatus::Ready { user, .. }) => ("ready", Some(user.name.clone())),
    };
    let steam = project.steam.as_ref();
    Ok(ToolResult::new(json!({
        "compiled": STEAM_COMPILED,
        "declared": steam.is_some(),
        "app_id": steam.map(|s| s.app_id),
        "required": steam.map(|s| s.required),
        "cloud": steam.map(|s| s.cloud),
        "initialized": status.as_ref().is_some_and(SteamStatus::is_ready),
        "status": state,
        "user_name": user_name,
    })))
}
