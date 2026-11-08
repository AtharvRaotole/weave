//! Stdio message framing.

use std::io::{BufRead, Read, Write};

/// Per-frame transport bound, not a process-memory cap.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 16 * 1024;

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
    read_message_limited(input, MAX_FRAME_BYTES)
}

pub fn read_message_limited(input: &mut impl BufRead, max_bytes: usize) -> Result<Option<(Json, Framing)>, String> {
    let mut content_length = None;
    let mut header_bytes = 0usize;
    loop {
        let mut line = String::new();
        let limit = if content_length.is_some() { MAX_HEADER_BYTES } else { max_bytes.max(MAX_HEADER_BYTES) };
        let n = (&mut *input).take(limit as u64 + 1).read_line(&mut line).map_err(|err| err.to_string())?;
        if n > limit { return Err("MCP frame/header byte budget exceeded".into()); }
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
            if trimmed.len() > max_bytes { return Err("MCP frame byte budget exceeded".into()); }
            let message = serde_json::from_str(trimmed).map_err(|err| err.to_string())?;
            return Ok(Some((message, Framing::Lines)));
        }
        header_bytes = header_bytes.saturating_add(n);
        if header_bytes > MAX_HEADER_BYTES { return Err("MCP header byte budget exceeded".into()); }
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
    if len > max_bytes { return Err("MCP frame byte budget exceeded".into()); }
    let mut buf = vec![0u8; len];
    input.read_exact(&mut buf).map_err(|err| err.to_string())?;
    let text = String::from_utf8(buf).map_err(|err| err.to_string())?;
    serde_json::from_str(&text)
        .map(|message| Some((message, Framing::Headers)))
        .map_err(|err| err.to_string())
}

pub fn write_message(output: &mut impl Write, message: &Json, framing: Framing) -> Result<(), String> {
    struct Bounded(Vec<u8>);
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_FRAME_BYTES.saturating_sub(self.0.len()) {
                return Err(std::io::Error::other("MCP frame byte budget exceeded"));
            }
            self.0.extend_from_slice(bytes); Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let mut body = Bounded(Vec::new());
    serde_json::to_writer(&mut body, message).map_err(|err| err.to_string())?;
    let body = body.0;
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

#[cfg(test)]
mod budget_tests {
    use super::*;
    use std::io::Cursor;
    #[test]
    fn byte_boundaries_and_malicious_lengths() {
        let body = r#"{"x":"é"}"#;
        for cap in [body.len()-1, body.len(), body.len()+1] {
            for input in [format!("{body}\n"),format!("Content-Length: {}\r\n\r\n{body}",body.len())] {
                assert_eq!(read_message_limited(&mut Cursor::new(input),cap).is_ok(), cap>=body.len());
            }
        }
        let huge=format!("Content-Length: {}\r\n\r\n",usize::MAX);
        assert!(read_message(&mut Cursor::new(huge)).unwrap_err().contains("budget"));
        let mut out=Vec::new();
        assert!(write_message(&mut out,&serde_json::json!({"x":"a".repeat(MAX_FRAME_BYTES)}),Framing::Lines).is_err());
        assert!(out.is_empty(),"never emit partial oversized JSON");
    }
}
