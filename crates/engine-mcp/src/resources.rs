//! MCP resources: confined project files addressed as `project://<path>`.
//! Listing is paginated and skips hidden directories and links; reading serves
//! UTF-8 text as `text` and anything else as a base64 `blob`, bounded by the
//! configured resource size.

use crate::{
    paths::{self, ReadError},
    protocol::RpcError,
    server::Server,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

const SCHEME: &str = "project://";
const PAGE_SIZE: usize = 100;

fn uri_for(path: &str) -> String {
    format!("{SCHEME}{}", paths::percent_encode(path))
}

pub(crate) fn templates() -> Value {
    json!({"resourceTemplates": [{
        "uriTemplate": format!("{SCHEME}{{+path}}"),
        "name": "project_file",
        "title": "Project file",
        "description": "Any file inside the opened project root, addressed by its \
            project-relative path (forward slashes, percent-encoded). Text files are \
            returned as text, other files as base64 blobs. Hidden directories (.git, \
            .derived, target) and links are not served.",
        "mimeType": "application/octet-stream"
    }]})
}

pub(crate) fn list(server: &Server, params: &Value) -> Result<Value, RpcError> {
    let offset = match params.get("cursor") {
        None | Some(Value::Null) => 0,
        Some(Value::String(cursor)) => cursor
            .strip_prefix('o')
            .and_then(|n| n.parse::<usize>().ok())
            .ok_or_else(|| RpcError::invalid_params("invalid cursor"))?,
        Some(_) => return Err(RpcError::invalid_params("cursor must be a string")),
    };
    let (entries, _) = paths::walk(
        &server.root,
        &server.root,
        true,
        server.config.limits.max_walk_entries,
    );
    let files: Vec<_> = entries.into_iter().filter(|e| !e.is_dir).collect();
    let page: Vec<Value> = files
        .iter()
        .skip(offset)
        .take(PAGE_SIZE)
        .map(|entry| {
            json!({"uri": uri_for(&entry.path), "name": entry.path,
                "mimeType": paths::mime_for_extension(&entry.path), "size": entry.size})
        })
        .collect();
    let mut result = json!({"resources": page});
    if offset + PAGE_SIZE < files.len() {
        result["nextCursor"] = json!(format!("o{}", offset + PAGE_SIZE));
    }
    Ok(result)
}

pub(crate) fn read(server: &Server, params: &Value) -> Result<Value, RpcError> {
    let Some(uri) = params["uri"].as_str() else {
        return Err(RpcError::invalid_params(
            "resources/read requires a uri string",
        ));
    };
    let not_found = || RpcError::new(-32002, "resource not found");
    let Some(encoded) = uri.strip_prefix(SCHEME) else {
        return Err(RpcError::invalid_params(
            "unsupported URI scheme; use project://<project-relative path>",
        ));
    };
    if encoded.contains(['?', '#']) {
        return Err(RpcError::invalid_params(
            "project:// URIs carry no query or fragment",
        ));
    }
    let path = paths::percent_decode(encoded).ok_or_else(not_found)?;
    let file = paths::resolve_existing_file(&server.root, &path).ok_or_else(not_found)?;
    let relative = paths::display_relative(&server.root, &file);
    let limit = server.config.limits.max_resource_bytes;
    let bytes = paths::read_bounded(&file, limit).map_err(|error| match error {
        ReadError::TooLarge => RpcError::invalid_params(format!(
            "resource exceeds the {limit} byte resource limit; use file_read with a line range"
        )),
        ReadError::Io => not_found(),
    })?;
    let uri = uri_for(&relative);
    let declared = paths::mime_for_extension(&relative);
    let content = match paths::as_text(&bytes) {
        Some(text) => {
            let mime = if declared == "application/octet-stream" {
                "text/plain"
            } else {
                declared
            };
            json!({"uri": uri, "mimeType": mime, "text": text})
        }
        None => json!({"uri": uri, "mimeType": declared, "blob": STANDARD.encode(&bytes)}),
    };
    Ok(json!({"contents": [content]}))
}
