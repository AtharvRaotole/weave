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
    let stdout = io::stdout();
    let mut output = stdout.lock();
    loop {
        let Some((message, framing)) = weave_mcp::framing::read_message(&mut input)? else {
            return Ok(());
        };
        if let Some(response) = weave_mcp::handle_with(&message, &config) {
            weave_mcp::framing::write_message(&mut output, &response, framing)?;
        }
    }
}

fn config_from_args() -> Result<weave_mcp::ServerConfig, String> {
    let mut args = std::env::args().skip(1);
    let mut path = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                path = Some(args.next().ok_or("--config needs a path")?);
            }
            "-h" | "--help" => {
                println!("weave-mcp [--config tools.json]");
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

