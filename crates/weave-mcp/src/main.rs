//! Stdio MCP server. Messages use Content-Length framing, same as LSP.

use std::io::{self, BufRead, Write};

use serde_json::Value as Json;

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
        let Some(message) = read_message(&mut input)? else {
            return Ok(());
        };
        if let Some(response) = weave_mcp::handle_with(&message, &config) {
            write_message(&mut output, &response)?;
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

fn read_message(input: &mut impl BufRead) -> Result<Option<Json>, String> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let n = input.read_line(&mut line).map_err(|err| err.to_string())?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(rest) = trimmed
            .strip_prefix("Content-Length:")
            .or_else(|| trimmed.strip_prefix("content-length:"))
        {
            content_length = Some(
                rest.trim()
                    .parse::<usize>()
                    .map_err(|_| format!("bad Content-Length `{rest}`"))?,
            );
        }
    }
    let len = content_length.ok_or("missing Content-Length header")?;
    let mut buf = vec![0u8; len];
    input.read_exact(&mut buf).map_err(|err| err.to_string())?;
    let text = String::from_utf8(buf).map_err(|err| err.to_string())?;
    serde_json::from_str(&text).map(Some).map_err(|err| err.to_string())
}

fn write_message(output: &mut impl Write, message: &Json) -> Result<(), String> {
    let body = serde_json::to_vec(message).map_err(|err| err.to_string())?;
    write!(output, "Content-Length: {}\r\n\r\n", body.len()).map_err(|err| err.to_string())?;
    output.write_all(&body).map_err(|err| err.to_string())?;
    output.flush().map_err(|err| err.to_string())
}
