//! Speculation must never serve a read that a preceding write made stale.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use weave::{Effect, Engine, Footprint, PredictedCall, ToolSpec, Value};

type Store = Arc<Mutex<HashMap<String, i64>>>;

/// `get_<r>(k)` reads resource `r`, `put_<r>(k, v)` writes it. When
/// `annotate` is false every tool keeps the default footprint (all).
fn engine(store: &Store, reads: &Arc<AtomicUsize>, annotate: bool) -> Engine {
    let mut engine = Engine::new();
    for res in ["a", "b"] {
        let s = store.clone();
        let count = reads.clone();
        let mut spec = ToolSpec::effect(Effect::Read).with_arity(1);
        if annotate {
            spec = spec.with_reads(Footprint::only([res]));
        }
        let prefix = res.to_string();
        engine.register(&format!("get_{res}"), spec, move |args, _| {
            count.fetch_add(1, Ordering::SeqCst);
            let key = format!("{prefix}:{}", args[0].as_str().unwrap_or_default());
            Ok(Value::Int(*s.lock().unwrap().get(&key).unwrap_or(&0)))
        });
        let s = store.clone();
        let mut spec = ToolSpec::effect(Effect::Reversible).with_arity(2);
        if annotate {
            spec = spec.with_writes(Footprint::only([res]));
        }
        let prefix = res.to_string();
        engine.register(&format!("put_{res}"), spec, move |args, _| {
            let key = format!("{prefix}:{}", args[0].as_str().unwrap_or_default());
            let Value::Int(v) = args[1] else { return Err("int".into()) };
            s.lock().unwrap().insert(key, v);
            Ok(Value::Null)
        });
    }
    engine
}

fn wait_started(engine: &Engine, calls: &[PredictedCall]) -> weave::SpecCache {
    let cache = engine.speculate(calls);
    // Let the speculative reads finish so a stale value is really cached.
    std::thread::sleep(Duration::from_millis(50));
    cache
}

#[test]
fn write_before_read_invalidates_default_footprint() {
    let store: Store = Arc::default();
    store.lock().unwrap().insert("a:x".into(), 1);
    let reads = Arc::new(AtomicUsize::new(0));
    let engine = engine(&store, &reads, false);
    let cache = wait_started(&engine, &[PredictedCall::new("get_a", vec![Value::str("x")])]);
    let result = engine
        .run_with("put_a(\"x\", 2)\nv = get_a(\"x\")\nprint(v)\n", Some(&cache))
        .unwrap();
    assert_eq!(result.stdout(), "2\n", "stale speculative read was served");
    let stats = result.speculation.unwrap();
    assert_eq!(stats.invalidated, 1, "{stats:?}");
    assert_eq!(stats.hits, 0, "{stats:?}");
    assert_eq!(stats.misses, 1, "{stats:?}");
    assert_eq!(reads.load(Ordering::SeqCst), 2, "read must re-run live");
}

#[test]
fn default_footprint_write_drops_unrelated_reads_too() {
    let store: Store = Arc::default();
    let reads = Arc::new(AtomicUsize::new(0));
    let engine = engine(&store, &reads, false);
    let cache = wait_started(&engine, &[PredictedCall::new("get_b", vec![Value::str("y")])]);
    let result = engine
        .run_with("put_a(\"x\", 2)\nprint(get_b(\"y\"))\n", Some(&cache))
        .unwrap();
    assert_eq!(result.stdout(), "0\n");
    assert_eq!(result.speculation.unwrap().invalidated, 1);
}

#[test]
fn disjoint_footprints_keep_the_cached_read() {
    let store: Store = Arc::default();
    store.lock().unwrap().insert("b:y".into(), 7);
    let reads = Arc::new(AtomicUsize::new(0));
    let engine = engine(&store, &reads, true);
    let cache = wait_started(
        &engine,
        &[
            PredictedCall::new("get_a", vec![Value::str("x")]),
            PredictedCall::new("get_b", vec![Value::str("y")]),
        ],
    );
    let result = engine
        .run_with(
            "put_a(\"x\", 5)\nprint(get_a(\"x\"), get_b(\"y\"))\n",
            Some(&cache),
        )
        .unwrap();
    assert_eq!(result.stdout(), "5 7\n");
    let stats = result.speculation.unwrap();
    assert_eq!(stats.invalidated, 1, "only get_a overlaps: {stats:?}");
    assert_eq!(stats.hits, 1, "{stats:?}");
    assert_eq!(stats.misses, 1, "{stats:?}");
}

#[test]
fn read_before_write_still_hits_and_sees_old_value() {
    let store: Store = Arc::default();
    store.lock().unwrap().insert("a:x".into(), 1);
    let reads = Arc::new(AtomicUsize::new(0));
    let engine = engine(&store, &reads, false);
    let cache = wait_started(&engine, &[PredictedCall::new("get_a", vec![Value::str("x")])]);
    let result = engine
        .run_with(
            "old = get_a(\"x\")\nput_a(\"x\", old + 1)\nprint(old, get_a(\"x\"))\n",
            Some(&cache),
        )
        .unwrap();
    assert_eq!(result.stdout(), "1 2\n");
    let stats = result.speculation.unwrap();
    assert_eq!(stats.hits, 1, "{stats:?}");
    assert_eq!(stats.misses, 1, "{stats:?}");
}

#[test]
fn async_tools_are_speculated() {
    let mut engine = Engine::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    engine.register_async("fetch", ToolSpec::effect(Effect::Read).with_arity(1), move |args, _, _| {
        let seen = seen.clone();
        async move {
            seen.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok(Value::str(format!("got {}", args[0].as_str().unwrap_or_default())))
        }
    });
    let cache = engine.speculate(&[PredictedCall::new("fetch", vec![Value::str("q")])]);
    let result = engine.run_with("print(fetch(\"q\"))\n", Some(&cache)).unwrap();
    assert_eq!(result.stdout(), "got q\n");
    let stats = result.speculation.unwrap();
    assert_eq!(stats.started, 1, "{stats:?}");
    assert_eq!(stats.hits, 1, "{stats:?}");
    assert_eq!(stats.rejected, 0, "{stats:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn footprint_parse_and_overlap() {
    assert_eq!(Footprint::parse("*"), Footprint::All);
    assert_eq!(Footprint::parse(" a, b "), Footprint::only(["a", "b"]));
    assert!(Footprint::only(["a"]).overlaps(&Footprint::All));
    assert!(!Footprint::only(["a"]).overlaps(&Footprint::only(["b"])));
    assert!(!Footprint::only(Vec::<String>::new()).overlaps(&Footprint::only(["a"])));
}
