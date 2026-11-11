//! Footprint-aware ordering: a read waits only for earlier writes whose
//! declared footprint overlaps its own, so a loop that writes one ticket does
//! not hold back reads of the others. Without declarations nothing changes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use weave::{Effect, Engine, Footprint, Limits, ToolSpec, Value};

type Store = Arc<Mutex<HashMap<i64, i64>>>;

/// `get(id)` reads ticket `id` (60 ms), `close(id)` writes it (60 ms).
fn engine(declare: bool, log: Arc<Mutex<Vec<String>>>) -> (Engine, Store) {
    let store: Store = Arc::new(Mutex::new((0..10).map(|i| (i, i * 10)).collect()));
    let mut engine = Engine::new();
    let mut read = ToolSpec::effect(Effect::Read).with_arity(1);
    let mut write = ToolSpec::effect(Effect::Reversible);
    write.accepts_kwargs = true;
    if declare {
        read = read.with_reads(Footprint::only(["ticket:{0|id}"]));
        write = write.with_writes(Footprint::only(["ticket:{0|id}"]));
    }
    let (s, l) = (store.clone(), log.clone());
    engine.register("get", read, move |args, _| {
        let id = args[0].as_int().unwrap();
        l.lock().unwrap().push(format!("get:{id}"));
        thread::sleep(Duration::from_millis(60));
        Ok(Value::int(*s.lock().unwrap().get(&id).ok_or("no ticket")?))
    });
    let (s, l) = (store.clone(), log);
    engine.register("close", write, move |args, kwargs| {
        let id = args.first().or_else(|| kwargs.get("id")).and_then(|v| v.as_int()).ok_or("need id")?;
        if id == 99 {
            return Err("close failed".into());
        }
        l.lock().unwrap().push(format!("close:{id}"));
        thread::sleep(Duration::from_millis(60));
        s.lock().unwrap().insert(id, -1);
        Ok(Value::Null)
    });
    (engine, store)
}

const TRIAGE: &str = "closed = 0\nfor i in range(8):\n    v = get(i)\n    if v >= 40:\n        close(i)\n        closed += 1\nprint(closed)\n";

#[test]
fn loops_that_write_overlap_unrelated_reads() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let (engine, store) = engine(true, log.clone());
    let started = Instant::now();
    let out = engine.run(TRIAGE).unwrap().stdout();
    let elapsed = started.elapsed();
    assert_eq!(out, "4\n");
    // 8 reads and 4 writes of 60 ms each would take 720 ms one at a time.
    assert!(elapsed < Duration::from_millis(330), "took {elapsed:?}");
    let s = store.lock().unwrap();
    assert_eq!((0..8).filter(|i| s[i] == -1).count(), 4);
}

#[test]
fn without_footprints_reads_still_wait_for_every_write() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let (engine, _) = engine(false, log.clone());
    let started = Instant::now();
    assert_eq!(engine.run(TRIAGE).unwrap().stdout(), "4\n");
    // Each read after a write waits for it: at least 4 write+read pairs.
    assert!(started.elapsed() >= Duration::from_millis(480), "took {:?}", started.elapsed());
    // Order is unchanged: every read issued after a write starts after it.
    let log = log.lock().unwrap();
    let first_close = log.iter().position(|e| e == "close:4").unwrap();
    assert!(log[first_close + 1..].iter().all(|e| e != "get:4"));
}

#[test]
fn an_overlapping_read_sees_the_write() {
    for declare in [true, false] {
        let log = Arc::new(Mutex::new(Vec::new()));
        let (engine, _) = engine(declare, log);
        let src = "a = get(3)\nclose(3)\nb = get(3)\nc = get(5)\nclose(id=5)\nd = get(5)\nprint(a, b, c, d)\n";
        assert_eq!(engine.run(src).unwrap().stdout(), "30 -1 50 -1\n", "declare={declare}");
        // Same inside a pipelined loop: the read of i after closing i.
        let src = "out = []\nfor i in [1, 2, 1]:\n    out.append(get(i))\n    if i == 1:\n        close(i)\nprint(out)\n";
        assert_eq!(engine.run(src).unwrap().stdout(), "[10, 20, -1]\n", "declare={declare}");
    }
}

#[test]
fn a_failed_write_stops_overlapping_reads_only() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let (engine, _) = engine(true, log.clone());
    let err = engine.run("close(99)\nx = get(99)\ny = get(2)\n").unwrap_err().to_string();
    assert!(err.contains("close failed"), "{err}");
    let log = log.lock().unwrap();
    assert!(!log.contains(&"get:99".to_string()), "{log:?}");
}

#[test]
fn untaken_writes_and_break_issue_nothing_early() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let (engine, _) = engine(true, log.clone());
    let src = "for i in range(6):\n    v = get(i)\n    if v >= 30:\n        close(i)\n        break\nprint(i)\n";
    assert_eq!(engine.run(src).unwrap().stdout(), "3\n");
    let log = log.lock().unwrap();
    let closes: Vec<_> = log.iter().filter(|e| e.starts_with("close")).collect();
    assert_eq!(closes, ["close:3"]);
}

#[test]
fn replay_of_a_pipelined_write_loop_is_exact() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let (engine, _) = engine(true, log.clone());
    let result = engine.run(TRIAGE).unwrap();
    let (fresh, _) = self::engine(true, Arc::new(Mutex::new(Vec::new())));
    let replay = fresh.replay(TRIAGE, &result.trace).unwrap();
    assert_eq!(replay.stdout(), result.stdout());
}

#[test]
fn write_loops_stay_sequential_when_pipelining_is_off() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let (mut engine, _) = engine(true, log);
    engine.set_limits(Limits::default().with_pipeline_loops(false));
    let started = Instant::now();
    assert_eq!(engine.run(TRIAGE).unwrap().stdout(), "4\n");
    assert!(started.elapsed() >= Duration::from_millis(700));
}
