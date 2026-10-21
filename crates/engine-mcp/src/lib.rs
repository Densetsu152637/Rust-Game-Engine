//! Read-only, line-delimited JSON-RPC transport for authored project services.
//! Ports the engine adapter's two implemented protocol versions; no network,
//! game execution, native compilation, or process-global stdout is used here.

use engine_project::{Project, confined_file, load_scene};
use serde_json::{Value, json};
use std::{fmt, fs, io, io::BufRead, io::Write, path::Path};

pub const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";
pub const LEGACY_PROTOCOL_VERSION: &str = "2025-11-25";
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_REQUEST_ID_BYTES: usize = 1024;
const VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
const CAPABILITIES_KEY: &str = "io.modelcontextprotocol/clientCapabilities";

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    InvalidRoot,
    InvalidFraming,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::InvalidRoot => f.write_str("MCP project root is not an accessible directory"),
            Self::InvalidFraming => f.write_str("oversized or unterminated JSON-RPC message"),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}
impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}
pub type Result<T> = std::result::Result<T, Error>;

/// Serve a fixed, canonical project root until clean EOF. Input and output are
/// compact UTF-8 JSON messages terminated by LF (CRLF input is also accepted).
/// Protocol errors are sent to `writer`; transport/root errors are returned.
/// Output is flushed after each response. A partial final line or oversized
/// input emits a parse error and terminates with [`Error::InvalidFraming`].
pub fn serve(
    project_root: impl AsRef<Path>,
    reader: impl BufRead,
    writer: impl Write,
) -> Result<()> {
    serve_with_limit(project_root, reader, writer, MAX_MESSAGE_BYTES)
}

/// As [`serve`], with a configurable input-line limit in bytes, excluding LF.
/// Request storage never exceeds this limit; output retains the 1 MiB limit.
pub fn serve_with_limit(
    project_root: impl AsRef<Path>,
    mut reader: impl BufRead,
    mut writer: impl Write,
    max_input_message_bytes: usize,
) -> Result<()> {
    let root = fs::canonicalize(project_root).map_err(|_| Error::InvalidRoot)?;
    if !root.is_dir() {
        return Err(Error::InvalidRoot);
    }
    let mut session = Session::default();
    let mut line = Vec::new();
    let mut oversized = false;
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            if oversized || !line.is_empty() {
                let message = if oversized {
                    "message exceeds configured size limit"
                } else {
                    "unterminated JSON-RPC message"
                };
                emit(&mut writer, error_response(Value::Null, -32700, message))?;
                return Err(Error::InvalidFraming);
            }
            writer.flush()?;
            return Ok(());
        }
        let newline = bytes.iter().position(|&byte| byte == b'\n');
        let count = newline.unwrap_or(bytes.len());
        let remaining = max_input_message_bytes - line.len();
        line.extend_from_slice(&bytes[..count.min(remaining)]);
        oversized |= count > remaining;
        reader.consume(count + usize::from(newline.is_some()));
        if newline.is_none() {
            continue;
        }
        if oversized {
            emit(
                &mut writer,
                error_response(Value::Null, -32700, "message exceeds configured size limit"),
            )?;
            return Err(Error::InvalidFraming);
        }
        let response = match serde_json::from_slice(&line) {
            Ok(request) => session.process(&root, request),
            Err(_) => Some(error_response(Value::Null, -32700, "parse error")),
        };
        if let Some(response) = response {
            emit(&mut writer, response)?;
        }
        line.clear();
    }
}

fn response(id: Value, result: Value, modern: bool) -> Value {
    let mut result = result;
    if modern {
        result["resultType"] = json!("complete");
        result["_meta"] = json!({"io.modelcontextprotocol/serverInfo": server_info()});
    }
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}
fn server_info() -> Value {
    json!({"name": "cpp-game-engine", "version": "0.1.0"})
}
fn capabilities() -> Value {
    json!({"tools": {"listChanged": false}})
}
fn error_response(id: Value, code: i32, message: impl AsRef<str>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.as_ref()}})
}
fn tool_result(data: Value, failed: bool) -> Value {
    json!({"content": [{"type": "text", "text": data.to_string()}],
        "structuredContent": data, "isError": failed})
}
fn emit(writer: &mut impl Write, message: Value) -> io::Result<()> {
    let mut encoded = message.to_string();
    if encoded.len() > MAX_MESSAGE_BYTES {
        let id = message["id"].clone();
        let replacement = if message["result"].get("content").is_some() {
            response(
                id,
                tool_result(
                    json!({"ok": false, "error": "tool response exceeds the 1 MiB output limit"}),
                    true,
                ),
                message["result"].get("resultType").is_some(),
            )
        } else {
            error_response(id, -32603, "response exceeds the 1 MiB output limit")
        };
        encoded = replacement.to_string();
    }
    writer.write_all(encoded.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}
fn only_keys(value: &Value, allowed: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.keys().all(|key| allowed.contains(&key.as_str())))
}
fn valid_id(id: &Value) -> bool {
    id.is_number() || id.as_str().is_some_and(|s| s.len() <= MAX_REQUEST_ID_BYTES)
}

#[derive(Default)]
struct Session {
    initialized: bool,
    received_initialized: bool,
}
impl Session {
    fn process(&mut self, root: &Path, request: Value) -> Option<Value> {
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
        if method == "server/discover" || params.get("_meta").is_some() {
            if !has_id {
                return None;
            }
            if !params["_meta"].is_object() {
                return Some(error_response(
                    id,
                    -32602,
                    "modern MCP requests require per-request metadata",
                ));
            }
            let metadata = &params["_meta"];
            let Some(version) = metadata[VERSION_KEY]
                .as_str()
                .filter(|_| metadata[CAPABILITIES_KEY].is_object())
            else {
                return Some(error_response(
                    id,
                    -32602,
                    "modern MCP metadata requires protocolVersion and clientCapabilities",
                ));
            };
            if version != MODERN_PROTOCOL_VERSION {
                return Some(json!({"jsonrpc": "2.0", "id": id, "error": {
                    "code": -32022, "message": "Unsupported protocol version",
                    "data": {"supported": [MODERN_PROTOCOL_VERSION], "requested": version}
                }}));
            }
            params
                .as_object_mut()
                .expect("metadata requires object")
                .remove("_meta");
            return Some(match method {
                "server/discover" => response(
                    id,
                    json!({"supportedVersions": [MODERN_PROTOCOL_VERSION], "capabilities": capabilities()}),
                    true,
                ),
                "tools/list" => response(id, tools_list(), true),
                "tools/call" => tool_response(root, id, &params, true),
                _ => error_response(id, -32601, format!("method not found: {method}")),
            });
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
                json!({"protocolVersion": LEGACY_PROTOCOL_VERSION, "capabilities": capabilities(), "serverInfo": server_info()}),
                false,
            ));
        }
        if !self.initialized || !self.received_initialized {
            return Some(error_response(id, -32002, "server is not initialized"));
        }
        Some(match method {
            "ping" => response(id, json!({}), false),
            "tools/list" => response(id, tools_list(), false),
            "tools/call" => tool_response(root, id, &params, false),
            _ => error_response(id, -32601, format!("method not found: {method}")),
        })
    }
}

fn tools_list() -> Value {
    let tool = |name: &str, field: &str, description: &str| {
        json!({"name": name, "description": description,
            "inputSchema": {"type": "object", "properties": {field: {"type": "string", "description": format!("Project-relative {field} path.")}}, "required": [field], "additionalProperties": false},
            "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}})
    };
    json!({"tools": [
        tool("project_validate", "manifest", "Statically validate a project manifest and referenced authored content without compiling or running native game code."),
        tool("scene_inspect", "scene", "Inspect a scene and return its authored entities and components.")
    ]})
}

fn tool_response(root: &Path, id: Value, params: &Value, modern: bool) -> Value {
    match call_tool(root, params) {
        Ok(result) => response(id, result, modern),
        Err(message) => error_response(id, -32602, message),
    }
}
fn diagnostic_result(error: engine_project::Error) -> Value {
    tool_result(json!({"ok": false, "diagnostics": error.diagnostics}), true)
}
fn call_tool(root: &Path, params: &Value) -> std::result::Result<Value, String> {
    if !only_keys(params, &["name", "arguments"]) || !params["name"].is_string() {
        return Err("tools/call requires name and object arguments".into());
    }
    let args = &params["arguments"];
    if !args.is_object() {
        return Err("tool arguments must be an object".into());
    }
    let name = params["name"].as_str().expect("validated name");
    let field = match name {
        "project_validate" => "manifest",
        "scene_inspect" => "scene",
        _ => return Err(format!("unknown tool: {name}")),
    };
    if !only_keys(args, &[field]) || args.get(field).is_none() {
        return Err(format!(
            "{name} requires only a project-relative {field} path"
        ));
    }
    let file = args[field]
        .as_str()
        .and_then(|path| confined_file(root, Path::new(path)).ok());
    let Some(file) = file else {
        return Ok(tool_result(
            json!({"ok": false, "error": format!("{field} path is invalid, missing, or outside the opened project")}),
            true,
        ));
    };
    if name == "project_validate" {
        return Ok(match Project::load(file) {
            Ok(_) => tool_result(
                json!({"ok": true, "command": "project validate", "diagnostics": []}),
                false,
            ),
            Err(error) => diagnostic_result(error),
        });
    }
    let Ok(manifest) = confined_file(root, Path::new("project.json")) else {
        return Ok(tool_result(
            json!({"ok": false, "error": "opened project manifest project.json is missing or outside the project root"}),
            true,
        ));
    };
    let project = match Project::load(manifest) {
        Ok(project) => project,
        Err(error) => return Ok(diagnostic_result(error)),
    };
    // Canonicalize first, then load through the same confined service. This
    // also rejects links in the opened project's catalog and authored scenes.
    let inspected = load_scene(root, file.strip_prefix(root).expect("confined file"))
        .and_then(|scene| project.validate_scene(&scene).map(|()| scene));
    Ok(match inspected {
        Ok(scene) => tool_result(json!(scene), false),
        Err(error) => diagnostic_result(error),
    })
}
