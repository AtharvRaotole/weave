//! Loop read prefetch: in-order loops (accumulators) start their
//! unconditional, loop-invariant read calls for every iteration up front.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use weave::{Effect, Engine, Limits, ToolSpec, Value};

fn engine(delay_ms: u64, calls: Arc<Mutex<Vec<String>>>) -> Engine {
    let mut engine = Engine::new();
    let log = calls.clone();
    engine.register("get", ToolSpec::effect(Effect::Read).with_arity(1), move |args, _| {
        log.lock().unwrap().push(format!("get:{}", args[0]));
        thread::sleep(Duration::from_millis(delay_ms));
        let n = args[0].as_int().unwrap_or(0);
        if n < 0 {
            return Err("negative id".into());
        }
        Ok(Value::int(n * 10))
    });
    let log = calls.clone();
    engine.register("put", ToolSpec::effect(Effect::Irreversible).with_arity(1), move |args, _| {
        log.lock().unwrap().push(format!("put:{}", args[0]));
        Ok(Value::Null)
    });
    engine
}

fn run(engine: &Engine, src: &str) -> (String, Duration) {
    let started = Instant::now();
    let result = engine.run(src).unwrap();
    (result.stdout(), started.elapsed())
}

#[test]
fn accumulator_loops_overlap_their_reads() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let engine = engine(100, calls.clone());
    for src in [
        "total = 0\nfor i in range(8):\n    total += get(i)\nprint(total)\n",
        "rows = []\nfor i in range(8):\n    rows.append(get(i))\nprint(sum(rows))\n",
        "best = None\nfor i in range(8):\n    v = get(i)\n    if best is None or v > best:\n        best = v\nprint(best + 210)\n",
        "d = {}\nfor i in range(8):\n    d[i] = get(i)\nprint(sum(d.values()))\n",
    ] {
        calls.lock().unwrap().clear();
        let (out, elapsed) = run(&engine, src);
        assert_eq!(out, "280\n", "{src}");
        assert!(elapsed < Duration::from_millis(450), "{src} took {elapsed:?}");
        assert_eq!(calls.lock().unwrap().len(), 8, "{src}");
    }
}

#[test]
fn prefetch_can_be_disabled() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut engine = engine(60, calls);
    engine.set_limits(Limits::default().with_loop_prefetch(false));
    let (out, elapsed) = run(&engine, "total = 0\nfor i in range(6):\n    total += get(i)\nprint(total)\n");
    assert_eq!(out, "150\n");
    assert!(elapsed >= Duration::from_millis(350), "sequential run took {elapsed:?}");
}

#[test]
fn break_and_conditional_calls_are_not_issued_early() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let engine = engine(5, calls.clone());
    let (out, _) = run(&engine, "hit = None\nfor i in range(10):\n    if get(i) >= 30:\n        hit = i\n        break\nprint(hit)\n");
    assert_eq!(out, "3\n");
    assert_eq!(calls.lock().unwrap().len(), 4, "break must not prefetch later items");
    calls.lock().unwrap().clear();
    let (out, _) = run(&engine, "n = 0\nfor i in range(6):\n    if i % 2 == 0:\n        n += get(i)\nprint(n)\n");
    assert_eq!(out, "60\n");
    assert_eq!(calls.lock().unwrap().len(), 3, "conditional calls are not prefetched");
}

#[test]
fn dependent_chains_and_writes_keep_their_order() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let engine = engine(5, calls.clone());
    // The argument depends on the previous iteration: no prefetch possible.
    let (out, _) = run(&engine, "x = 1\nfor i in range(3):\n    x = get(x)\nprint(x)\n");
    assert_eq!(out, "1000\n");
    calls.lock().unwrap().clear();
    // A write in the body disables prefetch: read, write, read, write ...
    let (out, _) = run(&engine, "s = 0\nfor i in range(3):\n    s += get(i)\n    put(s)\nprint(s)\n");
    assert_eq!(out, "30\n");
    // Irreversible calls wait for the script to finish; reads keep order.
    let log = calls.lock().unwrap().clone();
    let gets: Vec<_> = log.iter().filter(|c| c.starts_with("get")).cloned().collect();
    let puts: Vec<_> = log.iter().filter(|c| c.starts_with("put")).cloned().collect();
    assert_eq!(gets, ["get:0", "get:1", "get:2"]);
    assert_eq!(puts, ["put:0", "put:10", "put:30"]);
}

#[test]
fn per_item_failures_are_caught_in_order() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let engine = engine(50, calls);
    let src = "out = []\nfor i in [1, -1, 2, -2, 3]:\n    try:\n        out.append(get(i))\n    except ToolError:\n        out.append(-1)\nprint(out)\n";
    let (out, elapsed) = run(&engine, src);
    assert_eq!(out, "[10, -1, 20, -1, 30]\n");
    assert!(elapsed < Duration::from_millis(220), "took {elapsed:?}");
}

#[test]
fn prefetched_runs_replay_exactly() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let engine = engine(10, calls.clone());
    let src = "total = 0\nfor i in range(5):\n    total += get(i)\nprint(total)\n";
    let result = engine.run(src).unwrap();
    let before = calls.lock().unwrap().len();
    let replay = engine.replay(src, &result.trace).unwrap();
    assert_eq!(replay.stdout(), result.stdout());
    assert_eq!(replay.bindings, result.bindings);
    assert_eq!(calls.lock().unwrap().len(), before, "replay must not call tools");
}

#[test]
fn filled_dict_in_parallel_shaped_loop_keeps_every_key() {
    // Regression: `d[k] = v` in a read-only loop used to be scheduled as an
    // independent parallel loop and dropped every assignment.
    let calls = Arc::new(Mutex::new(Vec::new()));
    let engine = engine(1, calls);
    let (out, _) = run(&engine, "d = {}\nfor i in range(4):\n    d[i] = get(i)\nprint(d)\n");
    assert_eq!(out, "{0: 0, 1: 10, 2: 20, 3: 30}\n");
}
