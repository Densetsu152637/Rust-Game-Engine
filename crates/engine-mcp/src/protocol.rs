//! JSON-RPC envelope validation, the two protocol modes and method dispatch.
//!
//! * `2026-07-28` is stateless: every request carries `params._meta` with the
//!   protocol version and client capabilities, and `server/discover` replaces
//!   the handshake.
//! * `2025-11-25` uses the `initialize` handshake. Any proposed version is
//!   negotiated to this one (older clients such as `2025-06-18` and
//!   `2025-03-26` included).

use crate::{
    registry::{ToolContext, ToolFailure, tool_result_json},
    resources,
    server::Server,
};
use serde_json::{Value, json};

pub const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";
pub const LEGACY_PROTOCOL_VERSION: &str = "2025-11-25";
/// Older `initialize` versions that are accepted and negotiated to
/// [`LEGACY_PROTOCOL_VERSION`] (any other string is accepted the same way).
pub const OLDER_CLIENT_VERSIONS: [&str; 2] = ["2025-06-18", "2025-03-26"];
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_REQUEST_ID_BYTES: usize = 1024;
const VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
const CAPABILITIES_KEY: &str = "io.modelcontextprotocol/clientCapabilities";

const INSTRUCTIONS: &str = "Engine project server. Start with project_info, then use \
scene_list, scene_inspect, entity_get, asset_list, component_schema and input_actions to \
understand the project; file_list, file_read and search_text read project text files. \
Paths are project-relative with forward slashes. Every tool returns structuredContent \
{ok, ...}; failures carry error.code, error.message and error.hint, and validation \
failures also carry diagnostics. Scene edits (entity_*, component_*) and file_write are \
only available when the server was started with --allow-write and need the `revision` \
(or `hash`) returned by the read tools as expected_revision/expected_hash; on \
revision_conflict re-read and retry. project_build, project_run_headless and \
render_screenshot are only available with --allow-exec.";

/// A JSON-RPC error. `data` is only used for protocol-version errors.
pub(crate) struct RpcError {
    pub code: i32,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(-32602, message)
    }
    fn into_response(self, id: Value) -> Value {
        let mut error = json!({"code": self.code, "message": self.message});
        if let Some(data) = self.data {
            error["data"] = data;
        }
        json!({"jsonrpc": "2.0", "id": id, "error": error})
    }
}

pub(crate) fn error_response(id: Value, code: i32, message: impl AsRef<str>) -> Value {
    RpcError::new(code, message.as_ref()).into_response(id)
}

fn server_info() -> Value {
    json!({"name": "cpp-game-engine", "version": "0.1.0"})
}

fn capabilities() -> Value {
    json!({"tools": {"listChanged": false},
        "resources": {"subscribe": false, "listChanged": false}})
}

fn response(id: Value, result: Value, modern: bool) -> Value {
    let mut result = result;
    if modern {
        result["resultType"] = json!("complete");
        result["_meta"] = json!({"io.modelcontextprotocol/serverInfo": server_info()});
    }
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn human_limit(limit: usize) -> String {
    if limit >= 1024 * 1024 && limit.is_multiple_of(1024 * 1024) {
        format!("{} MiB", limit / (1024 * 1024))
    } else {
        format!("{limit} byte")
    }
}

/// The bounded replacement for a message longer than the output limit. Tool
/// results keep their shape (and modern metadata) so the session continues.
pub(crate) fn replace_oversized(message: &Value, limit: usize) -> Value {
    let id = message["id"].clone();
    if message["result"].get("content").is_some() {
        let failure = ToolFailure::new(
            "output_too_large",
            format!(
                "tool response exceeds the {} output limit",
                human_limit(limit)
            ),
        )
        .with_hint(
            "Narrow the request: add filters, lower `limit`, or page with `offset`, then retry.",
        );
        response(
            id,
            tool_result_json(&failure.to_json(), &[], true),
            message["result"].get("resultType").is_some(),
        )
    } else {
        error_response(
            id,
            -32603,
            format!("response exceeds the {} output limit", human_limit(limit)),
        )
    }
}

fn only_keys(value: &Value, allowed: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.keys().all(|key| allowed.contains(&key.as_str())))
}

fn valid_id(id: &Value) -> bool {
    id.is_number() || id.as_str().is_some_and(|s| s.len() <= MAX_REQUEST_ID_BYTES)
}

/// Per-connection protocol state (only the legacy handshake is stateful).
#[derive(Default)]
pub(crate) struct Session {
    initialized: bool,
    received_initialized: bool,
}

impl Session {
    pub fn process(&mut self, server: &Server, request: Value) -> Option<Value> {
        if !request.is_object() || request["jsonrpc"] != "2.0" || !request["method"].is_string() {
            return Some(error_response(
                Value::Null,
                -32600,
                "invalid JSON-RPC request",
            ));
        }
        let has_id = request.get("id").is_some();
        let id = request["id"].clone();
        if has_id && !valid_id(&id) {
            return Some(error_response(
                Value::Null,
                -32600,
                "request id must be a number or a string of at most 1024 bytes",
            ));
        }
        let method = request["method"].as_str().expect("validated method");
        let mut params = request.get("params").cloned().unwrap_or_else(|| json!({}));
        let handshaken = self.initialized && self.received_initialized;
        // A completed legacy session may carry unrelated MCP `_meta` members
        // (for example a progress token); those requests stay legacy.
        let foreign_meta = handshaken
            && method != "server/discover"
            && params.get("_meta").is_some_and(|meta| {
                meta.is_object()
                    && meta.get(VERSION_KEY).is_none()
                    && meta.get(CAPABILITIES_KEY).is_none()
            });
        if foreign_meta {
            params
                .as_object_mut()
                .expect("meta implies object")
                .remove("_meta");
        }
        if method == "server/discover" || params.get("_meta").is_some() {
            if !has_id {
                return None;
            }
            return Some(modern_request(server, id, method, params));
        }
        if method == "notifications/initialized" {
            if has_id {
                return Some(error_response(
                    id,
                    -32600,
                    "initialized must be a notification",
                ));
            }
            if self.initialized {
                self.received_initialized = true;
            }
            return None;
        }
        if !has_id {
            return None;
        }
        if method == "initialize" {
            if self.initialized || !params["protocolVersion"].is_string() {
                return Some(error_response(
                    id,
                    -32602,
                    "initialize requires protocolVersion and may only be sent once",
                ));
            }
            self.initialized = true;
            return Some(response(
                id,
                json!({"protocolVersion": LEGACY_PROTOCOL_VERSION, "capabilities": capabilities(),
                    "serverInfo": server_info(), "instructions": INSTRUCTIONS}),
                false,
            ));
        }
        if !handshaken {
            return Some(error_response(id, -32002, "server is not initialized"));
        }
        Some(if method == "ping" {
            response(id, json!({}), false)
        } else {
            match dispatch(server, method, &params) {
                Ok(result) => response(id, result, false),
                Err(error) => error.into_response(id),
            }
        })
    }
}

fn modern_request(server: &Server, id: Value, method: &str, mut params: Value) -> Value {
    if !params["_meta"].is_object() {
        return error_response(
            id,
            -32602,
            "modern MCP requests require per-request metadata",
        );
    }
    let metadata = &params["_meta"];
    let Some(version) = metadata[VERSION_KEY]
        .as_str()
        .filter(|_| metadata[CAPABILITIES_KEY].is_object())
    else {
        return error_response(
            id,
            -32602,
            "modern MCP metadata requires protocolVersion and clientCapabilities",
        );
    };
    if version != MODERN_PROTOCOL_VERSION {
        let mut error = RpcError::new(-32022, "Unsupported protocol version");
        error.data = Some(json!({"supported": [MODERN_PROTOCOL_VERSION], "requested": version}));
        return error.into_response(id);
    }
    params
        .as_object_mut()
        .expect("metadata requires object")
        .remove("_meta");
    if method == "server/discover" {
        return response(
            id,
            json!({"supportedVersions": [MODERN_PROTOCOL_VERSION], "capabilities": capabilities()}),
            true,
        );
    }
    match dispatch(server, method, &params) {
        Ok(result) => response(id, result, true),
        Err(error) => error.into_response(id),
    }
}

/// Methods shared by both protocol modes.
fn dispatch(server: &Server, method: &str, params: &Value) -> Result<Value, RpcError> {
    match method {
        "tools/list" => Ok(json!({"tools": server.registry.list_json()})),
        "tools/call" => call_tool(server, params),
        "resources/list" => resources::list(server, params),
        "resources/read" => resources::read(server, params),
        "resources/templates/list" => Ok(resources::templates()),
        _ => Err(RpcError::new(-32601, format!("method not found: {method}"))),
    }
}

fn call_tool(server: &Server, params: &Value) -> Result<Value, RpcError> {
    let invalid = |message: &str| RpcError::invalid_params(message);
    if !only_keys(params, &["name", "arguments"]) || !params["name"].is_string() {
        return Err(invalid("tools/call requires name and object arguments"));
    }
    let Some(args) = params["arguments"].as_object() else {
        return Err(invalid("tool arguments must be an object"));
    };
    let name = params["name"].as_str().expect("validated name");
    let cx = ToolContext {
        root: &server.root,
        limits: &server.config.limits,
        access: server.config.access,
    };
    server
        .registry
        .call(&cx, name, args)
        .map_err(RpcError::invalid_params)
}
