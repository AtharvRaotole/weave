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
    // Only declared read/pure direct calls are concurrent. Writes and scripts
    // drain the batch first. This is bounded transport concurrency, not auth.
    std::thread::scope(|scope| -> Result<(), String> {
        let mut pending = Vec::new();
        loop {
            let Some((message, framing)) = weave_mcp::framing::read_message(&mut input)? else { break };
            let read = message.get("method").and_then(|v| v.as_str()) == Some("tools/call") &&
                message.pointer("/params/name").and_then(|v| v.as_str()).is_some_and(|name| {
                    config.upstreams.iter().any(|u| u.tools.iter().any(|t| t.name == name && t.effect <= weave::Effect::Read)) ||
                    config.tools.iter().any(|t| t.name == name && t.effect <= weave::Effect::Read)
                });
            if !read || pending.len() >= 16 {
                for handle in pending.drain(..) {
                    let handle: std::thread::ScopedJoinHandle<'_, Result<(), String>> = handle;
                    handle.join().map_err(|_| "MCP request worker panicked")??;
                }
            }
            let config = &config;
            let respond = move || -> Result<(), String> {
                if let Some(response) = weave_mcp::handle_with(&message, config) {
                    weave_mcp::framing::write_message(&mut io::stdout().lock(), &response, framing)?;
                }
                Ok(())
            };
            if read { pending.push(scope.spawn(respond)); }
            else { respond()?; }
        }
        for handle in pending {
            handle.join().map_err(|_| "MCP request worker panicked")??;
        }
        Ok(())
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

