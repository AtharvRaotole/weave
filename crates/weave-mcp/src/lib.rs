//! A small Model Context Protocol server for Weave.
//!
//! Tools:
//! - `weave_check` statically checks a script
//! - `weave_run` runs it against scripted echo/const tools
//! - `weave_replay` re-executes a script from a JSON trace

use std::collections::BTreeMap;
use std::thread;
use std::time::Duration;

use std::sync::Arc;

use serde_json::{json, Value as Json};
use weave::{Effect, Engine, ToolSpec, Value};

pub mod framing;
pub mod upstream;
pub use upstream::{Upstream, UpstreamConfig};

/// Tools the server exposes directly, in addition to `weave_check`, `weave_run`,
/// and `weave_replay`. Loaded from a JSON config.
#[derive(Clone, Debug)]
pub struct ScriptedTool {
    pub name: String,
    pub description: String,
    pub effect: Effect,
    pub arity: Option<usize>,
    pub behavior: String,
    pub result: Option<Value>,
    pub delay_ms: u64,
    pub reads: Option<weave::Footprint>,
    pub writes: Option<weave::Footprint>,
}

#[derive(Clone, Debug, Default)]
pub struct ServerConfig {
    pub tools: Vec<ScriptedTool>,
    /// Other MCP servers whose tools Weave scripts can call.
    pub upstreams: Vec<Arc<Upstream>>,
}

impl ServerConfig {
    pub fn from_json(text: &str) -> Result<Self, String> {
        let value: Json = serde_json::from_str(text).map_err(|err| err.to_string())?;
        let empty = Vec::new();
        let tools = match value.get("tools") {
            None => &empty,
            Some(v) => v.as_array().ok_or("tools must be a list")?,
        };
        let upstream_list = match value.get("upstreams") {
            None => &empty,
            Some(v) => v.as_array().ok_or("upstreams must be a list")?,
        };
        if value.get("tools").is_none() && value.get("upstreams").is_none() {
            return Err("config needs a tools list or an upstreams list".into());
        }
        let mut parsed = Vec::with_capacity(tools.len());
        for tool in tools {
            parsed.push(parse_scripted(tool)?);
        }
        let mut upstreams = Vec::new();
        for item in upstream_list {
            upstreams.push(Upstream::connect(UpstreamConfig::parse(item)?)?);
        }
        let config = Self { tools: parsed, upstreams };
        config.validate_names()?;
        Ok(config)
    }

    fn validate_names(&self) -> Result<(), String> {
        let mut owners = BTreeMap::new();
        let mut insert = |name: &str, owner: &str| -> Result<(), String> {
            if reserved_tool_name(name) {
                return Err(format!("tool `{name}` from `{owner}` collides with a reserved runtime name"));
            }
            if let Some(old) = owners.insert(name.to_string(), owner.to_string()) {
                return Err(format!("duplicate tool `{name}` from `{old}` and `{owner}`; use unique prefixes"));
            }
            Ok(())
        };
        for tool in &self.tools { insert(&tool.name, "scripted tools")?; }
        for upstream in &self.upstreams {
            for tool in &upstream.tools { insert(&tool.name, &upstream.config.name)?; }
        }
        Ok(())
    }

    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
        Self::from_json(&text)
    }
}

fn reserved_tool_name(name: &str) -> bool {
    name.starts_with("weave_") || name.starts_with("__weave_") || matches!(name, "print" | "len" | "range" | "sum" | "min" | "max" |
        "sorted" | "reversed" | "enumerate" | "zip" | "any" | "all" | "list" | "str" | "int" | "parse_json")
}

pub fn handle_message(message: &Json) -> Option<Json> {
    handle_with(message, &ServerConfig::default())
}

pub fn handle_with(message: &Json, config: &ServerConfig) -> Option<Json> {
    let method = message.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = message.get("id").cloned();
    id.as_ref()?;
    let params = message.get("params").cloned().unwrap_or(json!({}));
    let result = if let Err(err) = config.validate_names() {
        Err((-32602, err))
    } else { match method {
        "initialize" => Ok(json!({
            "protocolVersion": negotiated_version(&params),
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "weave", "version": "0.1.0"}
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools_list(config)),
        "tools/call" => tools_call(&params, config),
        other => Err((
            -32601,
            format!("unknown method `{other}`"),
        )),
    }
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

/// Answer with the client's protocol version when it is one we speak.
fn negotiated_version(params: &Json) -> &'static str {
    const KNOWN: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
    let asked = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("");
    KNOWN.iter().copied().find(|v| *v == asked).unwrap_or("2024-11-05")
}

fn tools_list(config: &ServerConfig) -> Json {
    let mut tools = vec![
        json!({
            "name": "weave_check",
            "description": "Statically check a Weave script. Returns one short error per line, or 'ok'.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source": {"type": "string"},
                    "debug": {"type": "boolean", "description": "Include bindings and full trace (may contain sensitive intermediate data); default false"},
                    "tools": {"type": "array", "items": tool_schema()}
                },
                "required": ["source"]
            }
        }),
        json!({
            "name": "weave_run",
            "description": "Run a Weave script. Pure and read calls run in parallel; irreversible calls wait until the script is complete. Omit tools to use the server config (scripted tools and upstream MCP tools).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source": {"type": "string"},
                    "debug": {"type": "boolean", "description": "Include bindings and full trace (may contain sensitive intermediate data); default false"},
                    "tools": {"type": "array", "items": tool_schema()}
                },
                "required": ["source"]
            }
        }),
        json!({
            "name": "weave_replay",
            "description": "Replay a Weave script from a JSON trace without calling tools. Output matches the recorded run.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source": {"type": "string"},
                    "debug": {"type": "boolean", "description": "Include bindings and full trace (may contain sensitive intermediate data); default false"},
                    "trace": {"type": "object"}
                },
                "required": ["source", "trace"]
            }
        }),
    ];
    for tool in &config.tools {
        tools.push(json!({
            "name": tool.name,
            "description": tool.description,
            "annotations": {"readOnlyHint": tool.effect <= Effect::Read},
            "inputSchema": {
                "type": "object",
                "properties": {
                    "args": {"type": "array", "description": "Positional arguments"},
                    "kwargs": {"type": "object", "description": "Keyword arguments"}
                }
            }
        }));
    }
    for upstream in &config.upstreams {
        for tool in &upstream.tools {
            tools.push(json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": tool.input_schema,
                "annotations": {"readOnlyHint": tool.effect <= Effect::Read},
            }));
        }
    }
    if let Some(run) = tools.iter_mut().find(|t| t["name"] == "weave_run") {
        let listing = script_tool_listing(config);
        if !listing.is_empty() {
            let base = run["description"].as_str().unwrap_or("").to_string();
            run["description"] = json!(format!("{base}\nTools callable from the script:\n{listing}"));
        }
    }
    json!({ "tools": tools })
}

/// One line per tool a script can call, with parameters in positional order.
pub fn script_tool_listing(config: &ServerConfig) -> String {
    let mut lines = Vec::new();
    for upstream in &config.upstreams {
        for tool in &upstream.tools {
            let summary = tool.description.lines().next().unwrap_or("");
            let params: Vec<String> = tool
                .params
                .iter()
                .map(|p| if tool.required.contains(p) { p.clone() } else { format!("{p}=None") })
                .collect();
            lines.push(format!(
                "{}({}), effect {}. {}",
                tool.name,
                params.join(", "),
                tool.effect.as_str(),
                summary
            ));
            // Spell out structured parameters (lists of objects, objects):
            // a script cannot guess their field names the way a native
            // function call reads them from the JSON schema.
            let props = tool.input_schema.get("properties").and_then(|v| v.as_object());
            for param in &tool.params {
                let Some(schema) = props.and_then(|p| p.get(param)) else { continue };
                if is_structured(schema) {
                    lines.push(format!("  {param}: {}", compact_type(schema, 0)));
                }
            }
            if let Some(shape) = tool.shape {
                lines.push(format!("  returns: {}", shape.describe()));
            }
            if let Some((arguments, output)) = &tool.example {
                let output = match tool.shape {
                    Some(shape) => match crate::upstream::shape_result(Some(shape), output.clone()) {
                        Ok(value) => value.to_string(),
                        Err(_) => output.clone(),
                    },
                    None => format!("{output:?}"),
                };
                // Show the sample as a Weave call with keyword arguments.
                let call_args: Vec<String> = arguments
                    .as_object()
                    .map(|map| {
                        map.iter()
                            .map(|(k, v)| format!("{k}={}", crate::upstream::from_plain_json(v)))
                            .collect()
                    })
                    .unwrap_or_default();
                lines.push(format!("  example: {}({}) -> {}", tool.name, call_args.join(", "), output));
            }
        }
    }
    lines.join("\n")
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

fn tools_call(params: &Json, config: &ServerConfig) -> Result<Json, (i32, String)> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or((-32602, "tools/call needs a name".to_string()))?;
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let text = match name {
        "weave_check" => weave_check(&args, config),
        "weave_run" => weave_run(&args, config),
        "weave_replay" => weave_replay(&args, config),
        other => match config.tools.iter().find(|tool| tool.name == other) {
            Some(tool) => call_configured(tool, &args),
            None => match config
                .upstreams
                .iter()
                .find_map(|u| u.tools.iter().find(|t| t.name == other).map(|t| (u, t)))
            {
                Some((upstream, tool)) => return upstream.request("tools/call", json!({"name": tool.upstream_name, "arguments": args}))
                    .map_err(|err| (-32603, err)),
                None => return Err((-32602, format!("unknown tool `{other}`"))),
            },
        },
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

fn weave_check(args: &Json, config: &ServerConfig) -> Result<String, String> {
    let source = source_of(args)?;
    let engine = engine_from_args(args, config)?;
    let diags = engine.check(source);
    if diags.is_empty() {
        Ok("ok".to_string())
    } else {
        Err(diags.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n"))
    }
}

fn weave_run(args: &Json, config: &ServerConfig) -> Result<String, String> {
    let source = source_of(args)?;
    let engine = engine_from_args(args, config)?;
    let result = engine.run(source).map_err(|err| err.to_string())?;
    Ok(if args.get("debug").and_then(Json::as_bool) == Some(true) { render_result(&result) }
       else { json!({"stdout": result.stdout(), "status": "ok"}).to_string() })
}

fn weave_replay(args: &Json, config: &ServerConfig) -> Result<String, String> {
    let source = source_of(args)?;
    let trace_json = match args.get("trace") {
        Some(Json::String(text)) => text.clone(),
        Some(value) => serde_json::to_string(value).map_err(|err| err.to_string())?,
        None => return Err("trace is required".into()),
    };
    let trace = weave::Trace::from_json(&trace_json).map_err(|err| err.to_string())?;
    let engine = engine_from_args(args, config)?;
    let result = engine.replay(source, &trace).map_err(|err| err.to_string())?;
    Ok(if args.get("debug").and_then(Json::as_bool) == Some(true) { render_result(&result) }
       else { json!({"stdout": result.stdout(), "status": "ok"}).to_string() })
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

fn engine_from_args(args: &Json, config: &ServerConfig) -> Result<Engine, String> {
    config.validate_names()?;
    let mut engine = Engine::new();
    for upstream in &config.upstreams {
        upstream.register_all(&mut engine);
    }
    if let Some(tools) = args.get("tools") {
        let tools = tools.as_array().ok_or("tools must be a list")?;
        let parsed = tools.iter().map(parse_scripted).collect::<Result<Vec<_>, _>>()?;
        let request_config = ServerConfig { tools: parsed, upstreams: config.upstreams.clone() };
        request_config.validate_names()?;
        for tool in &request_config.tools {
            register_parsed(&mut engine, tool)?;
        }
        return Ok(engine);
    }
    for tool in &config.tools {
        register_parsed(&mut engine, tool)?;
    }
    Ok(engine)
}

fn call_configured(tool: &ScriptedTool, args: &Json) -> Result<String, String> {
    let positional = match args.get("args") {
        None => Vec::new(),
        Some(value) => value
            .as_array()
            .ok_or("args must be a list")?
            .iter()
            .map(arg_value)
            .collect::<Result<Vec<_>, _>>()?,
    };
    let mut kwargs = BTreeMap::new();
    if let Some(raw) = args.get("kwargs") {
        let obj = raw.as_object().ok_or("kwargs must be an object")?;
        for (key, value) in obj {
            kwargs.insert(key.clone(), arg_value(value)?);
        }
    }
    if let Some(arity) = tool.arity {
        if positional.len() != arity {
            return Err(format!(
                "tool `{}` expects {arity} argument(s), got {}",
                tool.name,
                positional.len()
            ));
        }
    }
    let value = run_behavior(&tool.behavior, tool.result.clone(), tool.delay_ms, &positional, &kwargs)?;
    Ok(serde_json::to_string_pretty(&value.to_json()).unwrap_or_else(|_| "{}".into()))
}

fn arg_value(value: &Json) -> Result<Value, String> {
    if value.as_object().and_then(|obj| obj.get("t")).is_some() {
        return Value::from_json(value);
    }
    match value {
        Json::Null => Ok(Value::Null),
        Json::Bool(flag) => Ok(Value::Bool(*flag)),
        Json::Number(number) => {
            if let Some(int) = number.as_i64() {
                Ok(Value::Int(int))
            } else if let Some(float) = number.as_f64() {
                Ok(Value::Float(float))
            } else {
                Err("number is out of range".into())
            }
        }
        Json::String(text) => Ok(Value::Str(text.clone())),
        Json::Array(items) => items.iter().map(arg_value).collect::<Result<Vec<_>, _>>().map(Value::List),
        Json::Object(_) => Err("keyword values must be plain JSON or a tagged Weave value".into()),
    }
}

fn parse_scripted(tool: &Json) -> Result<ScriptedTool, String> {
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
    let behavior = tool
        .get("behavior")
        .and_then(|v| v.as_str())
        .unwrap_or("echo")
        .to_string();
    let result = match tool.get("result") {
        Some(value) => Some(Value::from_json(value).map_err(|err| format!("tool `{name}` result: {err}"))?),
        None => None,
    };
    if behavior == "const" && result.is_none() {
        return Err(format!("tool `{name}` behavior const needs result"));
    }
    if behavior != "echo" && behavior != "const" {
        return Err(format!("unknown tool behavior `{behavior}`; use echo or const"));
    }
    let reads = footprint_field(tool, "reads").map_err(|err| format!("tool `{name}`: {err}"))?;
    let writes = footprint_field(tool, "writes").map_err(|err| format!("tool `{name}`: {err}"))?;
    Ok(ScriptedTool {
        name,
        description: tool
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("Scripted Weave tool")
            .to_string(),
        effect,
        arity: tool.get("arity").and_then(|v| v.as_u64()).map(|n| n as usize),
        behavior,
        result,
        delay_ms: tool.get("delay_ms").and_then(|v| v.as_u64()).unwrap_or(0),
        reads,
        writes,
    })
}

fn register_parsed(engine: &mut Engine, tool: &ScriptedTool) -> Result<(), String> {
    let mut spec = ToolSpec::effect(tool.effect);
    if let Some(arity) = tool.arity {
        spec = spec.with_arity(arity);
    }
    if let Some(reads) = &tool.reads {
        spec = spec.with_reads(reads.clone());
    }
    if let Some(writes) = &tool.writes {
        spec = spec.with_writes(writes.clone());
    }
    let behavior = tool.behavior.clone();
    let delay_ms = tool.delay_ms;
    let constant = tool.result.clone();
    engine.register(&tool.name, spec, move |args, kwargs| {
        run_behavior(&behavior, constant.clone(), delay_ms, &args, &kwargs)
    });
    Ok(())
}

fn run_behavior(
    behavior: &str,
    constant: Option<Value>,
    delay_ms: u64,
    args: &[Value],
    kwargs: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    if delay_ms > 0 {
        thread::sleep(Duration::from_millis(delay_ms));
    }
    match behavior {
        "const" => Ok(constant.unwrap_or(Value::Null)),
        "echo" => Ok(echo_value(args, kwargs)),
        other => Err(format!("unknown tool behavior `{other}`; use echo or const")),
    }
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

/// `"reads"` / `"writes"`: `"*"` or a list of resource names.
fn footprint_field(tool: &Json, key: &str) -> Result<Option<weave::Footprint>, String> {
    match tool.get(key) {
        None => Ok(None),
        Some(Json::String(text)) => Ok(Some(weave::Footprint::parse(text))),
        Some(Json::Array(items)) => {
            let mut names = Vec::new();
            for item in items {
                names.push(
                    item.as_str()
                        .ok_or_else(|| format!("`{key}` entries must be strings"))?
                        .to_string(),
                );
            }
            Ok(Some(weave::Footprint::parse(&names.join(","))))
        }
        Some(_) => Err(format!("`{key}` must be \"*\" or a list of strings")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parsed_config_reads_footprints() {
        let tool = parse_scripted(&json!({
            "name": "get_user", "effect": "read",
            "reads": ["user:{0}"], "writes": "*"
        }))
        .unwrap();
        assert_eq!(tool.reads, Some(weave::Footprint::parse("user:{0}")));
        assert_eq!(tool.writes, Some(weave::Footprint::All));
        let bad = parse_scripted(&json!({"name": "x", "reads": 3})).unwrap_err();
        assert!(bad.contains("`reads` must be"), "{bad}");
    }

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
                "arguments": {"debug": true,
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
                "arguments": {"debug": true,
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

    #[test]
    fn config_tools_are_listed_and_callable() {
        let config = ServerConfig::from_json(
            r#"{"tools":[{"name":"weather","description":"City weather","effect":"read","arity":1,"behavior":"echo"}]}"#,
        )
        .unwrap();
        let listed = handle_with(
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            &config,
        )
        .unwrap();
        let names: Vec<&str> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["weave_check", "weave_run", "weave_replay", "weather"]
        );
        let called = handle_with(
            &json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": "weather", "arguments": {"args": ["Paris"]}}
            }),
            &config,
        )
        .unwrap();
        let text = called["result"]["content"][0]["text"].as_str().unwrap();
        assert_eq!(called["result"]["isError"], false);
        assert!(text.contains("Paris"), "{text}");

        let ran = handle_with(
            &json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "weave_run",
                    "arguments": {"source": "print(weather(\"Paris\"))\n"}
                }
            }),
            &config,
        )
        .unwrap();
        let body = ran["result"]["content"][0]["text"].as_str().unwrap();
        assert!(ran["result"]["isError"] == false, "{body}");
        assert!(body.contains("Paris"), "{body}");
    }
}

fn is_structured(schema: &Json) -> bool {
    match schema.get("type").and_then(|t| t.as_str()) {
        Some("object") => true,
        Some("array") => schema.get("items").is_some_and(|items| items.get("type").and_then(|t| t.as_str()) != Some("string") || is_structured(items)),
        _ => false,
    }
}

/// A short Python-like type for a JSON schema, e.g.
/// `list[{name: str, entityType: str, observations: list[str]}]`.
/// Optional object fields end in `?`.
pub fn compact_type(schema: &Json, depth: usize) -> String {
    if depth > 4 {
        return "...".into();
    }
    if let Some(options) = schema.get("anyOf").or_else(|| schema.get("oneOf")).and_then(|v| v.as_array()) {
        return options.iter().map(|o| compact_type(o, depth + 1)).collect::<Vec<_>>().join(" | ");
    }
    if let Some(values) = schema.get("enum").and_then(|v| v.as_array()) {
        return values.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(" | ");
    }
    match schema.get("type").and_then(|t| t.as_str()) {
        Some("string") => "str".into(),
        Some("integer") => "int".into(),
        Some("number") => "float".into(),
        Some("boolean") => "bool".into(),
        Some("null") => "None".into(),
        Some("array") => match schema.get("items") {
            Some(items) => format!("list[{}]", compact_type(items, depth + 1)),
            None => "list".into(),
        },
        Some("object") => {
            let Some(props) = schema.get("properties").and_then(|v| v.as_object()) else { return "dict".into() };
            let required: Vec<&str> = schema
                .get("required")
                .and_then(|v| v.as_array())
                .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            let fields: Vec<String> = props
                .iter()
                .map(|(name, sub)| {
                    let mark = if required.contains(&name.as_str()) { "" } else { "?" };
                    format!("{name}{mark}: {}", compact_type(sub, depth + 1))
                })
                .collect();
            format!("{{{}}}", fields.join(", "))
        }
        _ => "any".into(),
    }
}

#[cfg(test)]
mod compact_type_tests {
    use super::*;

    #[test]
    fn nested_schema_renders_as_a_python_like_type() {
        let schema = json!({"type": "array", "items": {"type": "object",
            "properties": {"name": {"type": "string"}, "observations": {"type": "array", "items": {"type": "string"}},
                           "weight": {"type": "number"}},
            "required": ["name", "observations"]}});
        assert!(is_structured(&schema));
        assert_eq!(compact_type(&schema, 0), "list[{name: str, observations: list[str], weight?: float}]");
        assert!(!is_structured(&json!({"type": "array", "items": {"type": "string"}})));
        assert!(!is_structured(&json!({"type": "string"})));
        assert_eq!(compact_type(&json!({"enum": ["a", "b"]}), 0), "\"a\" | \"b\"");
    }
}
