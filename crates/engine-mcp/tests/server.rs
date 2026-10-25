//! Server construction, configuration and output bounds.

mod common;

use common::*;
use engine_mcp::{
    AccessLevel, Annotations, Error, Limits, Server, ServerConfig, Tool, ToolContext,
    ToolDefinition, ToolFailure, ToolOutcome, ToolProvider,
};
use serde_json::{Map, Value, json};
use std::io::Cursor;

struct Clash;
struct ClashTool;
impl Tool for ClashTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "project_info".into(),
            title: "Clash".into(),
            description: "Duplicates a built-in name.".into(),
            input_schema: engine_mcp::schema::object(vec![], &[]),
            output_schema: None,
            annotations: Annotations::READ_ONLY,
            access: AccessLevel::Read,
        }
    }
    fn call(&self, _cx: &ToolContext<'_>, _args: &Map<String, Value>) -> ToolOutcome {
        Err(ToolFailure::new("never", "never called"))
    }
}
impl ToolProvider for Clash {
    fn tools(&self) -> Vec<Box<dyn Tool>> {
        vec![Box::new(ClashTool)]
    }
}

#[test]
fn duplicate_tool_names_are_a_configuration_error() {
    let dir = fixture();
    let error = Server::new(dir.path(), ServerConfig::new().with_provider(Clash))
        .err()
        .unwrap();
    assert!(matches!(error, Error::InvalidConfig(ref m) if m.contains("project_info")));
    assert!(
        error
            .to_string()
            .contains("invalid MCP server configuration")
    );
}

#[test]
fn a_server_can_be_reused_for_many_connections() {
    let dir = fixture();
    let server = Server::new(dir.path(), ServerConfig::new()).unwrap();
    for _ in 0..2 {
        let mut out = Vec::new();
        let request = format!("{}\n", modern(1, "tools/list", json!({})));
        server
            .serve(Cursor::new(request.into_bytes()), &mut out)
            .unwrap();
        let response: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 20);
    }
    let names: Vec<_> = server
        .registry()
        .definitions()
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(names[0], "project_info");
    assert!(
        server
            .registry()
            .definitions()
            .all(|d| d.access == AccessLevel::Read)
    );
}

#[test]
fn access_flags_are_independent() {
    let dir = fixture();
    let count = |config: ServerConfig| {
        Server::new(dir.path(), config)
            .unwrap()
            .registry()
            .definitions()
            .count()
    };
    assert_eq!(count(ServerConfig::new()), 20);
    let access = |config: ServerConfig, name: &str| {
        Server::new(dir.path(), config)
            .unwrap()
            .registry()
            .definitions()
            .find(|tool| tool.name == name)
            .map(|tool| tool.access)
    };
    assert_eq!(
        access(ServerConfig::new(), "navigation_query"),
        Some(AccessLevel::Read)
    );
    assert_eq!(access(ServerConfig::new(), "navigation_bake"), None);
    assert_eq!(
        access(ServerConfig::new().allow_write(true), "navigation_bake"),
        Some(AccessLevel::Write)
    );
    for name in ["acoustics_inspect", "acoustics_query"] {
        assert_eq!(
            access(ServerConfig::new(), name),
            Some(AccessLevel::Read),
            "{name}"
        );
    }
    assert_eq!(access(ServerConfig::new(), "acoustics_bake"), None);
    assert_eq!(
        access(ServerConfig::new().allow_write(true), "acoustics_bake"),
        Some(AccessLevel::Write)
    );
    for tool in ["locale_list", "locale_messages", "locale_pseudo_preview"] {
        assert_eq!(
            access(ServerConfig::new(), tool),
            Some(AccessLevel::Read),
            "{tool} is a read tool"
        );
    }
    for tool in [
        "locale_set_message",
        "locale_remove_message",
        "locale_add_locale",
    ] {
        assert_eq!(
            access(ServerConfig::new(), tool),
            None,
            "{tool} needs write"
        );
        assert_eq!(
            access(ServerConfig::new().allow_write(true), tool),
            Some(AccessLevel::Write)
        );
    }
    for tool in ["ui_inspect", "ui_preview"] {
        assert_eq!(
            access(ServerConfig::new(), tool),
            Some(AccessLevel::Read),
            "{tool} is a read tool"
        );
    }
    assert_eq!(
        access(ServerConfig::new(), "ui_edit"),
        None,
        "ui_edit needs write"
    );
    assert_eq!(
        access(ServerConfig::new().allow_write(true), "ui_edit"),
        Some(AccessLevel::Write)
    );
    assert_eq!(count(ServerConfig::new().allow_write(true)), 34);
    for (config, exposed) in [
        (ServerConfig::new(), false),
        (ServerConfig::new().allow_exec(true), false),
        (ServerConfig::new().allow_write(true), true),
    ] {
        let server = Server::new(dir.path(), config).unwrap();
        assert_eq!(
            server
                .registry()
                .definitions()
                .any(|tool| tool.name == "asset_import_model"),
            exposed,
            "model import requires authoring write access"
        );
    }
    assert_eq!(
        count(ServerConfig::new().allow_exec(true)),
        20,
        "exec tools come from providers"
    );
    assert_eq!(
        count(ServerConfig::new().allow_write(true).allow_write(false)),
        20
    );
}

#[test]
fn a_smaller_output_limit_replaces_large_results_with_a_bounded_error() {
    let dir = fixture();
    let config = ServerConfig::new().with_limits(Limits {
        max_output_message_bytes: 600,
        ..Default::default()
    });
    let result = result_with(
        dir.path(),
        config.clone(),
        "scene_inspect",
        json!({"scene":"scenes/start.json"}),
    );
    assert_eq!(result["isError"], true);
    let error = &result["structuredContent"]["error"];
    assert_eq!(error["code"], "output_too_large");
    assert_eq!(
        error["message"],
        "tool response exceeds the 600 byte output limit"
    );
    assert!(error["hint"].as_str().unwrap().contains("Narrow"));
    // Small results still pass and the session survives.
    let responses = exchange(
        dir.path(),
        config,
        &[
            modern(
                1,
                "tools/call",
                json!({"name":"scene_inspect","arguments":{"scene":"scenes/start.json"}}),
            ),
            modern(
                2,
                "tools/call",
                json!({"name":"asset_info","arguments":{"asset":"nope"}}),
            ),
        ],
    );
    assert_eq!(responses[0]["id"], 1);
    assert_eq!(responses[1]["id"], 2);
}

#[test]
fn oversized_resource_responses_become_json_rpc_errors() {
    let dir = fixture();
    let config = ServerConfig::new().with_limits(Limits {
        max_output_message_bytes: 300,
        ..Default::default()
    });
    let responses = exchange(
        dir.path(),
        config,
        &[modern(1, "resources/list", json!({}))],
    );
    assert_eq!(responses[0]["error"]["code"], -32603);
    assert_eq!(responses[0]["id"], 1);
}

#[test]
fn input_limit_is_configurable() {
    let dir = fixture();
    let config = ServerConfig::new().with_limits(Limits {
        max_input_message_bytes: 64,
        ..Default::default()
    });
    let request = format!("{}\n", modern(1, "tools/list", json!({})));
    let mut out = Vec::new();
    let error = Server::new(dir.path(), config)
        .unwrap()
        .serve(Cursor::new(request.into_bytes()), &mut out)
        .unwrap_err();
    assert!(matches!(error, Error::InvalidFraming));
    let response: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(response["error"]["code"], -32700);
}

#[test]
fn tool_panics_become_internal_errors_and_the_session_continues() {
    struct Boom;
    struct BoomTool;
    impl Tool for BoomTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "boom".into(),
                title: "Boom".into(),
                description: "Always panics.".into(),
                input_schema: engine_mcp::schema::object(vec![], &[]),
                output_schema: None,
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            }
        }
        fn call(&self, _cx: &ToolContext<'_>, _args: &Map<String, Value>) -> ToolOutcome {
            panic!("deliberate test panic");
        }
    }
    impl ToolProvider for Boom {
        fn tools(&self) -> Vec<Box<dyn Tool>> {
            vec![Box::new(BoomTool)]
        }
    }
    let dir = fixture();
    let responses = exchange(
        dir.path(),
        ServerConfig::new().with_provider(Boom),
        &[
            modern(1, "tools/call", json!({"name":"boom","arguments":{}})),
            modern(
                2,
                "tools/call",
                json!({"name":"project_info","arguments":{}}),
            ),
        ],
    );
    assert_eq!(
        responses[0]["result"]["structuredContent"]["error"]["code"],
        "internal_error"
    );
    assert_eq!(responses[1]["result"]["isError"], false);
}
