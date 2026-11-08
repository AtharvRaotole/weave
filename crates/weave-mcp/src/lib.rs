//! A small Model Context Protocol server for Weave.
//!
//! Tools:
//! - `weave_check` statically checks a script
//! - `weave_run` runs it against scripted echo/const tools
//! - `weave_replay` re-executes a script from a JSON trace

use std::collections::BTreeMap;
use std::thread;
use std::time::Duration;

use serde_json::{json, Value as Json};
use weave::{Effect, Engine, ToolSpec, Value};

pub fn handle_message(message: &Json) -> Option<Json> {
    let method = message.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = message.get("id").cloned();
    if id.is_none() {
        return None;
    }
    let params = message.get("params").cloned().unwrap_or(json!({}));
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "weave", "version": "0.1.0"}
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools_list()),
        "tools/call" => tools_call(&params),
        other => Err((
            -32601,
            format!("unknown method `{other}`"),
        )),
    };
    Some(match result {
        Ok(value) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": value
        }),
        Err((code, message)) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": code, "message": message}
        }),
    })
}

fn tools_list() -> Json {
    json!({
        "tools": [
            {
                "name": "weave_check",
                "description": "Statically check a Weave script. Returns one short error per line, or 'ok'.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "source": {"type": "string"},
                        "tools": {"type": "array", "items": tool_schema()}
                    },
                    "required": ["source"]
                }
            },
            {
                "name": "weave_run",
                "description": "Run a Weave script. Scripted tools are echo or const. Pure and read calls run in parallel; irreversible calls wait until the script is complete.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "source": {"type": "string"},
                        "tools": {"type": "array", "items": tool_schema()}
                    },
                    "required": ["source"]
                }
            },
            {
                "name": "weave_replay",
                "description": "Replay a Weave script from a JSON trace without calling tools. Output matches the recorded run.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "source": {"type": "string"},
                        "trace": {"type": "object"}
                    },
                    "required": ["source", "trace"]
                }
            }
        ]
    })
}

fn tool_schema() -> Json {
    json!({
        "type": "object",
        "properties": {
            "name": {"type": "string"},
            "effect": {"type": "string", "enum": ["pure", "read", "reversible", "irreversible"]},
            "arity": {"type": "integer"},
            "behavior": {"type": "string", "enum": ["echo", "const"]},
            "result": {"type": "object", "description": "Tagged Weave value used when behavior is const"},
            "delay_ms": {"type": "integer"}
        },
        "required": ["name"]
    })
}

fn tools_call(params: &Json) -> Result<Json, (i32, String)> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or((-32602, "tools/call needs a name".to_string()))?;
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let text = match name {
        "weave_check" => weave_check(&args),
        "weave_run" => weave_run(&args),
        "weave_replay" => weave_replay(&args),
        other => return Err((-32602, format!("unknown tool `{other}`"))),
    };
    match text {
        Ok(body) => Ok(tool_ok(&body)),
        Err(message) => Ok(tool_err(&message)),
    }
}

fn tool_ok(text: &str) -> Json {
    json!({
        "content": [{"type": "text", "text": text}],
        "isError": false
    })
}

fn tool_err(text: &str) -> Json {
    json!({
        "content": [{"type": "text", "text": text}],
        "isError": true
    })
}

fn source_of(args: &Json) -> Result<&str, String> {
    args.get("source")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "source must be a string".to_string())
}

fn weave_check(args: &Json) -> Result<String, String> {
    let source = source_of(args)?;
    let engine = engine_from_args(args)?;
    let diags = engine.check(source);
    if diags.is_empty() {
        Ok("ok".to_string())
    } else {
        Err(diags.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n"))
    }
}

fn weave_run(args: &Json) -> Result<String, String> {
    let source = source_of(args)?;
    let engine = engine_from_args(args)?;
    let result = engine.run(source).map_err(|err| err.to_string())?;
    Ok(render_result(&result))
}

fn weave_replay(args: &Json) -> Result<String, String> {
    let source = source_of(args)?;
    let trace_json = match args.get("trace") {
        Some(Json::String(text)) => text.clone(),
        Some(value) => serde_json::to_string(value).map_err(|err| err.to_string())?,
        None => return Err("trace is required".into()),
    };
    let trace = weave::Trace::from_json(&trace_json).map_err(|err| err.to_string())?;
    let engine = engine_from_args(args)?;
    let result = engine.replay(source, &trace).map_err(|err| err.to_string())?;
    Ok(render_result(&result))
}

fn render_result(result: &weave::RunResult) -> String {
    let mut bindings = serde_json::Map::new();
    for (name, value) in &result.bindings {
        bindings.insert(name.clone(), value.to_json());
    }
    serde_json::to_string_pretty(&json!({
        "stdout": result.stdout(),
        "prints": result.prints,
        "bindings": bindings,
        "trace": result.trace
    }))
    .unwrap_or_else(|_| "{}".into())
}

fn engine_from_args(args: &Json) -> Result<Engine, String> {
    let mut engine = Engine::new();
    let Some(tools) = args.get("tools") else {
        return Ok(engine);
    };
    let tools = tools.as_array().ok_or("tools must be a list")?;
    for tool in tools {
        register_scripted(&mut engine, tool)?;
    }
    Ok(engine)
}

fn register_scripted(engine: &mut Engine, tool: &Json) -> Result<(), String> {
    let name = tool
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("tool name must be a string")?
        .to_string();
    let effect = match tool.get("effect").and_then(|v| v.as_str()) {
        None => Effect::Irreversible,
        Some(text) => Effect::parse(text).ok_or_else(|| {
            format!("unknown effect `{text}`; use pure, read, reversible, or irreversible")
        })?,
    };
    let mut spec = ToolSpec::effect(effect);
    if let Some(arity) = tool.get("arity").and_then(|v| v.as_u64()) {
        spec = spec.with_arity(arity as usize);
    }
    let behavior = tool
        .get("behavior")
        .and_then(|v| v.as_str())
        .unwrap_or("echo")
        .to_string();
    let delay_ms = tool.get("delay_ms").and_then(|v| v.as_u64()).unwrap_or(0);
    let constant = match tool.get("result") {
        Some(value) => Some(Value::from_json(value).map_err(|err| format!("tool `{name}` result: {err}"))?),
        None => None,
    };
    if behavior == "const" && constant.is_none() {
        return Err(format!("tool `{name}` behavior const needs result"));
    }
    engine.register(&name, spec, move |args, kwargs| {
        if delay_ms > 0 {
            thread::sleep(Duration::from_millis(delay_ms));
        }
        match behavior.as_str() {
            "const" => Ok(constant.clone().unwrap_or(Value::Null)),
            "echo" => Ok(echo_value(&args, &kwargs)),
            other => Err(format!("unknown tool behavior `{other}`; use echo or const")),
        }
    });
    Ok(())
}

fn echo_value(args: &[Value], kwargs: &BTreeMap<String, Value>) -> Value {
    if kwargs.is_empty() && args.len() == 1 {
        args[0].clone()
    } else if kwargs.is_empty() {
        Value::List(args.to_vec())
    } else {
        let mut items = Vec::new();
        for (key, value) in kwargs {
            items.push((Value::str(key), value.clone()));
        }
        Value::Dict(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lists_and_runs_an_echo_tool() {
        let init = handle_message(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        }))
        .unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "weave");

        let listed = handle_message(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list"
        }))
        .unwrap();
        let names: Vec<&str> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["weave_check", "weave_run", "weave_replay"]);

        let ran = handle_message(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "weave_run",
                "arguments": {
                    "source": "a = fetch(\"x\")\nb = fetch(\"y\")\nprint(a)\nprint(b)\n",
                    "tools": [{"name": "fetch", "effect": "read", "arity": 1, "behavior": "echo"}]
                }
            }
        }))
        .unwrap();
        let text = ran["result"]["content"][0]["text"].as_str().unwrap();
        assert!(ran["result"]["isError"] == false, "{text}");
        let body: Json = serde_json::from_str(text).unwrap_or_else(|err| panic!("{err}\nTEXT:\n{text}"));
        assert_eq!(body["stdout"], "x\ny\n");

        let trace = body["trace"].clone();
        let replayed = handle_message(&json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "weave_replay",
                "arguments": {
                    "source": "a = fetch(\"x\")\nb = fetch(\"y\")\nprint(a)\nprint(b)\n",
                    "trace": trace,
                    "tools": [{"name": "fetch", "effect": "read", "arity": 1, "behavior": "echo"}]
                }
            }
        }))
        .unwrap();
        let replay_text = replayed["result"]["content"][0]["text"].as_str().unwrap();
        assert!(replayed["result"]["isError"] == false, "{replay_text}");
        let replay_body: Json = serde_json::from_str(replay_text).unwrap();
        assert_eq!(replay_body["stdout"], body["stdout"]);
        assert_eq!(replay_body["bindings"], body["bindings"]);
    }

    #[test]
    fn check_reports_a_short_error() {
        let response = handle_message(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "weave_check",
                "arguments": {"source": "import os\n"}
            }
        }))
        .unwrap();
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("`import` is not supported"), "{text}");
        assert_eq!(response["result"]["isError"], true);
    }
}
