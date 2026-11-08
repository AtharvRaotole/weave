use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use weave::{CancelToken, Effect, Engine, IsolatedJob, PredictedCall, ToolSpec, Value};

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

#[test]
fn errors_include_a_fix_hint() {
    let mut engine = Engine::new();
    engine.register("fetch", ToolSpec::effect(Effect::Read).with_arity(1), |_, _| Ok(Value::Null));
    let text = engine
        .check("serch(\"x\")\nprint(missing)\n")
        .into_iter()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("L1:C1:"), "{text}");
    assert!(text.contains("hint: did you mean `fetch`?"), "{text}");
    assert!(text.contains("hint: assign `missing`"), "{text}");
}

#[test]
fn try_except_catches_tool_errors_and_replays() {
    let mut engine = Engine::new();
    engine.register("fetch", ToolSpec::effect(Effect::Read).with_arity(1), |args, _| {
        let key = args[0].as_str().unwrap();
        if key == "missing" {
            Err("not found".into())
        } else {
            Ok(Value::str(key))
        }
    });
    let source = r#"
try:
    value = fetch("missing")
except ToolError as err:
    value = "fallback"
    reason = err
"#;
    let result = engine.run(source).unwrap();
    assert_eq!(result.bindings["value"], Value::str("fallback"));
    assert!(result.bindings["reason"].as_str().unwrap().contains("not found"));
    let replayed = engine.replay(source, &result.trace).unwrap();
    assert_eq!(replayed.bindings["value"], result.bindings["value"]);
    assert_eq!(replayed.bindings["reason"], result.bindings["reason"]);
}

#[test]
fn bare_except_catches_runtime_errors() {
    let engine = Engine::new();
    let result = engine
        .run("try:\n    bad = [1][3]\nexcept:\n    bad = 0\n")
        .unwrap();
    assert_eq!(result.bindings["bad"], Value::int(0));
}

#[test]
fn tool_timeout_and_retry_are_honored() {
    let mut engine = Engine::new();
    engine.register(
        "slow",
        ToolSpec::effect(Effect::Pure).with_arity(0).with_timeout(Duration::from_millis(30)),
        |_, _| {
            thread::sleep(Duration::from_millis(200));
            Ok(Value::int(1))
        },
    );
    let started = Instant::now();
    let err = engine.run("slow()\n").unwrap_err();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(150) && err.to_string().contains("timed out"),
        "elapsed={elapsed:?} err={err}"
    );
    assert!(err.to_string().contains("hint:"), "{err}");

    let mut engine = Engine::new();
    let tries = Arc::new(Mutex::new(0u32));
    let seen = tries.clone();
    engine.register(
        "flaky",
        ToolSpec::effect(Effect::Pure).with_arity(0).with_retries(2),
        move |_, _| {
            let mut n = seen.lock().unwrap();
            *n += 1;
            if *n < 3 {
                Err("not yet".into())
            } else {
                Ok(Value::int(9))
            }
        },
    );
    let result = engine.run("x = flaky()\n").unwrap();
    assert_eq!(result.bindings["x"], Value::int(9));
    assert_eq!(*tries.lock().unwrap(), 3);
}

#[test]
fn cancel_prevents_irreversible_tool() {
    let mut engine = Engine::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    engine.register("send", ToolSpec::default().with_arity(0), move |_, _| {
        seen.lock().unwrap().push("sent");
        Ok(Value::Null)
    });
    let mut session = engine.session();
    session.push("send()\n").unwrap();
    thread::sleep(Duration::from_millis(40));
    session.cancel();
    let err = session.finish().unwrap_err();
    assert!(err.to_string().contains("cancelled"), "{err}");
    assert!(log.lock().unwrap().is_empty(), "cancelled send still ran");
}

#[test]
fn async_timeout_drops_the_in_flight_call() {
    let mut engine = Engine::new();
    let dropped = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let dropped_flag = dropped.clone();
    let finished_flag = finished.clone();
    engine.register_async(
        "slow",
        ToolSpec::effect(Effect::Read).with_arity(0).with_timeout(Duration::from_millis(40)),
        move |_args, _kwargs, token: CancelToken| {
            let dropped_flag = dropped_flag.clone();
            let finished_flag = finished_flag.clone();
            async move {
                struct Mark(Arc<AtomicBool>);
                impl Drop for Mark {
                    fn drop(&mut self) {
                        self.0.store(true, Ordering::SeqCst);
                    }
                }
                let _mark = Mark(dropped_flag);
                tokio::select! {
                    _ = token.cancelled() => Err("cancelled".into()),
                    _ = tokio::time::sleep(Duration::from_millis(400)) => {
                        finished_flag.store(true, Ordering::SeqCst);
                        Ok(Value::int(1))
                    }
                }
            }
        },
    );
    let started = Instant::now();
    let err = engine.run("slow()\n").unwrap_err();
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_millis(200), "elapsed={elapsed:?} err={err}");
    assert!(err.to_string().contains("timed out"), "{err}");
    assert!(dropped.load(Ordering::SeqCst), "async call was not dropped");
    assert!(!finished.load(Ordering::SeqCst), "async call ran to completion");
}

#[test]
fn cooperative_timeout_signals_the_token() {
    let mut engine = Engine::new();
    let saw_cancel = Arc::new(AtomicBool::new(false));
    let flag = saw_cancel.clone();
    engine.register_cooperative(
        "slow",
        ToolSpec::effect(Effect::Read).with_arity(0).with_timeout(Duration::from_millis(40)),
        move |_args, _kwargs, token: CancelToken| {
            let started = std::time::Instant::now();
            while started.elapsed() < Duration::from_millis(400) {
                if token.is_cancelled() {
                    flag.store(true, Ordering::SeqCst);
                    return Err("stopped".into());
                }
                thread::sleep(Duration::from_millis(5));
            }
            Ok(Value::int(1))
        },
    );
    let started = Instant::now();
    let err = engine.run("slow()\n").unwrap_err();
    assert!(started.elapsed() < Duration::from_millis(200), "{err}");
    let deadline = Instant::now() + Duration::from_millis(200);
    while Instant::now() < deadline && !saw_cancel.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(saw_cancel.load(Ordering::SeqCst), "tool never observed cancellation");
}

#[test]
fn extra_builtins_and_methods() {
    let engine = Engine::new();
    let result = engine
        .run(
            r#"
nums = [3, 1, 2]
ordered = sorted(nums)
total = sum(nums)
pairs = list(zip(ordered, reversed(ordered)))
title = "paris".title()
found = title.find("is")
items = {"b": 1, "a": 2}.items()
"#,
        )
        .unwrap();
    assert_eq!(result.bindings["ordered"], Value::List(vec![Value::int(1), Value::int(2), Value::int(3)]));
    assert_eq!(result.bindings["total"], Value::int(6));
    assert_eq!(result.bindings["title"], Value::str("Paris"));
    assert_eq!(result.bindings["found"], Value::int(3));
    assert_eq!(
        result.bindings["pairs"],
        Value::List(vec![
            Value::List(vec![Value::int(1), Value::int(3)]),
            Value::List(vec![Value::int(2), Value::int(2)]),
            Value::List(vec![Value::int(3), Value::int(1)]),
        ])
    );
    match &result.bindings["items"] {
        Value::List(rows) => assert_eq!(rows.len(), 2),
        other => panic!("items was {other}"),
    }
}

#[test]
fn isolated_timeout_kills_the_job() {
    let mut engine = Engine::new();
    let killed = Arc::new(AtomicBool::new(false));
    let flag = killed.clone();
    engine.register_isolated(
        "slow",
        ToolSpec::effect(Effect::Read).with_arity(0).with_timeout(Duration::from_millis(40)),
        move |_args, _kwargs| {
            let flag = flag.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            let rx = Arc::new(Mutex::new(rx));
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(400));
                let _ = tx.send(Ok(Value::str("late")));
            });
            let wait_rx = rx.clone();
            Ok(IsolatedJob::new(
                move || {
                    wait_rx
                        .lock()
                        .expect("rx")
                        .recv()
                        .unwrap_or_else(|_| Err("worker gone".into()))
                },
                move || flag.store(true, Ordering::SeqCst),
            ))
        },
    );
    let started = Instant::now();
    let err = engine.run("slow()\n").unwrap_err();
    assert!(started.elapsed() < Duration::from_millis(200), "{err} elapsed={:?}", started.elapsed());
    assert!(err.to_string().contains("timed out"), "{err}");
    assert!(killed.load(Ordering::SeqCst), "kill was not called");
}

#[test]
fn speculation_serves_pure_calls_and_rejects_irreversible() {
    let mut engine = Engine::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    engine.register("search", ToolSpec::effect(Effect::Pure).with_arity(1), move |args, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(30));
        Ok(args[0].clone())
    });
    engine.register("pay", ToolSpec::effect(Effect::Irreversible).with_arity(1), |_args, _| {
        Ok(Value::str("paid"))
    });
    let cache = engine.speculate(&[
        PredictedCall::new("search", vec![Value::str("q")]),
        PredictedCall::new("pay", vec![Value::str("x")]),
        PredictedCall::new("missing", vec![Value::str("x")]),
    ]);
    let result = engine
        .run_with("a = search(\"q\")\nb = search(\"z\")\nprint(a)\n", Some(&cache))
        .unwrap();
    assert_eq!(result.stdout(), "q\n");
    let stats = result.speculation.expect("stats");
    assert_eq!(stats.started, 1, "{stats:?}");
    assert_eq!(stats.hits, 1, "{stats:?}");
    assert_eq!(stats.misses, 1, "{stats:?}");
    assert_eq!(stats.wasted, 0, "{stats:?}");
    assert_eq!(stats.rejected, 2, "{stats:?}");
    assert_eq!(stats.hit_rate(), 0.5);
    assert_eq!(calls.load(Ordering::SeqCst), 2, "a hit must not call the tool again");
}

#[test]
fn augmented_assignment_in_a_loop_is_carried() {
    let engine = Engine::new();
    let src = "n = 0\nfor a in range(3):\n    n += 1\nk = 0\nfor a in range(3):\n    for b in range(4):\n        k += 1\nprint(n, k)\n";
    assert_eq!(engine.run(src).unwrap().stdout(), "3 12\n");
}

#[test]
fn list_append_and_extend_on_a_name_accumulate_in_loops() {
    let mut engine = Engine::new();
    engine.register("priority", ToolSpec::effect(Effect::Read).with_arity(1), |args, _| {
        let id = args[0].as_str().unwrap();
        Ok(Value::str(if id == "t8" || id == "t16" { "high" } else { "low" }))
    });
    let source = r#"
high = []
for t in ["t4", "t8", "t15", "t16"]:
    if priority(t) == "high":
        high.append(t)
high.extend(["x"])
out = ",".join(high)
"#;
    let result = engine.run(source).unwrap();
    assert_eq!(result.bindings["out"], Value::str("t8,t16,x"));
    let replayed = engine.replay(source, &result.trace).unwrap();
    assert_eq!(replayed.bindings["out"], result.bindings["out"]);
}

#[test]
fn append_on_a_non_name_is_still_an_error() {
    let engine = Engine::new();
    let err = engine.run("d = {\"k\": []}\nd[\"k\"].append(1)\n").unwrap_err();
    assert!(err.to_string().contains("append"), "{err}");
}
