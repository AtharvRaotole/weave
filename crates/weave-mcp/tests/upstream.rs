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

fn config_with(extra: Json) -> ServerConfig {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake_mcp_server.py");
    let python = std::env::var("PYTHON").unwrap_or_else(|_| "python3".into());
    let mut upstream = json!({"name": "notes", "command": python, "args": [script], "prefix": "n_"});
    for (k, v) in extra.as_object().unwrap() {
        upstream[k] = v.clone();
    }
    ServerConfig::from_json(&json!({"upstreams": [upstream]}).to_string()).unwrap()
}

#[test]
fn weave_check_uses_the_upstream_input_schema() {
    let config = config();
    let ok = call(&config, "weave_check", json!({"source": "r = n_search_notes(\"a\", limit=3, mode=\"fuzzy\")\n"}));
    assert_eq!(text(&ok), "ok");
    let bad = call(&config, "weave_check", json!({"source": "r = n_search_notes(\"a\", limt=3)\ns = n_search_notes(\"a\", mode=\"regex\")\nt = n_search_notes(5)\n"}));
    assert_eq!(bad["isError"], json!(true));
    let message = text(&bad);
    assert!(message.contains("tool `n_search_notes` has no parameter `limt` hint: did you mean `limit`?"), "{message}");
    assert!(message.contains("argument `mode` of tool `n_search_notes` must be one of 'exact', 'fuzzy', got str 'regex'"), "{message}");
    assert!(message.contains("argument `query` of tool `n_search_notes` should be str, got int 5"), "{message}");
}

#[test]
fn a_bad_call_late_in_the_script_stops_an_earlier_write() {
    let config = config();
    let source = "n_write_note(\"early\", \"should not land\")\nr = n_read_note(kee=\"a\")\n";
    let result = call(&config, "weave_run", json!({"source": source}));
    assert_eq!(result["isError"], json!(true));
    assert!(text(&result).contains("has no parameter `kee` hint: did you mean `key`?"), "{}", text(&result));
    let probe = call(&config, "n_read_note", json!({"key": "early"}));
    assert_eq!(probe["isError"], json!(true), "the write must not have happened: {probe}");
}

#[test]
fn computed_values_are_checked_before_they_reach_the_upstream() {
    let config = config();
    let source = "n = str(2 + 3)\ntry:\n    r = n_search_notes(\"a\", limit=n)\nexcept ToolError as e:\n    r = \"caught: \" + str(e)\nok = n_search_notes(\"a\", limit=int(n))\nprint(r)\nprint(ok)\n";
    let result = call(&config, "weave_run", json!({"source": source}));
    let body: Json = serde_json::from_str(&text(&result)).unwrap_or_else(|_| panic!("{}", text(&result)));
    let stdout = body["stdout"].as_str().unwrap();
    assert!(stdout.contains("caught: ") && stdout.contains("argument `limit` of tool `n_search_notes` should be int, got str '5'"), "{stdout}");
    assert!(stdout.contains(r#"{"limit": 5, "query": "a"}"#), "{stdout}");
}

#[test]
fn check_args_false_turns_the_schema_checks_off() {
    let config = config_with(json!({"check_args": false}));
    let ok = call(&config, "weave_check", json!({"source": "r = n_search_notes(\"a\", limt=3)\n"}));
    assert_eq!(text(&ok), "ok");
    let result = call(&config, "weave_run", json!({"source": "print(n_search_notes(\"a\", limit=\"7\", extra=1))\n"}));
    let body: Json = serde_json::from_str(&text(&result)).unwrap_or_else(|_| panic!("{}", text(&result)));
    assert_eq!(body["stdout"], json!("{\"extra\": 1, \"limit\": \"7\", \"query\": \"a\"}\n"));
    assert!(ServerConfig::from_json(&json!({"upstreams": [{"name": "x", "command": "true", "check_args": "no"}]}).to_string())
        .unwrap_err()
        .contains("check_args must be true or false"));
}
