//! Declared error convention (`ToolSpec::with_error_key`): a tool that reports
//! failure in its result, e.g. `{"error": "..."}`, fails at the call like a
//! tool that raises.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use weave::{Effect, Engine, PredictedCall, ToolSpec, Value};

fn dict(pairs: &[(&str, Value)]) -> Value {
    Value::Dict(pairs.iter().map(|(k, v)| (Value::str(*k), v.clone())).collect())
}

/// `lookup(kind)` returns a different result shape per `kind`; `save(x)` counts its calls.
fn engine(key: Option<&str>, saves: &Arc<AtomicUsize>) -> Engine {
    let mut e = Engine::new();
    let mut spec = ToolSpec::effect(Effect::Read).with_arity(1);
    if let Some(k) = key {
        spec = spec.with_error_key(k);
    }
    e.register("lookup", spec, |args, _| {
        Ok(match args[0].as_str().unwrap_or_default() {
            "fail" => dict(&[("error", Value::str("No car found"))]),
            "code" => dict(&[("error", Value::int(404))]),
            "none" => dict(&[("error", Value::Null), ("id", Value::int(7))]),
            "ok" => dict(&[("id", Value::int(7))]),
            "list" => Value::List(vec![dict(&[("error", Value::str("inner"))])]),
            "text" => Value::str("error"),
            _ => Value::Null,
        })
    });
    let s = saves.clone();
    let mut spec = ToolSpec::effect(Effect::Reversible).with_arity(1);
    if let Some(k) = key {
        spec = spec.with_error_key(k);
    }
    e.register("save", spec, move |_, _| {
        s.fetch_add(1, Ordering::SeqCst);
        Ok(dict(&[("saved", Value::Bool(true))]))
    });
    e
}

#[test]
fn error_value_fails_at_the_call_and_stops_later_writes() {
    let saves = Arc::new(AtomicUsize::new(0));
    let e = engine(Some("error"), &saves);
    let err = e.run("r = lookup(\"fail\")\nsave(r)\n").unwrap_err().to_string();
    assert!(err.contains("lookup") && err.contains("No car found"), "{err}");
    assert_eq!(saves.load(Ordering::SeqCst), 0, "a write after the failed call ran");
}

#[test]
fn error_value_is_catchable_and_replays() {
    let saves = Arc::new(AtomicUsize::new(0));
    let e = engine(Some("error"), &saves);
    let src = "try:\n    r = lookup(\"fail\")\nexcept ToolError as err:\n    r = \"fallback\"\n    why = err\n";
    let result = e.run(src).unwrap();
    assert_eq!(result.bindings["r"], Value::str("fallback"));
    assert!(result.bindings["why"].as_str().unwrap().contains("No car found"));
    let ev = &result.trace.events[0];
    assert!(ev.error.as_deref().unwrap_or_default().contains("No car found"), "{ev:?}");
    let replayed = e.replay(src, &result.trace).unwrap();
    assert_eq!(replayed.bindings["r"], result.bindings["r"]);
    assert_eq!(replayed.bindings["why"], result.bindings["why"]);
}

#[test]
fn non_string_error_values_become_the_message() {
    let e = engine(Some("error"), &Arc::default());
    let err = e.run("lookup(\"code\")\n").unwrap_err().to_string();
    assert!(err.contains("404"), "{err}");
}

#[test]
fn other_results_pass_through() {
    let e = engine(Some("error"), &Arc::default());
    // None under the key, no key, the key only in a nested value, a string, None.
    let r = e
        .run("a = lookup(\"none\")\nb = lookup(\"ok\")\nc = lookup(\"list\")\nd = lookup(\"text\")\nf = lookup(\"x\")\n")
        .unwrap();
    assert_eq!(r.bindings["a"], dict(&[("error", Value::Null), ("id", Value::int(7))]));
    assert_eq!(r.bindings["b"], dict(&[("id", Value::int(7))]));
    assert_eq!(r.bindings["c"], Value::List(vec![dict(&[("error", Value::str("inner"))])]));
    assert_eq!(r.bindings["d"], Value::str("error"));
    assert_eq!(r.bindings["f"], Value::Null);
}

#[test]
fn without_a_declared_key_the_value_is_returned() {
    let saves = Arc::new(AtomicUsize::new(0));
    let e = engine(None, &saves);
    let r = e.run("r = lookup(\"fail\")\nsave(r)\n").unwrap();
    assert_eq!(r.bindings["r"], dict(&[("error", Value::str("No car found"))]));
    assert_eq!(saves.load(Ordering::SeqCst), 1);
}

#[test]
fn a_different_key_name_is_honored() {
    let e = engine(Some("failure"), &Arc::default());
    // "error" is not the declared key, so it is a value.
    assert!(e.run("lookup(\"fail\")\n").is_ok());
}

#[test]
fn retries_apply_to_error_values() {
    let mut e = Engine::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    e.register(
        "flaky",
        ToolSpec::effect(Effect::Read).with_retries(2).with_error_key("error"),
        move |_, _| {
            Ok(if c.fetch_add(1, Ordering::SeqCst) == 0 {
                dict(&[("error", Value::str("busy"))])
            } else {
                Value::int(1)
            })
        },
    );
    let r = e.run("v = flaky()\n").unwrap();
    assert_eq!(r.bindings["v"], Value::int(1));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn speculated_error_values_also_fail() {
    let saves = Arc::new(AtomicUsize::new(0));
    let e = engine(Some("error"), &saves);
    let cache = e.speculate(&[PredictedCall::new("lookup", vec![Value::str("fail")])]);
    std::thread::sleep(Duration::from_millis(50));
    let err = e.run_with("r = lookup(\"fail\")\nsave(r)\n", Some(&cache)).unwrap_err().to_string();
    assert!(err.contains("No car found"), "{err}");
    assert_eq!(saves.load(Ordering::SeqCst), 0);
}
