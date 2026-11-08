//! Stdio MCP server. Accepts newline-delimited JSON-RPC (the MCP stdio
//! transport) and, for older clients, Content-Length framing; replies use the
//! framing of the request.

use std::io;

fn main() {
    if let Err(err) = serve() {
        eprintln!("weave-mcp: {err}");
        std::process::exit(1);
    }
}

fn serve() -> Result<(), String> {
    let config = config_from_args()?;
    let stdin = io::stdin();
    let mut input = stdin.lock();
    // Keep reading notifications while scripts run. Declared reads share a gate;
    // scripts/writes take it exclusively. Bounded admission never blocks input.
    let gate = std::sync::RwLock::new(());
    let active = std::sync::Mutex::new(std::collections::HashMap::<String, weave_mcp::RequestControl>::new());
    std::thread::scope(|scope| -> Result<(), String> {
        let mut pending: Vec<std::thread::ScopedJoinHandle<'_, Result<(), String>>> = Vec::new();
        let result = (|| -> Result<(), String> {
            loop {
                while let Some(i) = pending.iter().position(|h| h.is_finished()) {
                    pending.swap_remove(i).join().map_err(|_| "MCP request worker panicked")??;
                }
                let Some((message, framing)) = weave_mcp::framing::read_message(&mut input)? else { break };
                let method = message.get("method").and_then(|v| v.as_str()).unwrap_or("");
                if method == "notifications/cancelled" && message.get("id").is_none() {
                    if let Some(id) = message.pointer("/params/requestId") {
                        if let Some(control) = active.lock().unwrap().get(&id.to_string()) { control.cancel(); }
                    }
                    continue;
                }
                if method != "tools/call" || message.get("id").is_none() {
                    if let Some(response) = weave_mcp::handle_with(&message, &config) {
                        weave_mcp::framing::write_message(&mut io::stdout().lock(), &response, framing)?;
                    }
                    continue;
                }
                let id = message["id"].to_string();
                let control = weave_mcp::RequestControl::default();
                let rejected = {
                    let mut requests = active.lock().unwrap();
                    if requests.contains_key(&id) { Some("duplicate active request id") }
                    else if pending.len() >= 16 { Some("maximum 16 in-flight tool requests; retry explicitly") }
                    else { requests.insert(id.clone(), control.clone()); None }
                };
                if let Some(reason) = rejected {
                    let response = serde_json::json!({"jsonrpc":"2.0", "id":message["id"],
                        "error":{"code":-32000,"message":reason}});
                    weave_mcp::framing::write_message(&mut io::stdout().lock(), &response, framing)?;
                    continue;
                }
                let read = message.pointer("/params/name").and_then(|v| v.as_str()).is_some_and(|name| {
                    config.upstreams.iter().any(|u| u.tools.iter().any(|t| t.name == name && t.effect <= weave::Effect::Read)) ||
                    config.tools.iter().any(|t| t.name == name && t.effect <= weave::Effect::Read)
                });
                let config = &config;
                let gate = &gate;
                let active = &active;
                pending.push(scope.spawn(move || {
                    let respond = || -> Result<(), String> {
                        if let Some(response) = weave_mcp::handle_with_control(&message, config, &control) {
                            weave_mcp::framing::write_message(&mut io::stdout().lock(), &response, framing)?;
                        }
                        Ok(())
                    };
                    let result = if read { let _guard = gate.read().unwrap(); respond() }
                                 else { let _guard = gate.write().unwrap(); respond() };
                    active.lock().unwrap().remove(&id);
                    result
                }));
            }
            Ok(())
        })();
        // EOF and malformed input cancel active/queued work before joining.
        for control in active.lock().unwrap().values() { control.cancel(); }
        for handle in pending { handle.join().map_err(|_| "MCP request worker panicked")??; }
        result
    })
}

fn config_from_args() -> Result<weave_mcp::ServerConfig, String> {
    let mut args = std::env::args().skip(1);
    let mut path = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                path = Some(args.next().ok_or("--config needs a path")?);
            }
            "-V" | "--version" => {
                println!("weave-mcp {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-h" | "--help" => {
                println!("weave-mcp [--config tools.json] [--version]");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    match path {
        Some(path) => weave_mcp::ServerConfig::load(std::path::Path::new(&path)),
        None => Ok(weave_mcp::ServerConfig::default()),
    }
}

