//! Stdio message framing.

use std::io::{BufRead, Write};

use serde_json::Value as Json;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Framing {
    /// One JSON-RPC message per line (the MCP stdio transport).
    Lines,
    /// `Content-Length` headers, as in LSP.
    Headers,
}

/// Read one message. Blank lines between messages are skipped.
pub fn read_message(input: &mut impl BufRead) -> Result<Option<(Json, Framing)>, String> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let n = input.read_line(&mut line).map_err(|err| err.to_string())?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            if content_length.is_some() {
                break;
            }
            continue;
        }
        if content_length.is_none() && trimmed.trim_start().starts_with('{') {
            let message = serde_json::from_str(trimmed).map_err(|err| err.to_string())?;
            return Ok(Some((message, Framing::Lines)));
        }
        let lower = trimmed.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("content-length:") {
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
    serde_json::from_str(&text)
        .map(|message| Some((message, Framing::Headers)))
        .map_err(|err| err.to_string())
}

pub fn write_message(output: &mut impl Write, message: &Json, framing: Framing) -> Result<(), String> {
    let body = serde_json::to_vec(message).map_err(|err| err.to_string())?;
    if framing == Framing::Headers {
        write!(output, "Content-Length: {}\r\n\r\n", body.len()).map_err(|err| err.to_string())?;
        output.write_all(&body).map_err(|err| err.to_string())?;
    } else {
        output.write_all(&body).map_err(|err| err.to_string())?;
        output.write_all(b"\n").map_err(|err| err.to_string())?;
    }
    output.flush().map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_both_framings() {
        let body = r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#;
        let input = format!("{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}}\n\nContent-Length: {}\r\n\r\n{body}", body.len());
        let mut reader = std::io::Cursor::new(input.into_bytes());
        let (first, framing) = read_message(&mut reader).unwrap().unwrap();
        assert_eq!((first["id"].as_i64(), framing), (Some(1), Framing::Lines));
        let (second, framing) = read_message(&mut reader).unwrap().unwrap();
        assert_eq!((second["id"].as_i64(), framing), (Some(2), Framing::Headers));
        assert!(read_message(&mut reader).unwrap().is_none());
    }

    #[test]
    fn writes_in_the_requested_framing() {
        let mut out = Vec::new();
        write_message(&mut out, &serde_json::json!({"a": 1}), Framing::Lines).unwrap();
        assert_eq!(out, b"{\"a\":1}\n");
    }
}
