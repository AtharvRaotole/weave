use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use weave::{Effect, Engine, ToolSpec, Value};

fn reg(engine: &mut Engine, name: &str, effect: Effect, calls: Arc<Mutex<Vec<String>>>, delay: Duration) {
    let label = name.to_string();
    let logged = label.clone();
    engine.register(name, ToolSpec::effect(effect).with_arity(1), move |args, _kwargs| {
        calls.lock().unwrap().push(format!("{logged}:{}", args[0]));
        if !delay.is_zero() {
            thread::sleep(delay);
        }
        Ok(Value::str(format!("out-{label}-{}", args[0].as_str().unwrap_or("?"))))
    });
}

#[test]
fn arithmetic_fstring_if_and_for() {
    let mut engine = Engine::new();
    engine.register("fetch", ToolSpec::effect(Effect::Read).with_arity(1), |args, _| {
        Ok(Value::str(format!("v:{}", args[0].as_str().unwrap())))
    });
    let result = engine
        .run(
            r#"
city = "Paris"
temp = fetch(city)
label = f"{city} is {temp}"
if temp.startswith("v:"):
    shown = label.upper()
else:
    shown = "missing"
total = 0
for n in [1, 2, 3]:
    total = total + n
pair = {"city": city, "n": total}
"#,
        )
        .unwrap();
    assert_eq!(result.bindings["shown"], Value::str("PARIS IS V:PARIS"));
    assert_eq!(result.bindings["total"], Value::int(6));
    assert_eq!(
        result.bindings["pair"],
        Value::Dict(vec![
            (Value::str("city"), Value::str("Paris")),
            (Value::str("n"), Value::int(6)),
        ])
    );
}

#[test]
fn static_errors_are_short() {
    let mut engine = Engine::new();
    engine.register("fetch", ToolSpec::effect(Effect::Read).with_arity(1), |_, _| Ok(Value::Null));
    let errors = engine.check("import os\nserch(\"x\")\nprint(missing)\n");
    let text = errors.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n");
    assert!(text.contains("`import` is not supported"), "{text}");
    assert!(text.contains("unknown tool `serch`"), "{text}");
    assert!(text.contains("did you mean `fetch`"), "{text}");
    assert!(text.contains("name `missing` is used before assignment"), "{text}");
    let arity = engine.check("fetch(\"a\", \"b\")\n");
    assert!(arity[0].to_string().contains("takes 1 argument"));
}

#[test]
fn independent_reads_overlap() {
    let mut engine = Engine::new();
    let delay = Duration::from_millis(150);
    let calls = Arc::new(Mutex::new(Vec::new()));
    reg(&mut engine, "slow", Effect::Read, calls.clone(), delay);
    let started = Instant::now();
    let result = engine
        .run(
            r#"
a = slow("a")
b = slow("b")
c = slow("c")
print(a)
print(b)
print(c)
"#,
        )
        .unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(350),
        "parallel reads took {elapsed:?}, expected them to overlap"
    );
    assert_eq!(
        result.stdout(),
        "out-slow-a\nout-slow-b\nout-slow-c\n"
    );
    let replay = engine.replay(
        r#"
a = slow("a")
b = slow("b")
c = slow("c")
print(a)
print(b)
print(c)
"#,
        &result.trace,
    ).unwrap();
    assert_eq!(replay.stdout(), result.stdout());
    assert_eq!(replay.bindings, result.bindings);
    assert_eq!(replay.trace, result.trace);
    assert!(calls.lock().unwrap().len() == 3, "replay must not call tools");
}

#[test]
fn sequential_baseline_is_slower_than_weave() {
    let delay = Duration::from_millis(80);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let sequential = {
        let calls = calls.clone();
        let run_one = |label: &str| {
            calls.lock().unwrap().push(label.to_string());
            thread::sleep(delay);
            format!("out-{label}")
        };
        let started = Instant::now();
        let _ = (run_one("a"), run_one("b"), run_one("c"));
        started.elapsed()
    };
    calls.lock().unwrap().clear();
    let mut engine = Engine::new();
    reg(&mut engine, "slow", Effect::Read, calls, delay);
    let started = Instant::now();
    engine
        .run("a = slow(\"a\")\nb = slow(\"b\")\nc = slow(\"c\")\n")
        .unwrap();
    let parallel = started.elapsed();
    assert!(
        parallel * 2 < sequential,
        "weave {parallel:?} was not faster than sequential {sequential:?}"
    );
}

#[test]
fn irreversible_waits_until_finish_and_stays_ordered() {
    let mut engine = Engine::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let read_log = log.clone();
    engine.register("read", ToolSpec::effect(Effect::Read).with_arity(1), move |args, _| {
        read_log
            .lock()
            .unwrap()
            .push(format!("read:{}", args[0].as_str().unwrap()));
        Ok(Value::str("data"))
    });
    let send_log = log.clone();
    engine.register("send", ToolSpec::effect(Effect::Irreversible).with_arity(1), move |args, _| {
        send_log
            .lock()
            .unwrap()
            .push(format!("send:{}", args[0].as_str().unwrap()));
        Ok(Value::Null)
    });
    let mut session = engine.session();
    session.push("x = read(\"a\")\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && log.lock().unwrap().is_empty() {
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(log.lock().unwrap().as_slice(), ["read:a"], "read should start before finish");
    session.push("send(\"b\")\n").unwrap();
    thread::sleep(Duration::from_millis(80));
    assert_eq!(log.lock().unwrap().as_slice(), ["read:a"], "irreversible must wait for finish");
    let result = session.finish().unwrap();
    assert_eq!(log.lock().unwrap().as_slice(), ["read:a", "send:b"]);
    assert_eq!(result.trace.events.len(), 2);
    assert_eq!(result.trace.events[1].effect, "irreversible");
}

#[test]
fn short_circuit_skips_the_second_call() {
    let mut engine = Engine::new();
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = calls.clone();
    engine.register("flag", ToolSpec::effect(Effect::Pure).with_arity(0), move |_, _| {
        seen.lock().unwrap().push("flag".into());
        Ok(Value::Bool(false))
    });
    let seen = calls.clone();
    engine.register("later", ToolSpec::effect(Effect::Pure).with_arity(0), move |_, _| {
        seen.lock().unwrap().push("later".into());
        Ok(Value::int(1))
    });
    let result = engine.run("x = flag() and later()\n").unwrap();
    assert_eq!(result.bindings["x"], Value::Bool(false));
    assert_eq!(calls.lock().unwrap().as_slice(), ["flag"]);
    assert_eq!(result.trace.events.len(), 1);
}

#[test]
fn parallel_for_overlaps_reads() {
    let mut engine = Engine::new();
    let delay = Duration::from_millis(120);
    let calls = Arc::new(Mutex::new(Vec::new()));
    reg(&mut engine, "slow", Effect::Read, calls, delay);
    let started = Instant::now();
    let result = engine
        .run(
            r#"
for city in ["a", "b", "c"]:
    print(slow(city))
"#,
        )
        .unwrap();
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_millis(300), "loop took {elapsed:?}");
    assert_eq!(result.stdout(), "out-slow-a\nout-slow-b\nout-slow-c\n");
}

#[test]
fn replay_detects_divergence() {
    let mut engine = Engine::new();
    engine.register("fetch", ToolSpec::effect(Effect::Read).with_arity(1), |args, _| {
        Ok(Value::str(args[0].as_str().unwrap()))
    });
    let result = engine.run("x = fetch(\"a\")\n").unwrap();
    let err = engine.replay("x = fetch(\"b\")\n", &result.trace).unwrap_err();
    assert!(err.to_string().contains("replay diverged"), "{err}");
}

#[test]
fn streaming_runs_completed_statement_before_the_rest_arrives() {
    let mut engine = Engine::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    engine.register("go", ToolSpec::effect(Effect::Read).with_arity(0), move |_, _| {
        seen.lock().unwrap().push("go");
        Ok(Value::int(1))
    });
    let mut session = engine.session();
    session.push("x = go()\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && log.lock().unwrap().is_empty() {
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(log.lock().unwrap().as_slice(), ["go"]);
    let result = session.finish().unwrap();
    assert_eq!(result.bindings["x"], Value::int(1));
}

#[test]
fn untagged_tool_is_irreversible() {
    let mut engine = Engine::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    engine.register("send", ToolSpec::default().with_arity(0), move |_, _: BTreeMap<String, Value>| {
        seen.lock().unwrap().push("send");
        Ok(Value::Null)
    });
    let mut session = engine.session();
    session.push("send()\n").unwrap();
    thread::sleep(Duration::from_millis(50));
    assert!(log.lock().unwrap().is_empty());
    session.finish().unwrap();
    assert_eq!(log.lock().unwrap().as_slice(), ["send"]);
}

#[test]
fn reversible_runs_before_program_completes() {
    let mut engine = Engine::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    engine.register("undoable", ToolSpec::effect(Effect::Reversible).with_arity(0), move |_, _| {
        seen.lock().unwrap().push("undoable");
        Ok(Value::int(7))
    });
    let mut session = engine.session();
    session.push("x = undoable()\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && log.lock().unwrap().is_empty() {
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(log.lock().unwrap().as_slice(), ["undoable"]);
    let result = session.finish().unwrap();
    assert_eq!(result.bindings["x"], Value::int(7));
    assert_eq!(result.trace.events[0].effect, "reversible");
}
