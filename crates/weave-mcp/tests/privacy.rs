//! Planted-secret tests against the real `weave-mcp` binary: upstream stderr,
//! upstream environment, upstream error text and failed example samples stay
//! out of client-visible output unless the config opts in.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};

use serde_json::{json, Value as Json};

const PLANTED_ENV: &str = "WEAVE_PLANTED_HOST_SECRET";
const ENV_SECRET: &str = "HOST-ENV-SECRET-91c2";

fn run_session(upstream: Json) -> (Vec<Json>, String) {
    let dir = std::env::temp_dir().join(format!("weave-privacy-{}-{}", std::process::id(), rand_suffix()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("config.json");
    std::fs::write(&config, json!({"upstreams": [upstream]}).to_string()).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_weave-mcp"))
        .args(["--config", config.to_str().unwrap()])
        .env(PLANTED_ENV, ENV_SECRET)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let script = "try:\n    p_leak()\nexcept ToolError as e:\n    print(\"leak:\", str(e))\ntry:\n    p_rpc_fail()\nexcept ToolError as e:\n    print(\"rpc:\", str(e))\nprint(\"env:\", p_env_probe(\"WEAVE_PLANTED_HOST_SECRET\"))\n";
    let requests = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"weave_run","arguments":{"source":script}}}),
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"p_rpc_fail","arguments":{}}}),
    ];
    let mut replies = Vec::new();
    for request in requests {
        writeln!(stdin, "{request}").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        replies.push(serde_json::from_str(&line).unwrap());
    }
    drop(stdin);
    let mut stderr = String::new();
    child.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();
    child.wait().unwrap();
    let _ = std::fs::remove_dir_all(dir);
    (replies, stderr)
}

fn rand_suffix() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
}

fn upstream(extra: Json) -> Json {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake_mcp_server.py");
    let python = std::env::var("PYTHON").unwrap_or_else(|_| "python3".into());
    let mut base = json!({"name": "probe", "command": python, "args": [script], "prefix": "p_",
                          "examples": {"leak": {}}});
    for (k, v) in extra.as_object().unwrap() {
        base[k] = v.clone();
    }
    base
}

#[test]
fn defaults_keep_upstream_secrets_out_of_client_output() {
    let (replies, stderr) = run_session(upstream(json!({})));
    let all = format!("{replies:?}\n{stderr}");
    for secret in ["UPSTREAM-STDERR-SECRET", "UPSTREAM-ERROR-SECRET", "RPC-SECRET", ENV_SECRET] {
        assert!(!all.contains(secret), "{secret} leaked:\n{all}");
    }
    let run = replies[2]["result"]["content"][0]["text"].as_str().unwrap();
    let stdout: Json = serde_json::from_str(run).unwrap();
    let stdout = stdout["stdout"].as_str().unwrap();
    assert!(stdout.contains("leak: tool `p_leak` failed: upstream tool returned isError (response content omitted"), "{stdout}");
    assert!(stdout.contains("rpc: tool `p_rpc_fail` failed: upstream `probe` returned a JSON-RPC error (payload omitted)"), "{stdout}");
    assert!(stdout.contains("env: absent"), "{stdout}");
    let listing = replies[1].to_string();
    assert!(listing.contains("sample failed; content omitted"), "{listing}");
}

#[test]
fn explicit_opt_ins_restore_diagnostics() {
    let (replies, stderr) = run_session(upstream(json!({"stderr": "inherit", "inherit_env": true, "debug": true})));
    assert!(stderr.contains("UPSTREAM-STDERR-SECRET"), "{stderr}");
    let run = replies[2]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(run.contains("UPSTREAM-ERROR-SECRET") && run.contains("RPC-SECRET") && run.contains(ENV_SECRET), "{run}");
}

#[test]
fn explicit_env_and_stderr_file() {
    let log = std::env::temp_dir().join(format!("weave-upstream-stderr-{}.log", rand_suffix()));
    let (replies, stderr) = run_session(upstream(json!({
        "stderr": {"file": log.to_str().unwrap()},
        "env": {"WEAVE_PLANTED_HOST_SECRET": "explicitly-passed"}
    })));
    assert!(!stderr.contains("UPSTREAM-STDERR-SECRET"));
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(logged.contains("UPSTREAM-STDERR-SECRET"), "{logged}");
    let run = replies[2]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(run.contains("env: explicitly-passed") && !run.contains(ENV_SECRET), "{run}");
    let _ = std::fs::remove_file(log);
}

#[test]
fn bad_privacy_config_is_rejected() {
    for bad in [json!({"stderr": "loud"}), json!({"env": {"A": 1}}), json!({"inherit_env": "yes"}), json!({"debug": 1})] {
        let mut value = json!({"name": "x", "command": "x"});
        for (k, v) in bad.as_object().unwrap() { value[k] = v.clone(); }
        assert!(weave_mcp::UpstreamConfig::parse(&value).is_err(), "{value}");
    }
}
