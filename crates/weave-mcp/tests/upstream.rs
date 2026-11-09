use std::time::Instant;

use serde_json::{json, Value as Json};
use weave_mcp::{handle_with, ServerConfig};

fn config() -> ServerConfig {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake_mcp_server.py");
    let python = std::env::var("PYTHON").unwrap_or_else(|_| "python3".into());
    ServerConfig::from_json(
        &json!({"upstreams": [{"name": "notes", "command": python, "args": [script], "prefix": "n_"}]}).to_string(),
    )
    .unwrap()
}

fn call(config: &ServerConfig, name: &str, arguments: Json) -> Json {
    let message = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": arguments}});
    handle_with(&message, config).unwrap()["result"].clone()
}

fn text(result: &Json) -> String {
    result["content"][0]["text"].as_str().unwrap().to_string()
}

#[test]
fn upstream_tools_are_listed_with_effects_from_annotations() {
    let config = config();
    let list = handle_with(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}), &config).unwrap();
    let names: Vec<_> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    assert!(names.contains(&"n_read_note".to_string()) && names.contains(&"n_write_note".to_string()), "{names:?}");
    let listing = weave_mcp::script_tool_listing(&config);
    assert!(listing.contains("n_read_note(key), effect read"), "{listing}");
    assert!(listing.contains("n_write_note(key, text), effect irreversible"), "{listing}");
}

#[test]
fn proxied_call_and_tool_error() {
    let config = config();
    assert_eq!(text(&call(&config, "n_read_note", json!({"key": "a"}))), "alpha");
    let missing = call(&config, "n_read_note", json!({"key": "zz"}));
    assert_eq!(missing["isError"], json!(true));
}

#[test]
fn scripts_call_upstream_tools_in_parallel_and_catch_failures() {
    let config = config();
    let source = "a = n_read_note(\"a\")\nb = n_read_note(key=\"b\")\ntry:\n    c = n_read_note(\"zz\")\nexcept ToolError:\n    c = \"none\"\nn_write_note(\"c\", a + \"+\" + b)\nprint(a, b, c)\n";
    let started = Instant::now();
    let result = call(&config, "weave_run", json!({"source": source}));
    let elapsed = started.elapsed();
    let body: Json = serde_json::from_str(&text(&result)).unwrap_or_else(|_| panic!("{}", text(&result)));
    assert_eq!(body["stdout"], json!("alpha beta none\n"), "{body}");
    // Three 200 ms reads overlap; sequential would take at least 600 ms.
    assert!(elapsed.as_millis() < 550, "took {elapsed:?}");
    assert_eq!(text(&call(&config, "n_read_note", json!({"key": "c"}))), "alpha+beta");
}

#[test]
fn bad_arguments_are_reported_before_calling() {
    let config = config();
    let result = call(&config, "weave_run", json!({"source": "x = n_write_note(\"k\")\n"}));
    let message = text(&result);
    assert!(message.contains("missing required argument(s): text"), "{message}");
}

#[test]
fn output_schema_title_and_annotations_are_forwarded() {
    let config = config();
    let list = handle_with(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}), &config).unwrap();
    let stats = list["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == "n_stats").unwrap().clone();
    assert_eq!(stats["outputSchema"]["required"], json!(["count", "keys"]), "{stats}");
    assert_eq!(stats["outputSchema"]["properties"]["count"], json!({"type": "integer"}));
    assert_eq!(stats["title"], json!("Note statistics"));
    assert_eq!(stats["annotations"], json!({"readOnlyHint": true, "idempotentHint": true}));
    let write = list["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == "n_write_note").unwrap().clone();
    assert!(write.get("outputSchema").is_none());
    assert_eq!(write["annotations"]["readOnlyHint"], json!(false));
    let listing = weave_mcp::script_tool_listing(&config);
    assert!(listing.contains("n_stats(), effect read") && listing.contains("returns: {count: int, keys: list[str]}"), "{listing}");
}

#[test]
fn structured_results_reach_scripts_and_direct_calls_unchanged() {
    let config = config();
    let direct = call(&config, "n_stats", json!({}));
    assert_eq!(direct["structuredContent"]["count"], json!(direct["structuredContent"]["keys"].as_array().unwrap().len()));
    let result = call(&config, "weave_run", json!({"source": "s = n_stats()\nprint(s[\"count\"] == len(s[\"keys\"]), type_ok(s))\n".replace("type_ok(s)", "\"keys\" in s")}));
    let body: Json = serde_json::from_str(&text(&result)).unwrap_or_else(|_| panic!("{}", text(&result)));
    assert_eq!(body["stdout"], json!("True True\n"), "{body}");
}
