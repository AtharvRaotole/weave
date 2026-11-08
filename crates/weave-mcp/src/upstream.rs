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
        Ok(Self { name, command, args, prefix, effects })
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
        let listed = upstream.request("tools/list", json!({}))?;
        let tools = listed.get("tools").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        upstream.tools = tools.iter().map(|tool| discover(&upstream.config, tool)).collect::<Result<_, _>>()?;
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

    /// Register every upstream tool on `engine`.
    pub fn register_all(self: &Arc<Self>, engine: &mut Engine) {
        for tool in &self.tools {
            let upstream = self.clone();
            let spec_tool = tool.clone();
            let mut spec = ToolSpec::effect(tool.effect);
            spec.accepts_kwargs = true;
            engine.register(&tool.name, spec, move |args, kwargs| {
                let arguments = arguments_for(&spec_tool, &args, &kwargs)?;
                upstream.call(&spec_tool.upstream_name, arguments).map(Value::Str)
            });
        }
    }
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
        name: format!("{}{}", config.prefix, upstream_name),
        description: tool.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        upstream_name,
        input_schema,
        params,
        required,
        effect,
    })
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
