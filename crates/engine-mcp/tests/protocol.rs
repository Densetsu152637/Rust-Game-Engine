//! JSON-RPC framing, negotiation, discovery listings, tool-call strictness and
//! path confinement for the read-only stdio adapter. Everything runs against
//! in-memory readers/writers and temporary directories.

use engine_mcp::{
    Error, LEGACY_PROTOCOL_VERSION, MAX_MESSAGE_BYTES, MODERN_PROTOCOL_VERSION,
    OLDER_CLIENT_VERSIONS, serve, serve_with_limit,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, BufReader, Cursor, Write},
    path::Path,
};
use tempfile::TempDir;

const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CAPS: &str = "io.modelcontextprotocol/clientCapabilities";

fn meta() -> Value {
    json!({META_VERSION: MODERN_PROTOCOL_VERSION, META_CAPS: {}})
}
fn modern(id: Value, method: &str, mut params: Value) -> Value {
    params["_meta"] = meta();
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}
fn modern_call(id: i32, name: &str, args: Value) -> Value {
    modern(
        json!(id),
        "tools/call",
        json!({"name": name, "arguments": args}),
    )
}
fn legacy(id: i32, method: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method})
}
fn legacy_with(id: i32, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}
fn initialized() -> Value {
    json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
}
/// The two messages that complete the legacy handshake.
fn handshake() -> Vec<Value> {
    vec![
        legacy_with(
            0,
            "initialize",
            json!({"protocolVersion": LEGACY_PROTOCOL_VERSION}),
        ),
        initialized(),
    ]
}

fn lines(requests: &[Value]) -> Vec<u8> {
    requests
        .iter()
        .flat_map(|r| format!("{r}\n").into_bytes())
        .collect()
}
fn decode(output: &[u8]) -> Vec<Value> {
    let text = std::str::from_utf8(output).unwrap();
    assert!(
        text.is_empty() || text.ends_with('\n'),
        "every message ends with LF"
    );
    text.lines()
        .map(|line| {
            assert!(line.len() <= MAX_MESSAGE_BYTES);
            serde_json::from_str(line).expect("output must be protocol JSON only")
        })
        .collect()
}
/// Serves raw bytes and returns the responses plus the serve result.
fn raw(root: &Path, input: &[u8]) -> (Vec<Value>, Result<(), Error>) {
    let mut out = Vec::new();
    let result = serve(root, Cursor::new(input.to_vec()), &mut out);
    (decode(&out), result)
}
fn run(root: &Path, requests: &[Value]) -> Vec<Value> {
    let (responses, result) = raw(root, &lines(requests));
    result.unwrap();
    responses
}
fn empty_root() -> TempDir {
    tempfile::tempdir().unwrap()
}
fn one(requests: &[Value]) -> Value {
    let dir = empty_root();
    let mut responses = run(dir.path(), requests);
    assert_eq!(responses.len(), 1, "{responses:?}");
    responses.remove(0)
}

fn fixture() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("scenes")).unwrap();
    fs::create_dir(dir.path().join("assets")).unwrap();
    fs::write(
        dir.path().join("project.json"),
        json!({"schema":1,"name":"Fixture","startup_scene":"scenes/start.json","assets":[
            {"id":"asset:mesh","kind":"mesh","path":"assets/triangle.mesh"},
            {"id":"asset:texture","kind":"texture","path":"assets/white.ppm"}
        ]})
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.path().join("assets/triangle.mesh"),
        "CGMESH 1\nvertex 0 0 0 0 0\nvertex 1 0 0 1 0\nvertex 0 1 0 0 1\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("assets/white.ppm"),
        "P3\n1 1\n255\n255 255 255\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("scenes/start.json"),
        json!({"schema":1,"scene_id":"scene:start","entities":[
            {"id":"entity:a","name":"A","components":{"MeshRenderer":{"mesh":"asset:mesh","texture":"asset:texture"}}}
        ]})
        .to_string(),
    )
    .unwrap();
    dir
}

fn code(response: &Value) -> i64 {
    response["error"]["code"]
        .as_i64()
        .unwrap_or_else(|| panic!("not an error: {response}"))
}
fn tool_error_text(response: &Value) -> String {
    assert_eq!(response["result"]["isError"], true, "{response}");
    response["result"]["structuredContent"]["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

// ---- constants -------------------------------------------------------------------------

#[test]
fn protocol_constants_are_stable() {
    assert_eq!(MODERN_PROTOCOL_VERSION, "2026-07-28");
    assert_eq!(LEGACY_PROTOCOL_VERSION, "2025-11-25");
    assert_eq!(MAX_MESSAGE_BYTES, 1024 * 1024);
}

// ---- envelope validation ---------------------------------------------------------------

macro_rules! invalid_envelope {
    ($($name:ident: $line:expr;)*) => {$(
        #[test]
        fn $name() {
            let dir = empty_root();
            let input = format!("{}\n", $line);
            let (responses, result) = raw(dir.path(), input.as_bytes());
            result.unwrap();
            assert_eq!(responses.len(), 1);
            assert_eq!(code(&responses[0]), -32600);
            assert_eq!(responses[0]["id"], Value::Null);
            assert_eq!(responses[0]["jsonrpc"], "2.0");
        }
    )*};
}

invalid_envelope! {
    rejects_number_at_top_level: "42";
    rejects_string_at_top_level: "\"ping\"";
    rejects_null_at_top_level: "null";
    rejects_boolean_at_top_level: "true";
    rejects_array_at_top_level: "[1,2]";
    rejects_batch_of_requests: r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#;
    rejects_empty_object: "{}";
    rejects_missing_jsonrpc_member: r#"{"id":1,"method":"ping"}"#;
    rejects_jsonrpc_version_one: r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#;
    rejects_numeric_jsonrpc_member: r#"{"jsonrpc":2,"id":1,"method":"ping"}"#;
    rejects_missing_method: r#"{"jsonrpc":"2.0","id":1}"#;
    rejects_numeric_method: r#"{"jsonrpc":"2.0","id":1,"method":7}"#;
    rejects_null_method: r#"{"jsonrpc":"2.0","id":1,"method":null}"#;
    rejects_object_method: r#"{"jsonrpc":"2.0","id":1,"method":{}}"#;
    rejects_null_id: r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#;
    rejects_boolean_id: r#"{"jsonrpc":"2.0","id":true,"method":"ping"}"#;
    rejects_object_id: r#"{"jsonrpc":"2.0","id":{"a":1},"method":"ping"}"#;
    rejects_array_id: r#"{"jsonrpc":"2.0","id":[1],"method":"ping"}"#;
}

#[test]
fn string_id_of_exactly_1024_bytes_is_accepted_and_1025_is_rejected() {
    let dir = empty_root();
    let ok = json!({"jsonrpc":"2.0","id":"x".repeat(1024),"method":"missing"});
    let bad = json!({"jsonrpc":"2.0","id":"x".repeat(1025),"method":"missing"});
    let responses = run(dir.path(), &[ok, bad]);
    assert_eq!(
        code(&responses[0]),
        -32002,
        "valid id reaches method dispatch"
    );
    assert_eq!(responses[0]["id"], "x".repeat(1024));
    assert_eq!(code(&responses[1]), -32600);
    assert_eq!(responses[1]["id"], Value::Null);
}

#[test]
fn multibyte_string_id_is_measured_in_bytes() {
    let dir = empty_root();
    // 513 two-byte characters = 1026 bytes.
    let id = "\u{e9}".repeat(513);
    let responses = run(
        dir.path(),
        &[json!({"jsonrpc":"2.0","id":id,"method":"ping"})],
    );
    assert_eq!(code(&responses[0]), -32600);
}

macro_rules! echoed_ids {
    ($($name:ident: $id:expr;)*) => {$(
        #[test]
        fn $name() {
            let response = one(&[json!({"jsonrpc":"2.0","id":$id,"method":"nope"})]);
            assert_eq!(response["id"], json!($id));
            assert_eq!(code(&response), -32002);
        }
    )*};
}

echoed_ids! {
    zero_id_is_echoed: 0;
    negative_id_is_echoed: -17;
    large_id_is_echoed: 9_007_199_254_740_991i64;
    fractional_id_is_echoed: 1.5;
    string_id_is_echoed: "req-\u{3b1}";
    empty_string_id_is_echoed: "";
}

#[test]
fn request_without_params_is_treated_as_empty_params() {
    let response = one(&[json!({"jsonrpc":"2.0","id":1,"method":"initialize"})]);
    assert_eq!(code(&response), -32602, "initialize needs protocolVersion");
}

macro_rules! parse_errors {
    ($($name:ident: $bytes:expr;)*) => {$(
        #[test]
        fn $name() {
            let dir = empty_root();
            let mut input: Vec<u8> = $bytes.to_vec();
            input.push(b'\n');
            input.extend_from_slice(&lines(&[legacy(5, "ping")]));
            let (responses, result) = raw(dir.path(), &input);
            result.unwrap();
            assert_eq!(responses.len(), 2, "{responses:?}");
            assert_eq!(code(&responses[0]), -32700);
            assert_eq!(responses[0]["error"]["message"], "parse error");
            assert_eq!(responses[0]["id"], Value::Null);
            assert_eq!(responses[1]["id"], 5, "the stream recovers after a parse error");
        }
    )*};
}

parse_errors! {
    unclosed_object_is_a_parse_error: b"{\"jsonrpc\":\"2.0\"";
    plain_text_is_a_parse_error: b"hello";
    blank_line_is_a_parse_error: b"";
    trailing_garbage_is_a_parse_error: b"{} extra";
    single_quotes_are_a_parse_error: b"{'jsonrpc':'2.0'}";
    invalid_utf8_is_a_parse_error: b"{\"jsonrpc\":\"\xff\xfe\"}";
    bare_comma_is_a_parse_error: b",";
    trailing_comma_is_a_parse_error: b"{\"a\":1,}";
}

#[test]
fn crlf_line_endings_are_accepted() {
    let dir = empty_root();
    let (responses, result) = raw(
        dir.path(),
        b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"x\"}\r\n",
    );
    result.unwrap();
    assert_eq!(responses[0]["id"], 3);
}

#[test]
fn output_is_one_compact_line_per_response() {
    let dir = empty_root();
    let mut out = Vec::new();
    serve(
        dir.path(),
        Cursor::new(lines(&[legacy(1, "x"), legacy(2, "y")])),
        &mut out,
    )
    .unwrap();
    let text = String::from_utf8(out).unwrap();
    assert_eq!(text.matches('\n').count(), 2);
    assert!(
        text.lines()
            .all(|l| !l.contains(": ") && !l.starts_with(' '))
    );
}

#[test]
fn responses_follow_request_order_and_ids() {
    let dir = empty_root();
    let requests: Vec<Value> = (10..30).map(|id| legacy(id, "missing")).collect();
    let responses = run(dir.path(), &requests);
    let ids: Vec<i64> = responses
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, (10..30).collect::<Vec<_>>());
}

#[test]
fn identical_transcripts_produce_identical_output() {
    let dir = fixture();
    let mut requests = handshake();
    requests.push(legacy(1, "tools/list"));
    requests.push(modern_call(
        2,
        "project_validate",
        json!({"manifest":"project.json"}),
    ));
    let a = run(dir.path(), &requests);
    let b = run(dir.path(), &requests);
    assert_eq!(a, b);
}

#[test]
fn empty_input_is_a_clean_eof_with_no_output() {
    let (responses, result) = raw(empty_root().path(), b"");
    result.unwrap();
    assert!(responses.is_empty());
}

#[test]
fn notifications_alone_produce_no_output() {
    let dir = empty_root();
    let responses = run(
        dir.path(),
        &[
            initialized(),
            json!({"jsonrpc":"2.0","method":"ping"}),
            json!({"jsonrpc":"2.0","method":"tools/list"}),
            json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}),
        ],
    );
    assert!(responses.is_empty());
}

// ---- framing limits --------------------------------------------------------------------

fn serve_limited(input: &[u8], limit: usize) -> (Vec<Value>, Result<(), Error>) {
    let dir = empty_root();
    let mut out = Vec::new();
    let result = serve_with_limit(dir.path(), Cursor::new(input.to_vec()), &mut out, limit);
    (decode(&out), result)
}

#[test]
fn line_exactly_at_the_limit_is_processed() {
    let line = lines(&[legacy(1, "x")]);
    let limit = line.len() - 1; // excludes LF
    let (responses, result) = serve_limited(&line, limit);
    result.unwrap();
    assert_eq!(responses[0]["id"], 1);
}

#[test]
fn line_one_byte_over_the_limit_terminates_with_invalid_framing() {
    let line = lines(&[legacy(1, "x")]);
    let (responses, result) = serve_limited(&line, line.len() - 2);
    assert!(matches!(result, Err(Error::InvalidFraming)));
    assert_eq!(responses.len(), 1);
    assert_eq!(code(&responses[0]), -32700);
    assert_eq!(
        responses[0]["error"]["message"],
        "message exceeds configured size limit"
    );
}

#[test]
fn responses_before_an_oversized_line_are_still_delivered() {
    let mut input = lines(&[legacy(1, "x")]);
    let limit = input.len();
    input.extend_from_slice(&vec![b'a'; limit * 3]);
    input.push(b'\n');
    input.extend_from_slice(&lines(&[legacy(2, "x")]));
    let (responses, result) = serve_limited(&input, limit);
    assert!(matches!(result, Err(Error::InvalidFraming)));
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["id"], 1);
    assert_eq!(code(&responses[1]), -32700);
}

#[test]
fn oversized_unterminated_input_reports_the_size_limit() {
    let (responses, result) = serve_limited(&vec![b'z'; 500], 100);
    assert!(matches!(result, Err(Error::InvalidFraming)));
    assert_eq!(
        responses[0]["error"]["message"],
        "message exceeds configured size limit"
    );
}

#[test]
fn unterminated_final_message_is_rejected_even_when_valid_json() {
    let dir = empty_root();
    let (responses, result) = raw(dir.path(), br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
    assert!(matches!(result, Err(Error::InvalidFraming)));
    assert_eq!(responses.len(), 1);
    assert_eq!(
        responses[0]["error"]["message"],
        "unterminated JSON-RPC message"
    );
}

#[test]
fn complete_messages_before_an_unterminated_tail_are_answered() {
    let mut input = lines(&[legacy(1, "x")]);
    input.extend_from_slice(b"{\"jsonrpc\"");
    let (responses, result) = raw(empty_root().path(), &input);
    assert!(matches!(result, Err(Error::InvalidFraming)));
    assert_eq!(responses[0]["id"], 1);
    assert_eq!(code(&responses[1]), -32700);
}

#[test]
fn default_limit_accepts_large_but_bounded_requests() {
    let dir = empty_root();
    let padding = "p".repeat(512 * 1024);
    let request = json!({"jsonrpc":"2.0","id":1,"method":"x","params":{"pad":padding}});
    let responses = run(dir.path(), &[request]);
    assert_eq!(code(&responses[0]), -32002);
}

#[test]
fn default_limit_rejects_requests_over_one_mebibyte() {
    let dir = empty_root();
    let mut input = vec![b'a'; MAX_MESSAGE_BYTES + 1];
    input.push(b'\n');
    let (responses, result) = raw(dir.path(), &input);
    assert!(matches!(result, Err(Error::InvalidFraming)));
    assert_eq!(code(&responses[0]), -32700);
}

macro_rules! reader_capacities {
    ($($name:ident: $capacity:expr;)*) => {$(
        #[test]
        fn $name() {
            let dir = fixture();
            let mut requests = handshake();
            requests.push(legacy(1, "tools/list"));
            requests.push(modern_call(2, "scene_inspect", json!({"scene":"scenes/start.json"})));
            requests.push(json!({"jsonrpc":"2.0","id":3,"method":"\u{3b1}\u{3b2}\u{3b3}"}));
            let input = lines(&requests);
            let expected = run(dir.path(), &requests);
            let mut out = Vec::new();
            serve(dir.path(), BufReader::with_capacity($capacity, Cursor::new(input)), &mut out).unwrap();
            assert_eq!(decode(&out), expected);
        }
    )*};
}

reader_capacities! {
    one_byte_reads_give_identical_output: 1;
    two_byte_reads_give_identical_output: 2;
    seven_byte_reads_give_identical_output: 7;
    page_sized_reads_give_identical_output: 4096;
}

// ---- legacy handshake ------------------------------------------------------------------

#[test]
fn initialize_reports_the_legacy_version_name_and_capabilities() {
    let response = one(&[legacy_with(
        1,
        "initialize",
        json!({"protocolVersion":"1900-01-01"}),
    )]);
    assert_eq!(
        response["result"]["protocolVersion"],
        LEGACY_PROTOCOL_VERSION
    );
    assert_eq!(
        response["result"]["serverInfo"],
        json!({"name":"cpp-game-engine","version":"0.1.0"})
    );
    assert_eq!(response["result"]["capabilities"], capabilities());
    assert!(
        response["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("project_info")
    );
}

fn capabilities() -> Value {
    json!({"tools":{"listChanged":false},"resources":{"subscribe":false,"listChanged":false}})
}

macro_rules! older_client_versions {
    ($($name:ident: $version:expr;)*) => {$(
        #[test]
        fn $name() {
            let response = one(&[legacy_with(1, "initialize", json!({"protocolVersion": $version}))]);
            assert_eq!(response["result"]["protocolVersion"], LEGACY_PROTOCOL_VERSION);
            assert_eq!(response["result"]["capabilities"], capabilities());
        }
    )*};
}

older_client_versions! {
    client_2025_06_18_negotiates_to_the_legacy_version: "2025-06-18";
    client_2025_03_26_negotiates_to_the_legacy_version: "2025-03-26";
    client_2024_11_05_negotiates_to_the_legacy_version: "2024-11-05";
    client_proposing_the_modern_version_via_initialize_gets_the_legacy_one: MODERN_PROTOCOL_VERSION;
}

#[test]
fn documented_older_versions_are_listed() {
    assert_eq!(OLDER_CLIENT_VERSIONS, ["2025-06-18", "2025-03-26"]);
}

#[test]
fn legacy_requests_may_carry_unrelated_meta_after_the_handshake() {
    let dir = fixture();
    let mut requests = handshake();
    requests.push(legacy_with(
        1,
        "tools/call",
        json!({"name":"project_info","arguments":{},"_meta":{"progressToken":"p1"}}),
    ));
    requests.push(legacy_with(
        2,
        "tools/list",
        json!({"_meta":{"progressToken":7}}),
    ));
    let responses = run(dir.path(), &requests);
    assert_eq!(responses[1]["result"]["isError"], false, "{}", responses[1]);
    assert!(responses[1]["result"].get("resultType").is_none());
    assert!(responses[2]["result"]["tools"].is_array());
}

#[test]
fn legacy_results_carry_no_modern_metadata() {
    let response = one(&[legacy_with(1, "initialize", json!({"protocolVersion":"x"}))]);
    assert!(response["result"].get("resultType").is_none());
    assert!(response["result"].get("_meta").is_none());
}

macro_rules! bad_initialize {
    ($($name:ident: $params:expr;)*) => {$(
        #[test]
        fn $name() {
            let response = one(&[legacy_with(1, "initialize", $params)]);
            assert_eq!(code(&response), -32602);
        }
    )*};
}

bad_initialize! {
    initialize_rejects_missing_protocol_version: json!({});
    initialize_rejects_numeric_protocol_version: json!({"protocolVersion": 20251125});
    initialize_rejects_null_protocol_version: json!({"protocolVersion": null});
    initialize_rejects_array_protocol_version: json!({"protocolVersion": ["2025-11-25"]});
}

#[test]
fn initialize_may_only_be_sent_once() {
    let dir = empty_root();
    let init = legacy_with(1, "initialize", json!({"protocolVersion":"x"}));
    let responses = run(dir.path(), &[init.clone(), init]);
    assert!(responses[0].get("result").is_some());
    assert_eq!(code(&responses[1]), -32602);
}

#[test]
fn failed_initialize_does_not_start_the_session() {
    let dir = empty_root();
    let responses = run(
        dir.path(),
        &[legacy(1, "initialize"), initialized(), legacy(2, "ping")],
    );
    assert_eq!(code(&responses[0]), -32602);
    assert_eq!(code(&responses[1]), -32002);
}

#[test]
fn requests_before_initialize_are_refused() {
    let dir = empty_root();
    let responses = run(
        dir.path(),
        &[
            legacy(1, "ping"),
            legacy(2, "tools/list"),
            legacy(3, "tools/call"),
        ],
    );
    assert!(responses.iter().all(|r| code(r) == -32002));
}

#[test]
fn initialized_notification_before_initialize_is_ignored() {
    let dir = empty_root();
    let init = legacy_with(2, "initialize", json!({"protocolVersion":"x"}));
    let responses = run(dir.path(), &[initialized(), init, legacy(3, "ping")]);
    assert!(responses[0].get("result").is_some());
    assert_eq!(
        code(&responses[1]),
        -32002,
        "the early notification must not count"
    );
}

#[test]
fn requests_between_initialize_and_initialized_are_refused() {
    let dir = empty_root();
    let responses = run(
        dir.path(),
        &[
            legacy_with(1, "initialize", json!({"protocolVersion":"x"})),
            legacy(2, "tools/list"),
        ],
    );
    assert_eq!(code(&responses[1]), -32002);
}

#[test]
fn initialized_with_an_id_is_an_invalid_request() {
    let dir = empty_root();
    let responses = run(
        dir.path(),
        &[
            legacy_with(1, "initialize", json!({"protocolVersion":"x"})),
            json!({"jsonrpc":"2.0","id":2,"method":"notifications/initialized"}),
            legacy(3, "ping"),
        ],
    );
    assert_eq!(code(&responses[1]), -32600);
    assert_eq!(
        code(&responses[2]),
        -32002,
        "an id-bearing initialized does not complete the handshake"
    );
}

#[test]
fn initialize_sent_as_a_notification_is_silent_and_inert() {
    let dir = empty_root();
    let responses = run(
        dir.path(),
        &[
            json!({"jsonrpc":"2.0","method":"initialize","params":{"protocolVersion":"x"}}),
            initialized(),
            legacy(1, "ping"),
        ],
    );
    assert_eq!(responses.len(), 1);
    assert_eq!(code(&responses[0]), -32002);
}

#[test]
fn ping_after_the_handshake_returns_an_empty_object() {
    let dir = empty_root();
    let mut requests = handshake();
    requests.push(legacy(7, "ping"));
    let responses = run(dir.path(), &requests);
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[1], json!({"jsonrpc":"2.0","id":7,"result":{}}));
}

#[test]
fn unknown_legacy_method_names_itself_in_the_error() {
    let dir = empty_root();
    let mut requests = handshake();
    requests.push(legacy(1, "prompts/get"));
    let responses = run(dir.path(), &requests);
    assert_eq!(code(&responses[1]), -32601);
    assert_eq!(
        responses[1]["error"]["message"],
        "method not found: prompts/get"
    );
}

macro_rules! unsupported_methods {
    ($($name:ident: $method:expr;)*) => {$(
        #[test]
        fn $name() {
            let dir = empty_root();
            let mut requests = handshake();
            requests.push(legacy(1, $method));
            let responses = run(dir.path(), &requests);
            assert_eq!(code(&responses[1]), -32601);
        }
    )*};
}

unsupported_methods! {
    prompts_list_is_not_implemented: "prompts/list";
    resources_subscribe_is_not_implemented: "resources/subscribe";
    completion_is_not_implemented: "completion/complete";
    logging_set_level_is_not_implemented: "logging/setLevel";
    empty_method_name_is_not_implemented: "";
    method_matching_is_case_sensitive: "Ping";
    unknown_namespaced_method_is_not_implemented: "server/discover-legacy";
}

#[test]
fn legacy_server_discover_without_metadata_is_a_metadata_error() {
    let dir = empty_root();
    let mut requests = handshake();
    requests.push(legacy(1, "server/discover"));
    let responses = run(dir.path(), &requests);
    assert_eq!(code(&responses[1]), -32602);
}

#[test]
fn legacy_tools_list_enumerates_the_read_only_tools() {
    let dir = empty_root();
    let mut requests = handshake();
    requests.push(legacy(1, "tools/list"));
    let responses = run(dir.path(), &requests);
    assert_eq!(
        responses[1]["result"]["tools"].as_array().unwrap().len(),
        READ_TOOLS.len()
    );
}

// ---- modern negotiation ----------------------------------------------------------------

#[test]
fn server_discover_lists_the_single_supported_version() {
    let response = one(&[modern(json!(1), "server/discover", json!({}))]);
    let result = &response["result"];
    assert_eq!(
        result["supportedVersions"],
        json!([MODERN_PROTOCOL_VERSION])
    );
    assert_eq!(result["capabilities"], capabilities());
    assert_eq!(result["resultType"], "complete");
    assert_eq!(
        result["_meta"]["io.modelcontextprotocol/serverInfo"],
        json!({"name":"cpp-game-engine","version":"0.1.0"})
    );
}

#[test]
fn modern_requests_work_without_any_handshake() {
    let response = one(&[modern(json!(1), "tools/list", json!({}))]);
    assert_eq!(response["result"]["resultType"], "complete");
    assert_eq!(
        response["result"]["tools"].as_array().unwrap().len(),
        READ_TOOLS.len()
    );
}

#[test]
fn modern_traffic_does_not_advance_the_legacy_session() {
    let dir = empty_root();
    let responses = run(
        dir.path(),
        &[
            modern(json!(1), "tools/list", json!({})),
            legacy(2, "tools/list"),
        ],
    );
    assert!(responses[0].get("result").is_some());
    assert_eq!(code(&responses[1]), -32002);
}

#[test]
fn modern_and_legacy_requests_can_interleave_after_the_handshake() {
    let dir = empty_root();
    let mut requests = handshake();
    requests.push(modern(json!(1), "tools/list", json!({})));
    requests.push(legacy(2, "tools/list"));
    let responses = run(dir.path(), &requests);
    assert!(responses[1]["result"].get("resultType").is_some());
    assert!(responses[2]["result"].get("resultType").is_none());
}

#[test]
fn server_discover_without_params_requires_metadata() {
    let response = one(&[legacy(1, "server/discover")]);
    assert_eq!(code(&response), -32602);
}

macro_rules! bad_metadata {
    ($($name:ident: $meta:expr;)*) => {$(
        #[test]
        fn $name() {
            let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta": $meta}});
            let response = one(&[request]);
            assert_eq!(code(&response), -32602, "{response}");
        }
    )*};
}

bad_metadata! {
    metadata_must_be_an_object_not_a_string: "2026-07-28";
    metadata_must_be_an_object_not_null: Value::Null;
    metadata_must_be_an_object_not_an_array: json!([]);
    metadata_requires_the_protocol_version: json!({META_CAPS: {}});
    metadata_requires_client_capabilities: json!({META_VERSION: MODERN_PROTOCOL_VERSION});
    metadata_version_must_be_a_string: json!({META_VERSION: 20260728, META_CAPS: {}});
    metadata_capabilities_must_be_an_object_not_null: json!({META_VERSION: MODERN_PROTOCOL_VERSION, META_CAPS: null});
    metadata_capabilities_must_be_an_object_not_an_array: json!({META_VERSION: MODERN_PROTOCOL_VERSION, META_CAPS: []});
    empty_metadata_object_is_incomplete: json!({});
}

macro_rules! unsupported_versions {
    ($($name:ident: $version:expr;)*) => {$(
        #[test]
        fn $name() {
            let request = json!({"jsonrpc":"2.0","id":4,"method":"tools/list",
                "params":{"_meta":{META_VERSION: $version, META_CAPS: {}}}});
            let response = one(&[request]);
            assert_eq!(code(&response), -32022);
            assert_eq!(response["error"]["message"], "Unsupported protocol version");
            assert_eq!(response["error"]["data"]["supported"], json!([MODERN_PROTOCOL_VERSION]));
            assert_eq!(response["error"]["data"]["requested"], $version);
            assert_eq!(response["id"], 4);
        }
    )*};
}

unsupported_versions! {
    legacy_version_in_modern_metadata_is_unsupported: LEGACY_PROTOCOL_VERSION;
    future_version_is_unsupported: "2099-01-01";
    empty_version_is_unsupported: "";
    version_comparison_is_exact: "2026-07-28 ";
}

#[test]
fn metadata_errors_for_notifications_are_silent() {
    let dir = empty_root();
    let responses = run(
        dir.path(),
        &[
            json!({"jsonrpc":"2.0","method":"tools/list","params":{"_meta":"bad"}}),
            json!({"jsonrpc":"2.0","method":"server/discover"}),
        ],
    );
    assert!(responses.is_empty());
}

#[test]
fn modern_unknown_method_is_not_found() {
    let response = one(&[modern(json!(1), "prompts/list", json!({}))]);
    assert_eq!(code(&response), -32601);
    assert_eq!(
        response["error"]["message"],
        "method not found: prompts/list"
    );
}

#[test]
fn modern_mode_has_no_ping_or_initialize() {
    let dir = empty_root();
    let responses = run(
        dir.path(),
        &[
            modern(json!(1), "ping", json!({})),
            modern(json!(2), "initialize", json!({"protocolVersion": "x"})),
        ],
    );
    assert!(responses.iter().all(|r| code(r) == -32601));
}

// ---- tool listing ----------------------------------------------------------------------

fn tools() -> Vec<Value> {
    one(&[modern(json!(1), "tools/list", json!({}))])["result"]["tools"]
        .as_array()
        .unwrap()
        .clone()
}

/// The always-available tools, in listing order.
const READ_TOOLS: [&str; 35] = [
    "project_info",
    "project_validate",
    "input_actions",
    "scene_list",
    "scene_inspect",
    "entity_get",
    "component_schema",
    "asset_list",
    "asset_info",
    "file_list",
    "file_read",
    "search_text",
    "navigation_query",
    "acoustics_inspect",
    "acoustics_query",
    "physics_query",
    "physics_debug",
    "locale_list",
    "locale_messages",
    "locale_pseudo_preview",
    "ui_inspect",
    "ui_preview",
    "network_inspect",
    "graph_schema",
    "graph_validate",
    "editor_settings_get",
    "tilemap_info",
    "tilemap_query",
    "pcg_validate",
    "streaming_info",
    "terrain_info",
    "foliage_info",
    "water_sample",
    "water_spectrum",
    "script_schema",
];

#[test]
fn read_only_servers_list_exactly_the_read_tools_in_order() {
    let names: Vec<_> = tools()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, READ_TOOLS);
}

#[test]
fn every_listed_tool_is_declared_read_only_and_closed_world() {
    for tool in tools() {
        let a = &tool["annotations"];
        assert_eq!(a["readOnlyHint"], true, "{tool}");
        assert_eq!(a["destructiveHint"], false);
        assert_eq!(a["idempotentHint"], true);
        assert_eq!(a["openWorldHint"], false);
    }
}

#[test]
fn every_tool_schema_is_a_closed_object_with_described_properties() {
    for tool in tools() {
        let schema = &tool["inputSchema"];
        assert_eq!(schema["type"], "object", "{tool}");
        assert_eq!(schema["additionalProperties"], false, "{tool}");
        let properties = schema["properties"].as_object().unwrap();
        for (name, property) in properties {
            assert!(
                property["description"]
                    .as_str()
                    .is_some_and(|d| d.len() > 8),
                "{} property {name} needs a description",
                tool["name"]
            );
            assert!(property["type"].is_string(), "{} {name}", tool["name"]);
        }
        for required in schema["required"].as_array().unwrap() {
            assert!(properties.contains_key(required.as_str().unwrap()));
        }
    }
}

#[test]
fn path_tools_require_a_string_path() {
    let by_name = |name: &str| tools().into_iter().find(|t| t["name"] == name).unwrap();
    assert_eq!(
        by_name("scene_inspect")["inputSchema"]["required"],
        json!(["scene"])
    );
    assert_eq!(
        by_name("scene_inspect")["inputSchema"]["properties"]["scene"]["type"],
        "string"
    );
    assert_eq!(
        by_name("project_validate")["inputSchema"]["required"],
        json!([])
    );
    assert_eq!(
        by_name("file_read")["inputSchema"]["required"],
        json!(["path"])
    );
    assert_eq!(
        by_name("search_text")["inputSchema"]["required"],
        json!(["query"])
    );
}

#[test]
fn every_tool_has_a_title_description_and_output_schema() {
    for tool in tools() {
        assert!(
            tool["title"].as_str().is_some_and(|t| t.len() > 3),
            "{tool}"
        );
        assert!(
            tool["description"].as_str().is_some_and(|d| d.len() > 60),
            "{tool}"
        );
        let output = &tool["outputSchema"];
        assert_eq!(output["type"], "object", "{tool}");
        assert_eq!(output["required"], json!(["ok"]));
        assert!(output["properties"]["error"]["properties"]["hint"].is_object());
    }
}

#[test]
fn enumerated_arguments_declare_their_values() {
    let tool = tools()
        .into_iter()
        .find(|t| t["name"] == "asset_list")
        .unwrap();
    let kinds = &tool["inputSchema"]["properties"]["kind"]["enum"];
    assert!(kinds.as_array().unwrap().contains(&json!("texture")));
    let tool = tools()
        .into_iter()
        .find(|t| t["name"] == "file_list")
        .unwrap();
    assert_eq!(tool["inputSchema"]["properties"]["path"]["default"], ".");
    assert_eq!(tool["inputSchema"]["properties"]["limit"]["maximum"], 1000);
}

#[test]
fn tool_listing_is_identical_across_protocol_modes() {
    let dir = empty_root();
    let mut requests = handshake();
    requests.push(legacy(1, "tools/list"));
    requests.push(modern(json!(2), "tools/list", json!({})));
    let responses = run(dir.path(), &requests);
    assert_eq!(
        responses[1]["result"]["tools"],
        responses[2]["result"]["tools"]
    );
}

// ---- tools/call strictness -------------------------------------------------------------

macro_rules! bad_calls {
    ($($name:ident: $params:expr;)*) => {$(
        #[test]
        fn $name() {
            let response = one(&[modern(json!(1), "tools/call", $params)]);
            assert_eq!(code(&response), -32602, "{response}");
        }
    )*};
}

bad_calls! {
    call_requires_a_name: json!({"arguments": {}});
    call_name_must_be_a_string: json!({"name": 5, "arguments": {}});
    call_rejects_unknown_tool: json!({"name": "project_build", "arguments": {}});
    call_rejects_tool_name_with_wrong_case: json!({"name": "Project_Validate", "arguments": {"manifest": "project.json"}});
    call_requires_arguments: json!({"name": "project_validate"});
    call_arguments_must_be_an_object_not_an_array: json!({"name": "project_validate", "arguments": ["project.json"]});
    call_arguments_must_be_an_object_not_a_string: json!({"name": "project_validate", "arguments": "project.json"});
    call_arguments_must_be_an_object_not_null: json!({"name": "project_validate", "arguments": null});
    call_rejects_unknown_top_level_members: json!({"name": "project_validate", "arguments": {"manifest": "project.json"}, "extra": 1});
    call_rejects_missing_required_argument: json!({"name": "scene_inspect", "arguments": {}});
    call_rejects_unknown_argument: json!({"name": "project_validate", "arguments": {"manifest": "project.json", "output": "x"}});
    call_rejects_wrong_argument_name_for_the_tool: json!({"name": "project_validate", "arguments": {"scene": "scenes/start.json"}});
    scene_call_rejects_manifest_argument: json!({"name": "scene_inspect", "arguments": {"manifest": "project.json"}});
}

#[test]
fn tool_names_in_calls_must_not_be_empty() {
    let response = one(&[modern(
        json!(1),
        "tools/call",
        json!({"name": "", "arguments": {}}),
    )]);
    assert_eq!(code(&response), -32602);
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown tool")
    );
}

#[test]
fn legacy_tools_call_applies_the_same_validation() {
    let dir = empty_root();
    let mut requests = handshake();
    requests.push(legacy_with(
        1,
        "tools/call",
        json!({"name": "nope", "arguments": {}}),
    ));
    let responses = run(dir.path(), &requests);
    assert_eq!(code(&responses[1]), -32602);
}

#[test]
fn legacy_tools_call_results_have_no_modern_metadata() {
    let dir = fixture();
    let mut requests = handshake();
    requests.push(legacy_with(
        1,
        "tools/call",
        json!({"name": "project_validate", "arguments": {"manifest": "project.json"}}),
    ));
    let responses = run(dir.path(), &requests);
    assert_eq!(responses[1]["result"]["isError"], false);
    assert!(responses[1]["result"].get("resultType").is_none());
}

#[test]
fn modern_tool_results_include_result_type_and_server_info() {
    let dir = fixture();
    let responses = run(
        dir.path(),
        &[modern_call(
            1,
            "project_validate",
            json!({"manifest":"project.json"}),
        )],
    );
    assert_eq!(responses[0]["result"]["resultType"], "complete");
    assert!(responses[0]["result"]["_meta"]["io.modelcontextprotocol/serverInfo"].is_object());
}

// ---- path confinement ------------------------------------------------------------------

macro_rules! rejected_paths {
    ($($name:ident: $tool:expr, $field:expr, $path:expr;)*) => {$(
        #[test]
        fn $name() {
            let dir = fixture();
            let outside = dir.path().parent().unwrap().join("outside-secret.json");
            let _ = fs::write(&outside, "{}");
            let responses = run(dir.path(), &[modern_call(1, $tool, json!({$field: $path}))]);
            let _ = fs::remove_file(&outside);
            assert!(
                tool_error_text(&responses[0]).contains("invalid, missing, or outside"),
                "{}",
                responses[0]
            );
            assert_eq!(responses[0]["result"]["structuredContent"]["ok"], false);
        }
    )*};
}

rejected_paths! {
    validate_rejects_parent_traversal: "project_validate", "manifest", "../outside-secret.json";
    validate_rejects_deep_parent_traversal: "project_validate", "manifest", "scenes/../../outside-secret.json";
    validate_rejects_backslash_traversal: "project_validate", "manifest", "..\\outside-secret.json";
    validate_rejects_unix_absolute_path: "project_validate", "manifest", "/etc/passwd";
    validate_rejects_windows_drive_path: "project_validate", "manifest", "C:\\Windows\\win.ini";
    validate_rejects_unc_path: "project_validate", "manifest", "\\\\server\\share\\project.json";
    validate_rejects_missing_file: "project_validate", "manifest", "missing.json";
    validate_rejects_empty_path: "project_validate", "manifest", "";
    validate_rejects_directory: "project_validate", "manifest", "scenes";
    validate_rejects_dot_directory: "project_validate", "manifest", ".";
    validate_rejects_nul_in_path: "project_validate", "manifest", "project.json\u{0}";
    validate_rejects_non_string_path: "project_validate", "manifest", 42;
    validate_rejects_null_path: "project_validate", "manifest", Value::Null;
    validate_rejects_array_path: "project_validate", "manifest", json!(["project.json"]);
    scene_rejects_parent_traversal: "scene_inspect", "scene", "../outside-secret.json";
    scene_rejects_absolute_path: "scene_inspect", "scene", "/scenes/start.json";
    scene_rejects_missing_scene: "scene_inspect", "scene", "scenes/missing.json";
    scene_rejects_directory: "scene_inspect", "scene", "scenes/";
    scene_rejects_non_string_path: "scene_inspect", "scene", true;
    scene_rejects_wildcard_path: "scene_inspect", "scene", "scenes/*.json";
}

#[test]
fn dot_slash_prefix_resolves_inside_the_root() {
    let dir = fixture();
    let responses = run(
        dir.path(),
        &[modern_call(
            1,
            "project_validate",
            json!({"manifest":"./project.json"}),
        )],
    );
    assert_eq!(responses[0]["result"]["isError"], false, "{}", responses[0]);
}

#[test]
fn parent_segments_are_rejected_even_when_they_stay_inside_the_root() {
    let dir = fixture();
    let responses = run(
        dir.path(),
        &[modern_call(
            1,
            "scene_inspect",
            json!({"scene":"assets/../scenes/start.json"}),
        )],
    );
    assert!(tool_error_text(&responses[0]).contains("invalid, missing, or outside"));
}

#[test]
fn rejection_messages_never_echo_the_requested_path() {
    let dir = fixture();
    let responses = run(
        dir.path(),
        &[modern_call(
            1,
            "project_validate",
            json!({"manifest":"../SECRET-PATH-MARKER"}),
        )],
    );
    assert!(!responses[0].to_string().contains("SECRET-PATH-MARKER"));
    assert!(
        !responses[0]
            .to_string()
            .contains(&dir.path().to_string_lossy().to_string())
    );
}

#[test]
fn rejected_paths_are_tool_errors_not_protocol_errors() {
    let dir = fixture();
    let responses = run(
        dir.path(),
        &[modern_call(1, "scene_inspect", json!({"scene":"../x"}))],
    );
    assert!(responses[0].get("error").is_none());
    assert_eq!(responses[0]["result"]["isError"], true);
}

// ---- tool behaviour --------------------------------------------------------------------

#[test]
fn project_validate_accepts_a_good_manifest() {
    let dir = fixture();
    let responses = run(
        dir.path(),
        &[modern_call(
            1,
            "project_validate",
            json!({"manifest":"project.json"}),
        )],
    );
    let result = &responses[0]["result"]["structuredContent"];
    assert_eq!(result["ok"], true);
    assert_eq!(result["command"], "project validate");
    assert_eq!(result["diagnostics"], json!([]));
    assert_eq!(result["project"], "Fixture");
    assert_eq!(result["assets"], 2);
}

#[test]
fn project_validate_defaults_to_the_root_manifest() {
    let dir = fixture();
    let responses = run(dir.path(), &[modern_call(1, "project_validate", json!({}))]);
    assert_eq!(responses[0]["result"]["isError"], false, "{}", responses[0]);
}

#[test]
fn tool_failures_are_structured_with_code_message_and_hint() {
    let dir = fixture();
    let responses = run(
        dir.path(),
        &[modern_call(1, "scene_inspect", json!({"scene":"../x"}))],
    );
    let error = &responses[0]["result"]["structuredContent"]["error"];
    assert_eq!(error["code"], "path_invalid");
    assert!(error["message"].as_str().unwrap().contains("scene path"));
    assert!(error["hint"].as_str().unwrap().contains("project-relative"));
}

#[test]
fn diagnostics_failures_carry_the_error_object_and_the_diagnostics() {
    let dir = fixture();
    fs::remove_file(dir.path().join("assets/white.ppm")).unwrap();
    let responses = run(dir.path(), &[modern_call(1, "project_validate", json!({}))]);
    let result = &responses[0]["result"]["structuredContent"];
    assert_eq!(result["ok"], false);
    let diagnostic = &result["diagnostics"][0];
    for key in ["code", "severity", "file", "path", "message"] {
        assert!(diagnostic.get(key).is_some(), "{key} missing: {result}");
    }
    assert_eq!(result["error"]["code"], diagnostic["code"]);
    assert!(result["error"]["hint"].is_string());
    let text = result.to_string();
    let folder = dir
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert!(
        !text.contains(&folder),
        "diagnostics must not reveal the host path: {text}"
    );
}

#[test]
fn project_validate_reports_diagnostics_for_a_broken_manifest() {
    let dir = fixture();
    fs::write(dir.path().join("broken.json"), "{ not json").unwrap();
    let responses = run(
        dir.path(),
        &[modern_call(
            1,
            "project_validate",
            json!({"manifest":"broken.json"}),
        )],
    );
    let result = &responses[0]["result"];
    assert_eq!(result["isError"], true);
    assert_eq!(result["structuredContent"]["ok"], false);
    assert!(
        !result["structuredContent"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn project_validate_reports_a_missing_asset_reference() {
    let dir = fixture();
    fs::remove_file(dir.path().join("assets/white.ppm")).unwrap();
    let responses = run(
        dir.path(),
        &[modern_call(
            1,
            "project_validate",
            json!({"manifest":"project.json"}),
        )],
    );
    assert_eq!(responses[0]["result"]["isError"], true);
    assert_eq!(responses[0]["result"]["structuredContent"]["ok"], false);
}

#[test]
fn project_validate_on_a_scene_file_is_a_diagnostic_not_a_crash() {
    let dir = fixture();
    let responses = run(
        dir.path(),
        &[modern_call(
            1,
            "project_validate",
            json!({"manifest":"scenes/start.json"}),
        )],
    );
    assert_eq!(responses[0]["result"]["isError"], true);
}

#[test]
fn scene_inspect_returns_entities_and_components() {
    let dir = fixture();
    let responses = run(
        dir.path(),
        &[modern_call(
            1,
            "scene_inspect",
            json!({"scene":"scenes/start.json"}),
        )],
    );
    let scene = &responses[0]["result"]["structuredContent"];
    assert_eq!(scene["scene_id"], "scene:start");
    assert_eq!(scene["entities"][0]["id"], "entity:a");
    assert_eq!(
        scene["entities"][0]["components"]["MeshRenderer"]["texture"],
        "asset:texture"
    );
}

#[test]
fn tool_text_content_mirrors_structured_content() {
    let dir = fixture();
    let responses = run(
        dir.path(),
        &[modern_call(
            1,
            "scene_inspect",
            json!({"scene":"scenes/start.json"}),
        )],
    );
    let result = &responses[0]["result"];
    assert_eq!(result["content"][0]["type"], "text");
    let parsed: Value =
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(parsed, result["structuredContent"]);
}

#[test]
fn scene_inspect_without_a_project_manifest_reports_the_missing_manifest() {
    let dir = empty_root();
    fs::write(
        dir.path().join("scene.json"),
        json!({"schema":1,"scene_id":"s","entities":[]}).to_string(),
    )
    .unwrap();
    let responses = run(
        dir.path(),
        &[modern_call(
            1,
            "scene_inspect",
            json!({"scene":"scene.json"}),
        )],
    );
    assert!(tool_error_text(&responses[0]).contains("project.json is missing"));
}

#[test]
fn scene_inspect_reports_diagnostics_for_unknown_asset_references() {
    let dir = fixture();
    fs::write(
        dir.path().join("scenes/bad.json"),
        json!({"schema":1,"scene_id":"scene:bad","entities":[
            {"id":"entity:x","name":"X","components":{"MeshRenderer":{"mesh":"asset:ghost","texture":"asset:texture"}}}
        ]})
        .to_string(),
    )
    .unwrap();
    let responses = run(
        dir.path(),
        &[modern_call(
            1,
            "scene_inspect",
            json!({"scene":"scenes/bad.json"}),
        )],
    );
    assert_eq!(responses[0]["result"]["isError"], true);
    assert_eq!(responses[0]["result"]["structuredContent"]["ok"], false);
}

#[test]
fn scene_inspect_reports_diagnostics_for_malformed_scene_json() {
    let dir = fixture();
    fs::write(dir.path().join("scenes/garbage.json"), "[[[").unwrap();
    let responses = run(
        dir.path(),
        &[modern_call(
            1,
            "scene_inspect",
            json!({"scene":"scenes/garbage.json"}),
        )],
    );
    assert_eq!(responses[0]["result"]["isError"], true);
}

#[test]
fn tools_never_modify_the_project_directory() {
    let dir = fixture();
    let snapshot = |root: &Path| {
        let mut entries: Vec<_> = walk(root);
        entries.sort();
        entries
    };
    fn walk(root: &Path) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        for entry in fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push((
                    path.to_string_lossy().into_owned(),
                    fs::read(&path).unwrap(),
                ));
            }
        }
        out
    }
    let before = snapshot(dir.path());
    run(
        dir.path(),
        &[
            modern_call(1, "project_validate", json!({"manifest":"project.json"})),
            modern_call(2, "scene_inspect", json!({"scene":"scenes/start.json"})),
            modern_call(3, "scene_inspect", json!({"scene":"../nope"})),
        ],
    );
    assert_eq!(snapshot(dir.path()), before);
}

#[test]
fn repeated_calls_are_idempotent() {
    let dir = fixture();
    let call = modern_call(1, "scene_inspect", json!({"scene":"scenes/start.json"}));
    let responses = run(dir.path(), &[call.clone(), call.clone(), call]);
    assert_eq!(responses[0], responses[1]);
    assert_eq!(responses[1], responses[2]);
}

#[test]
fn files_edited_between_calls_are_reread() {
    let dir = fixture();
    let call = modern_call(1, "scene_inspect", json!({"scene":"scenes/start.json"}));
    let first = run(dir.path(), std::slice::from_ref(&call));
    fs::write(
        dir.path().join("scenes/start.json"),
        json!({"schema":1,"scene_id":"scene:renamed","entities":[]}).to_string(),
    )
    .unwrap();
    let second = run(dir.path(), &[call]);
    assert_eq!(
        first[0]["result"]["structuredContent"]["scene_id"],
        "scene:start"
    );
    assert_eq!(
        second[0]["result"]["structuredContent"]["scene_id"],
        "scene:renamed"
    );
}

// ---- roots and transport failures ------------------------------------------------------

#[test]
fn nonexistent_root_is_invalid_root() {
    let dir = empty_root();
    let error = serve(
        dir.path().join("missing"),
        Cursor::new(Vec::new()),
        Vec::new(),
    )
    .unwrap_err();
    assert!(matches!(error, Error::InvalidRoot));
    assert_eq!(
        error.to_string(),
        "MCP project root is not an accessible directory"
    );
}

#[test]
fn file_as_root_is_invalid_root() {
    let dir = fixture();
    let error = serve(
        dir.path().join("project.json"),
        Cursor::new(Vec::new()),
        Vec::new(),
    )
    .unwrap_err();
    assert!(matches!(error, Error::InvalidRoot));
}

#[test]
fn invalid_root_produces_no_output() {
    let dir = empty_root();
    let mut out = Vec::new();
    let _ = serve(
        dir.path().join("nope"),
        Cursor::new(lines(&[legacy(1, "x")])),
        &mut out,
    );
    assert!(out.is_empty());
}

#[test]
fn relative_segments_in_the_root_are_canonicalised() {
    let dir = fixture();
    let sneaky = dir.path().join("scenes").join("..");
    let (responses, result) = {
        let mut out = Vec::new();
        let result = serve(
            sneaky,
            Cursor::new(lines(&[modern_call(
                1,
                "project_validate",
                json!({"manifest":"project.json"}),
            )])),
            &mut out,
        );
        (decode(&out), result)
    };
    result.unwrap();
    assert_eq!(responses[0]["result"]["isError"], false);
}

#[test]
fn framing_error_display_is_descriptive() {
    assert_eq!(
        Error::InvalidFraming.to_string(),
        "oversized or unterminated JSON-RPC message"
    );
}

#[test]
fn io_errors_expose_their_source_and_display() {
    let error = Error::from(io::Error::other("disk on fire"));
    assert_eq!(error.to_string(), "disk on fire");
    assert!(std::error::Error::source(&error).is_some());
    assert!(std::error::Error::source(&Error::InvalidRoot).is_none());
}

struct FailingWriter;
impl Write for FailingWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "client went away",
        ))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn writer_failure_is_returned_as_an_io_error() {
    let dir = empty_root();
    let error = serve(
        dir.path(),
        Cursor::new(lines(&[legacy(1, "x")])),
        FailingWriter,
    )
    .unwrap_err();
    assert!(matches!(error, Error::Io(ref e) if e.kind() == io::ErrorKind::BrokenPipe));
}

#[test]
fn notifications_do_not_touch_a_failing_writer() {
    let dir = empty_root();
    serve(
        dir.path(),
        Cursor::new(lines(&[initialized()])),
        FailingWriter,
    )
    .unwrap();
}

struct FailingReader;
impl io::Read for FailingReader {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::ConnectionReset,
            "stdin closed abruptly",
        ))
    }
}

#[test]
fn reader_failure_is_returned_as_an_io_error() {
    let dir = empty_root();
    let error = serve(dir.path(), BufReader::new(FailingReader), Vec::new()).unwrap_err();
    assert!(matches!(error, Error::Io(ref e) if e.kind() == io::ErrorKind::ConnectionReset));
}

// ---- output limit ----------------------------------------------------------------------

#[test]
fn oversized_legacy_tool_results_are_replaced_without_modern_metadata() {
    let dir = fixture();
    // Each sprite carries 4096 animation frames (~40 KiB); a default page of
    // 200 such entities is far beyond 1 MiB.
    let frames: Vec<Value> = (0..4096).map(|_| json!([0, 0, 1, 1])).collect();
    let entities: Vec<Value> = (0..200)
        .map(|i| {
            json!({"id": format!("entity:{i}"), "name": "x", "components": {
                "Transform": {"position": [0, 0, 0]},
                "SpriteRenderer": {"texture": "asset:texture", "frames": frames}}})
        })
        .collect();
    fs::write(
        dir.path().join("scenes/huge.json"),
        json!({"schema":1,"scene_id":"scene:huge","entities":entities}).to_string(),
    )
    .unwrap();
    let mut requests = handshake();
    requests.push(legacy_with(
        9,
        "tools/call",
        json!({"name":"scene_inspect","arguments":{"scene":"scenes/huge.json"}}),
    ));
    requests.push(legacy(10, "ping"));
    let responses = run(dir.path(), &requests);
    assert_eq!(responses[1]["id"], 9);
    assert_eq!(responses[1]["result"]["isError"], true);
    assert!(responses[1]["result"].get("resultType").is_none());
    let error = &responses[1]["result"]["structuredContent"]["error"];
    assert!(error["message"].as_str().unwrap().contains("1 MiB"));
    assert_eq!(error["code"], "output_too_large");
    assert!(error["hint"].is_string());
    assert_eq!(
        responses[2]["result"],
        json!({}),
        "the session continues after the replacement"
    );
}
