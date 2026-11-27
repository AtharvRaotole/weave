//! Virtual-time runtime (used by the wasm32 build and the browser
//! playground). Run with `cargo test -p weave --features virtual-time --test virtual_time`.
#![cfg(feature = "virtual-time")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use weave::{vclock, Effect, Engine, Limits, ToolSpec, Value};

type Log = Arc<Mutex<Vec<(String, u128, u128)>>>;

fn ms(d: Duration) -> u128 {
    d.as_millis()
}

/// Register an async tool that sleeps `latency` on the virtual clock and logs
/// `(label, start_ms, end_ms)`.
fn tool(engine: &mut Engine, log: &Log, name: &str, spec: ToolSpec, latency: u64) {
    let log = log.clone();
    let label = name.to_string();
    engine.register_async(name, spec, move |args, _kwargs, _token| {
        let log = log.clone();
        let label = label.clone();
        async move {
            let start = vclock::now();
            vclock::sleep(Duration::from_millis(latency)).await;
            let arg = args.first().map(weave_str).unwrap_or_default();
            log.lock()
                .unwrap()
                .push((format!("{label}({arg})"), ms(start), ms(vclock::now())));
            Ok(Value::str(format!("{label}:{arg}")))
        }
    });
}

fn weave_str(v: &Value) -> String {
    v.as_str()
        .map(str::to_string)
        .unwrap_or_else(|| format!("{v:?}"))
}

fn setup() -> (Engine, Log) {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let mut engine = Engine::new();
    tool(
        &mut engine,
        &log,
        "fetch",
        ToolSpec::effect(Effect::Read).with_arity(1),
        100,
    );
    tool(
        &mut engine,
        &log,
        "slow",
        ToolSpec::effect(Effect::Read).with_arity(1),
        300,
    );
    tool(
        &mut engine,
        &log,
        "save",
        ToolSpec::effect(Effect::Reversible).with_arity(1),
        50,
    );
    tool(
        &mut engine,
        &log,
        "send",
        ToolSpec::effect(Effect::Irreversible).with_arity(1),
        20,
    );
    (engine, log)
}

fn spans(log: &Log) -> Vec<(String, u128, u128)> {
    let mut v = log.lock().unwrap().clone();
    v.sort_by(|a, b| (a.1, a.2, &a.0).cmp(&(b.1, b.2, &b.0)));
    v
}

#[test]
fn independent_reads_overlap_on_the_virtual_clock() {
    let (engine, log) = setup();
    let result = engine
        .run("a = fetch(\"x\")\nb = fetch(\"y\")\nc = slow(\"z\")\nprint(a, b, c)\n")
        .unwrap();
    assert_eq!(result.stdout(), "fetch:x fetch:y slow:z\n");
    let s = spans(&log);
    assert_eq!(s.len(), 3);
    assert!(s.iter().all(|(_, start, _)| *start == 0), "{s:?}");
    assert_eq!(ms(vclock::now()), 300);
}

#[test]
fn dependent_calls_chain() {
    let (engine, log) = setup();
    engine
        .run("a = fetch(\"x\")\nb = fetch(a)\nprint(b)\n")
        .unwrap();
    let s = spans(&log);
    assert_eq!(s[0], ("fetch(x)".into(), 0, 100));
    assert_eq!(s[1], ("fetch(fetch:x)".into(), 100, 200));
}

#[test]
fn writes_keep_program_order_and_irreversible_waits() {
    let (engine, log) = setup();
    engine
        .run("r = slow(\"q\")\nsave(\"one\")\nsave(\"two\")\nsend(\"done\")\n")
        .unwrap();
    let s = spans(&log);
    let find = |name: &str| s.iter().find(|e| e.0 == name).cloned().unwrap();
    let one = find("save(one)");
    let two = find("save(two)");
    let send = find("send(done)");
    assert!(two.1 >= one.2, "{s:?}");
    assert!(send.1 >= two.2, "{s:?}");
}

#[test]
fn per_call_timeout_uses_virtual_time() {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let mut engine = Engine::new();
    tool(
        &mut engine,
        &log,
        "slow",
        ToolSpec::effect(Effect::Read)
            .with_arity(1)
            .with_timeout(Duration::from_millis(50)),
        5_000,
    );
    let err = engine.run("print(slow(\"x\"))\n").unwrap_err();
    assert!(
        err.summary().contains("timed out after 50ms"),
        "{}",
        err.summary()
    );
    assert_eq!(ms(vclock::now()), 50);
}

#[test]
fn wall_time_limit_uses_virtual_time() {
    let (mut engine, _log) = setup();
    engine.set_limits(Limits {
        max_wall_time: Duration::from_millis(200),
        ..Limits::default()
    });
    let err = engine.run("print(slow(\"x\"))\n").unwrap_err();
    assert!(
        err.summary().contains("time limit of 200ms exceeded"),
        "{}",
        err.summary()
    );
}

#[test]
fn try_except_catches_tool_errors() {
    let mut engine = Engine::new();
    engine.register_async(
        "boom",
        ToolSpec::effect(Effect::Read).with_arity(0),
        |_, _, _| async {
            vclock::sleep(Duration::from_millis(10)).await;
            Err::<Value, String>("nope".into())
        },
    );
    let result = engine
        .run("try:\n    boom()\nexcept Exception as e:\n    print(\"caught\")\n")
        .unwrap();
    assert_eq!(result.stdout(), "caught\n");
}

#[test]
fn sync_tools_run_inline() {
    let mut engine = Engine::new();
    engine.register(
        "add",
        ToolSpec::effect(Effect::Pure).with_arity(2),
        |args, _| {
            Ok(Value::int(
                args[0].as_int().unwrap() + args[1].as_int().unwrap(),
            ))
        },
    );
    let result = engine.run("print(add(2, 3))\n").unwrap();
    assert_eq!(result.stdout(), "5\n");
}

#[test]
fn loops_fan_out_and_runs_are_deterministic() {
    let src = "out = []\nfor c in [\"a\", \"b\", \"c\", \"d\"]:\n    out.append(fetch(c))\nprint(len(out))\n";
    let mut first = None;
    for _ in 0..3 {
        let (engine, log) = setup();
        let result = engine.run(src).unwrap();
        assert_eq!(result.stdout(), "4\n");
        let s = spans(&log);
        assert!(
            s.iter().all(|(_, start, end)| *start == 0 && *end == 100),
            "{s:?}"
        );
        match &first {
            None => first = Some(s),
            Some(f) => assert_eq!(f, &s),
        }
    }
}

#[test]
fn check_reports_unknown_tool_with_suggestion() {
    let (engine, _log) = setup();
    let diags = engine.check("x = fetchh(\"a\")\nprint(x)\n");
    assert!(!diags.is_empty());
    let text = format!("{diags:?}");
    assert!(text.contains("fetch"), "{text}");
}
