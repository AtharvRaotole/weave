//! Client side: spawn another MCP server over stdio and expose its tools to
//! Weave scripts. Upstream servers speak newline-delimited JSON-RPC, as the
//! MCP stdio transport specifies.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Map, Value as Json};
use weave::{Effect, Engine, ToolSpec, Value};

const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// How one upstream server is launched and tagged.
#[derive(Clone, Debug)]
pub struct UpstreamConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    /// Prepended to every upstream tool name, e.g. `fs_`.
    pub prefix: String,
    /// Per-tool effects. Tools not listed use MCP annotations: `readOnlyHint`
    /// means read; anything else is irreversible.
    pub effects: BTreeMap<String, Effect>,
    /// Per-tool result shaping: `lines` turns text into a list of non-empty
    /// lines; `json` parses the text as JSON. Other tools return text.
    pub results: BTreeMap<String, ResultShape>,
    /// Per-tool sample arguments. At startup each listed read-only tool is
    /// called once and its (truncated) output is shown in the tool listing,
    /// so a script writer knows the result format before parsing it.
    pub examples: BTreeMap<String, Json>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultShape {
    Text,
    Lines,
    Json,
}

impl ResultShape {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Text => "str (text content)",
            Self::Lines => "list of str, one per line",
            Self::Json => "parsed JSON value",
        }
    }
}

/// One tool discovered on an upstream server.
#[derive(Clone, Debug)]
pub struct UpstreamTool {
    /// Name as Weave scripts see it (prefix + upstream name).
    pub name: String,
    pub upstream_name: String,
    pub description: String,
    pub input_schema: Json,
    /// Parameter names in schema order; positional arguments map onto these.
    pub params: Vec<String>,
    pub required: Vec<String>,
    pub effect: Effect,
    pub shape: Option<ResultShape>,
    /// `(arguments, output)` from one sample call made at startup.
    pub example: Option<(Json, String)>,
}

type Pending = Arc<Mutex<HashMap<u64, Sender<Json>>>>;

/// A running upstream server. Calls may be issued from many threads at once;
/// responses are matched to requests by id.
pub struct Upstream {
    pub config: UpstreamConfig,
    pub tools: Vec<UpstreamTool>,
    stdin: Mutex<ChildStdin>,
    pending: Pending,
    next_id: AtomicU64,
    child: Mutex<Child>,
}

impl std::fmt::Debug for Upstream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upstream").field("name", &self.config.name).field("tools", &self.tools.len()).finish()
    }
}

impl Drop for Upstream {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl UpstreamConfig {
    pub fn parse(value: &Json) -> Result<Self, String> {
        let name = value.get("name").and_then(|v| v.as_str()).ok_or("upstream needs a name")?.to_string();
        let command = value
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("upstream `{name}` needs a command"))?
            .to_string();
        let args = match value.get("args") {
            None => Vec::new(),
            Some(Json::Array(items)) => items
                .iter()
                .map(|item| item.as_str().map(str::to_string).ok_or_else(|| format!("upstream `{name}` args must be strings")))
                .collect::<Result<_, _>>()?,
            Some(_) => return Err(format!("upstream `{name}` args must be a list")),
        };
        let prefix = value.get("prefix").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let mut effects = BTreeMap::new();
        if let Some(map) = value.get("effects") {
            let map = map.as_object().ok_or_else(|| format!("upstream `{name}` effects must be an object"))?;
            for (tool, effect) in map {
                let text = effect.as_str().unwrap_or("");
                let effect = Effect::parse(text)
                    .ok_or_else(|| format!("upstream `{name}`: unknown effect `{text}` for `{tool}`"))?;
                effects.insert(tool.clone(), effect);
            }
        }
        let mut results = BTreeMap::new();
        if let Some(map) = value.get("results") {
            let map = map.as_object().ok_or_else(|| format!("upstream `{name}` results must be an object"))?;
            for (tool, shape) in map {
                let shape = match shape.as_str() {
                    Some("lines") => ResultShape::Lines,
                    Some("json") => ResultShape::Json,
                    Some("text") => ResultShape::Text,
                    _ => return Err(format!("upstream `{name}`: result shape for `{tool}` must be lines, json or text")),
                };
                results.insert(tool.clone(), shape);
            }
        }
        let examples = match value.get("examples") {
            None => BTreeMap::new(),
            Some(Json::Object(map)) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            Some(_) => return Err(format!("upstream `{name}` examples must be an object")),
        };
        Ok(Self { name, command, args, prefix, effects, results, examples })
    }
}

impl Upstream {
    /// Spawn the server, run the MCP handshake, and list its tools.
    pub fn connect(config: UpstreamConfig) -> Result<Arc<Self>, String> {
        let mut child = Command::new(&config.command)
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|err| format!("upstream `{}`: cannot start `{}`: {err}", config.name, config.command))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let reader_pending = pending.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let Ok(message) = serde_json::from_str::<Json>(line.trim()) else { continue };
                let Some(id) = message.get("id").and_then(|v| v.as_u64()) else { continue };
                if message.get("method").is_some() {
                    continue; // server-to-client requests are not supported
                }
                if let Some(tx) = reader_pending.lock().unwrap().remove(&id) {
                    let _ = tx.send(message);
                }
            }
            // Wake every waiter when the server exits.
            reader_pending.lock().unwrap().clear();
        });
        let mut upstream = Self {
            config,
            tools: Vec::new(),
            stdin: Mutex::new(stdin),
            pending,
            next_id: AtomicU64::new(1),
            child: Mutex::new(child),
        };
        upstream.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "weave-mcp", "version": env!("CARGO_PKG_VERSION")}
            }),
        )?;
        upstream.notify("notifications/initialized")?;
        let mut cursor = None;
        let mut seen = std::collections::BTreeSet::new();
        let mut tools = Vec::new();
        loop {
            if seen.len() >= 64 || tools.len() > 4096 {
                return Err(format!("upstream `{}` discovery limit exceeded", upstream.config.name));
            }
            let params = cursor.as_ref().map(|c| json!({"cursor": c})).unwrap_or(json!({}));
            let listed = upstream.request("tools/list", params)?;
            tools.extend(listed.get("tools").and_then(Json::as_array).cloned().unwrap_or_default());
            if tools.len() > 4096 { return Err("tools/list tool count limit exceeded".into()); }
            match listed.get("nextCursor").and_then(Json::as_str) {
                Some(next) => {
                    if !seen.insert(next.to_string()) { return Err("repeated tools/list cursor".into()); }
                    cursor = Some(next.to_string());
                }
                None => break,
            }
        }
        upstream.tools = tools.iter().map(|tool| discover(&upstream.config, tool)).collect::<Result<_, _>>()?;
        upstream.capture_examples();
        Ok(Arc::new(upstream))
    }

    fn send(&self, message: &Json) -> Result<(), String> {
        let mut stdin = self.stdin.lock().unwrap();
        let mut line = serde_json::to_string(message).map_err(|err| err.to_string())?;
        line.push('\n');
        stdin.write_all(line.as_bytes()).and_then(|_| stdin.flush()).map_err(|err| format!("upstream `{}`: {err}", self.config.name))
    }

    fn notify(&self, method: &str) -> Result<(), String> {
        self.send(&json!({"jsonrpc": "2.0", "method": method}))
    }

    pub fn request(&self, method: &str, params: Json) -> Result<Json, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let reply = rx.recv_timeout(CALL_TIMEOUT).map_err(|_| {
            self.pending.lock().unwrap().remove(&id);
            format!("upstream `{}` did not answer `{method}`", self.config.name)
        })?;
        if let Some(error) = reply.get("error") {
            let message = error.get("message").and_then(|v| v.as_str()).unwrap_or("error");
            return Err(format!("upstream `{}`: {message}", self.config.name));
        }
        Ok(reply.get("result").cloned().unwrap_or(Json::Null))
    }

    /// Call a tool with an MCP arguments object. Returns the text content, or
    /// an error when the server sets `isError`.
    pub fn call(&self, tool: &str, arguments: Json) -> Result<String, String> {
        let result = self.request("tools/call", json!({"name": tool, "arguments": arguments}))?;
        let text = result
            .get("content")
            .and_then(|v| v.as_array())
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|part| part.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if result.get("isError").and_then(|v| v.as_bool()).unwrap_or(false) {
            Err(text)
        } else {
            Ok(text)
        }
    }

    /// Call each configured example once. Only read-only and pure tools are
    /// sampled; a failing sample is shown as the error text it returned.
    fn capture_examples(&mut self) {
        let mut samples = Vec::new();
        for (index, tool) in self.tools.iter().enumerate() {
            let Some(arguments) = self.config.examples.get(&tool.upstream_name) else { continue };
            if !matches!(tool.effect, Effect::Read | Effect::Pure) {
                continue;
            }
            let output = match self.call(&tool.upstream_name, arguments.clone()) {
                Ok(text) => text,
                Err(text) => format!("ToolError: {text}"),
            };
            samples.push((index, (arguments.clone(), truncate(&output, 300))));
        }
        for (index, sample) in samples {
            self.tools[index].example = Some(sample);
        }
    }

    /// Register every upstream tool on `engine`.
    pub fn register_all(self: &Arc<Self>, engine: &mut Engine) {
        for tool in &self.tools {
            let upstream = self.clone();
            let spec_tool = tool.clone();
            let mut spec = ToolSpec::effect(tool.effect);
            spec.accepts_kwargs = true;
            engine.register(&tool.name, spec, move |args, kwargs| {
                let arguments = arguments_for(&spec_tool, &args, &kwargs)?;
                let result = upstream.request("tools/call", json!({"name": spec_tool.upstream_name, "arguments": arguments}))?;
                if result.get("isError").and_then(Json::as_bool) == Some(true) {
                    return Err(result.to_string());
                }
                if spec_tool.shape.is_none() {
                    if let Some(structured) = result.get("structuredContent") {
                        return Ok(from_plain_json(structured));
                    }
                }
                let parts = result.get("content").and_then(Json::as_array).cloned().unwrap_or_default();
                if parts.is_empty() && result.get("structuredContent").is_some() {
                    return Err("explicit text shaping requested, but upstream returned structured content only".into());
                }
                if parts.iter().any(|p| p.get("type").and_then(Json::as_str) != Some("text")) {
                    return Err("unsupported non-text MCP content; use the direct tool envelope".into());
                }
                let text = parts.iter().filter_map(|p| p.get("text").and_then(Json::as_str)).collect::<Vec<_>>().join("\n");
                shape_result(spec_tool.shape, text)
            });
        }
    }
}

fn script_alias(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()) { out.push(c); }
        else { out.push_str(&format!("_u{:x}_", c as u32)); }
    }
    if out.is_empty() || matches!(out.as_str(), "if" | "else" | "elif" | "for" | "in" | "and" | "or" | "not" | "True" | "False" | "None" | "try" | "except" | "break" | "continue" | "pass" | "import" | "def" | "class" | "while" | "with" | "lambda" | "await" | "return" | "is" | "as" | "from" | "raise" | "yield" | "async" | "global" | "nonlocal" | "assert" | "del" | "finally") {
        out.insert_str(0, "tool_");
    }
    out
}

fn discover(config: &UpstreamConfig, tool: &Json) -> Result<UpstreamTool, String> {
    let upstream_name = tool.get("name").and_then(|v| v.as_str()).ok_or("upstream tool without a name")?.to_string();
    let input_schema = tool.get("inputSchema").cloned().unwrap_or(json!({"type": "object"}));
    let params = input_schema
        .get("properties")
        .and_then(|v| v.as_object())
        .map(|props| props.keys().cloned().collect())
        .unwrap_or_default();
    let required = input_schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|items| items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let read_only = tool
        .get("annotations")
        .and_then(|a| a.get("readOnlyHint"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let effect = config
        .effects
        .get(&upstream_name)
        .copied()
        .unwrap_or(if read_only { Effect::Read } else { Effect::Irreversible });
    Ok(UpstreamTool {
        name: script_alias(&format!("{}{}", config.prefix, upstream_name)),
        description: tool.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        input_schema,
        params,
        required,
        effect,
        shape: config.results.get(&upstream_name).copied(),
        upstream_name,
        example: None,
    })
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{cut}...")
}

/// Apply a configured result shape to an upstream tool's text.
pub fn shape_result(shape: Option<ResultShape>, text: String) -> Result<Value, String> {
    match shape {
        None | Some(ResultShape::Text) => Ok(Value::Str(text)),
        Some(ResultShape::Lines) => Ok(Value::List(
            text.lines().filter(|line| !line.trim().is_empty()).map(Value::str).collect(),
        )),
        Some(ResultShape::Json) => serde_json::from_str::<Json>(&text)
            .map(|json| from_plain_json(&json))
            .map_err(|err| format!("result is not JSON: {err}")),
    }
}

/// Weave value for untagged JSON.
pub fn from_plain_json(json: &Json) -> Value {
    match json {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => n.as_i64().map(Value::Int).unwrap_or_else(|| Value::Float(n.as_f64().unwrap_or(0.0))),
        Json::String(s) => Value::Str(s.clone()),
        Json::Array(items) => Value::List(items.iter().map(from_plain_json).collect()),
        Json::Object(map) => Value::Dict(map.iter().map(|(k, v)| (Value::Str(k.clone()), from_plain_json(v))).collect()),
    }
}

/// Map Weave positional and keyword arguments onto an MCP arguments object.
/// Positional arguments fill the schema's properties in order.
pub fn arguments_for(tool: &UpstreamTool, args: &[Value], kwargs: &BTreeMap<String, Value>) -> Result<Json, String> {
    if args.len() > tool.params.len() {
        return Err(format!(
            "tool `{}` takes at most {} positional argument(s) ({}), got {}",
            tool.name,
            tool.params.len(),
            tool.params.join(", "),
            args.len()
        ));
    }
    let mut out = Map::new();
    for (name, value) in tool.params.iter().zip(args) {
        // `None` for an optional parameter means "not given".
        if *value != Value::Null {
            out.insert(name.clone(), plain_json(value));
        }
    }
    for (name, value) in kwargs {
        if out.contains_key(name) {
            return Err(format!("tool `{}` got `{name}` twice", tool.name));
        }
        out.insert(name.clone(), plain_json(value));
    }
    let missing: Vec<_> = tool.required.iter().filter(|name| !out.contains_key(*name)).cloned().collect();
    if !missing.is_empty() {
        return Err(format!("tool `{}` is missing required argument(s): {}", tool.name, missing.join(", ")));
    }
    Ok(Json::Object(out))
}

/// Untagged JSON for a Weave value, as MCP servers expect.
pub fn plain_json(value: &Value) -> Json {
    match value {
        Value::Null => Json::Null,
        Value::Bool(b) => json!(b),
        Value::Int(i) => json!(i),
        Value::Float(f) => json!(f),
        Value::Str(s) => json!(s),
        Value::List(items) => Json::Array(items.iter().map(plain_json).collect()),
        Value::Dict(items) => Json::Object(
            items
                .iter()
                .map(|(k, v)| (match k { Value::Str(s) => s.clone(), other => other.to_string() }, plain_json(v)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod shape_tests {
    use super::*;

    #[test]
    fn aliases_are_identifiers_and_collisions_remain_visible() {
        assert_eq!(script_alias("a_read-file"), "a_read_u2d_file");
        assert_eq!(script_alias("read.file"), "read_u2e_file");
        assert_eq!(script_alias("for"), "tool_for");
        // Alias collisions are deliberate inputs to config validation, never
        // silently routed; this pair must produce the same final name.
        assert_eq!(script_alias("read-file"), script_alias("read_u2d_file"));
    }

    #[test]
    fn lines_shape_drops_blank_lines() {
        let value = shape_result(Some(ResultShape::Lines), "[FILE] a.txt\n\n[DIR] sub\n".into()).unwrap();
        assert_eq!(value, Value::List(vec![Value::str("[FILE] a.txt"), Value::str("[DIR] sub")]));
        assert_eq!(shape_result(None, "x\ny".into()).unwrap(), Value::str("x\ny"));
    }

    #[test]
    fn json_shape_parses_plain_json() {
        let value = shape_result(Some(ResultShape::Json), r#"{"a": [1, 2.5, null]}"#.into()).unwrap();
        assert_eq!(
            value,
            Value::Dict(vec![(Value::str("a"), Value::List(vec![Value::Int(1), Value::Float(2.5), Value::Null]))])
        );
        assert!(shape_result(Some(ResultShape::Json), "nope".into()).is_err());
    }

    #[test]
    fn config_reads_results_and_examples() {
        let config = UpstreamConfig::parse(&json!({
            "name": "fs", "command": "x",
            "results": {"list_directory": "lines", "read_text_file": "text"},
            "examples": {"list_directory": {"path": "/tmp"}}
        }))
        .unwrap();
        assert_eq!(config.results.get("list_directory"), Some(&ResultShape::Lines));
        assert_eq!(config.results["read_text_file"], ResultShape::Text);
        assert_eq!(config.examples["list_directory"], json!({"path": "/tmp"}));
        assert!(UpstreamConfig::parse(&json!({"name": "fs", "command": "x", "results": {"a": "csv"}})).is_err());
    }
}
