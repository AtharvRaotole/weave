//! `weave check|run|replay|trace` for scripts whose tools are echo or const.
//!
//! Host functions with real bodies are registered from Python. This binary
//! is the same four commands for scripts that only need scripted tools.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::process::ExitCode;

use weave::{Effect, Engine, ToolSpec, Trace, Value};

fn main() -> ExitCode {
    match run(env::args().skip(1)) {
        Ok(text) => {
            if !text.is_empty() {
                print!("{text}");
                if !text.ends_with('\n') {
                    println!();
                }
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("weave: {err}");
            ExitCode::from(1)
        }
    }
}

fn run(args: impl Iterator<Item = String>) -> Result<String, String> {
    let args: Vec<String> = args.collect();
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        return Ok(HELP.to_string());
    }
    if args[0] == "-V" || args[0] == "--version" {
        return Ok(format!("weave {}\n", env!("CARGO_PKG_VERSION")));
    }
    let cmd = args[0].as_str();
    let rest = &args[1..];
    match cmd {
        "check" => cmd_check(rest),
        "run" => cmd_run(rest),
        "replay" => cmd_replay(rest),
        "trace" => cmd_trace(rest),
        other => Err(format!("unknown command `{other}`\n{HELP}")),
    }
}

const HELP: &str = "\
weave check FILE [--tool name=...,effect=read,arity=1]
weave run FILE [--tool name=...,effect=read,behavior=echo] [--trace-out PATH]
weave replay FILE --trace PATH [--tool ...]
weave trace PATH

--tool fields: name, effect (pure|read|reversible|irreversible), arity,
behavior (echo|const), result (a string, required for const).
echo returns its single positional argument.
";

struct Flags {
    file: String,
    trace: Option<String>,
    trace_out: Option<String>,
    tools: Vec<ToolFlag>,
}

struct ToolFlag {
    name: String,
    effect: Effect,
    arity: Option<usize>,
    behavior: String,
    result: Option<String>,
}

fn parse_flags(args: &[String], need_trace: bool) -> Result<Flags, String> {
    let mut file = None;
    let mut trace = None;
    let mut trace_out = None;
    let mut tools = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--tool" => {
                let spec = args.get(i + 1).ok_or("--tool needs a value")?;
                tools.push(parse_tool(spec)?);
                i += 2;
            }
            "--trace" => {
                trace = Some(args.get(i + 1).ok_or("--trace needs a path")?.clone());
                i += 2;
            }
            "--trace-out" => {
                trace_out = Some(args.get(i + 1).ok_or("--trace-out needs a path")?.clone());
                i += 2;
            }
            other if other.starts_with('-') => return Err(format!("unknown flag `{other}`")),
            other if file.is_none() => {
                file = Some(other.to_string());
                i += 1;
            }
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }
    let file = file.ok_or("expected a script path")?;
    if need_trace && trace.is_none() {
        return Err("--trace is required".into());
    }
    Ok(Flags {
        file,
        trace,
        trace_out,
        tools,
    })
}

fn parse_tool(spec: &str) -> Result<ToolFlag, String> {
    let mut name = None;
    let mut effect = Effect::Irreversible;
    let mut arity = None;
    let mut behavior = "echo".to_string();
    let mut result = None;
    for part in spec.split(',') {
        let (key, value) = part
            .split_once('=')
            .ok_or_else(|| format!("tool field `{part}` must be key=value"))?;
        match key {
            "name" => name = Some(value.to_string()),
            "effect" => {
                effect = Effect::parse(value)
                    .ok_or_else(|| format!("unknown effect `{value}`"))?;
            }
            "arity" => {
                arity = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| format!("arity `{value}` is not an integer"))?,
                );
            }
            "behavior" => behavior = value.to_string(),
            "result" => result = Some(value.to_string()),
            other => return Err(format!("unknown tool field `{other}`")),
        }
    }
    let name = name.ok_or("tool spec needs name=")?;
    if behavior == "const" && result.is_none() {
        return Err(format!("tool `{name}` behavior const needs result="));
    }
    if behavior != "echo" && behavior != "const" {
        return Err(format!("unknown behavior `{behavior}`; use echo or const"));
    }
    Ok(ToolFlag {
        name,
        effect,
        arity,
        behavior,
        result,
    })
}

fn engine_from(tools: &[ToolFlag]) -> Engine {
    let mut engine = Engine::new();
    for tool in tools {
        let mut spec = ToolSpec::effect(tool.effect);
        if let Some(arity) = tool.arity {
            spec = spec.with_arity(arity);
        }
        let behavior = tool.behavior.clone();
        let constant = tool.result.clone();
        let name = tool.name.clone();
        engine.register(&name, spec, move |args, kwargs| {
            execute(&behavior, constant.as_deref(), &args, &kwargs)
        });
    }
    engine
}

fn execute(
    behavior: &str,
    constant: Option<&str>,
    args: &[Value],
    kwargs: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    match behavior {
        "const" => Ok(Value::str(constant.unwrap_or(""))),
        "echo" => Ok(echo_value(args, kwargs)),
        other => Err(format!("unknown tool behavior `{other}`")),
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

fn cmd_check(args: &[String]) -> Result<String, String> {
    let flags = parse_flags(args, false)?;
    let source = fs::read_to_string(&flags.file).map_err(|err| format!("{}: {err}", flags.file))?;
    let diags = engine_from(&flags.tools).check(&source);
    if diags.is_empty() {
        Ok("ok\n".into())
    } else {
        Err(diags
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

fn cmd_run(args: &[String]) -> Result<String, String> {
    let flags = parse_flags(args, false)?;
    let source = fs::read_to_string(&flags.file).map_err(|err| format!("{}: {err}", flags.file))?;
    let result = engine_from(&flags.tools)
        .run(&source)
        .map_err(|err| err.to_string())?;
    if let Some(path) = &flags.trace_out {
        let json = result.trace.to_json().map_err(|err| err.to_string())?;
        fs::write(path, json).map_err(|err| format!("{path}: {err}"))?;
    }
    Ok(result.stdout())
}

fn cmd_replay(args: &[String]) -> Result<String, String> {
    let flags = parse_flags(args, true)?;
    let source = fs::read_to_string(&flags.file).map_err(|err| format!("{}: {err}", flags.file))?;
    let trace_path = flags.trace.as_deref().unwrap();
    let trace_text = fs::read_to_string(trace_path).map_err(|err| format!("{trace_path}: {err}"))?;
    let trace = Trace::from_json(&trace_text).map_err(|err| err.to_string())?;
    let result = engine_from(&flags.tools)
        .replay(&source, &trace)
        .map_err(|err| err.to_string())?;
    Ok(result.stdout())
}

fn cmd_trace(args: &[String]) -> Result<String, String> {
    let path = args.first().ok_or("trace needs a path")?;
    if args.len() != 1 {
        return Err("trace takes one path".into());
    }
    let text = fs::read_to_string(path).map_err(|err| format!("{path}: {err}"))?;
    let trace = Trace::from_json(&text).map_err(|err| err.to_string())?;
    Ok(render_trace(&trace))
}

fn render_trace(trace: &Trace) -> String {
    let mut out = String::from("seq\tline\ttool\teffect\targs\tresult\terror\n");
    for event in &trace.events {
        let args = serde_json::to_string(&event.args).unwrap_or_else(|_| "[]".into());
        let result = event
            .error
            .as_ref()
            .map(|_| String::new())
            .unwrap_or_else(|| serde_json::to_string(&event.result).unwrap_or_default());
        let error = event.error.clone().unwrap_or_default();
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            event.seq, event.line, event.tool, event.effect, args, result, error
        ));
    }
    out
}
