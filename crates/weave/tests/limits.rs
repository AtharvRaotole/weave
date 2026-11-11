//! Sandbox limits: steps, value size, nesting depth, and no escape hatches.

use weave::{Engine, Limits};

fn err(engine: &Engine, src: &str) -> String {
    match engine.run(src) {
        Ok(result) => panic!("expected an error, got {:?}", result.stdout()),
        Err(err) => err.to_string(),
    }
}

#[test]
fn step_limit_stops_nested_loops() {
    let mut engine = Engine::new();
    engine.set_limits(Limits::default().with_max_steps(10_000));
    let msg = err(&engine, "n = 0\nfor a in range(1000):\n    for b in range(1000):\n        n += 1\n");
    assert!(msg.contains("step limit of 10000 exceeded"), "{msg}");
    let msg = err(&engine, "for a in range(100000):\n    for b in range(100000):\n        pass\n");
    assert!(msg.contains("step limit"), "{msg}");
}

#[test]
fn step_limit_allows_work_under_budget() {
    let mut engine = Engine::new();
    engine.set_limits(Limits::default().with_max_steps(10_000));
    let out = engine.run("n = 0\nfor a in range(50):\n    for b in range(50):\n        n += 1\nprint(n)\n").unwrap();
    assert_eq!(out.stdout(), "2500\n");
}

#[test]
fn huge_repetition_is_refused_before_allocating() {
    let engine = Engine::new();
    assert!(err(&engine, "x = \"a\" * 10000000000\n").contains("repetition would build"));
    assert!(err(&engine, "x = [1] * 4611686018427387904\n").contains("repetition would build"));
    assert!(err(&engine, "x = [[\"a\" * 1000]] * 1000000\n").contains("repetition would build"));
}

#[test]
fn value_size_limit_stops_doubling() {
    let mut engine = Engine::new();
    engine.set_limits(Limits::default().with_max_value_size(1_000_000));
    let msg = err(&engine, "s = \"ab\"\nfor i in range(40):\n    s = s + s\n");
    assert!(msg.contains("larger than the 1000000 limit"), "{msg}");
    let msg = err(&engine, "s = \"ab\" * 1000\nt = s.replace(\"\", s)\n");
    assert!(msg.contains("larger than"), "{msg}");
}

#[test]
fn value_nesting_is_capped() {
    let engine = Engine::new();
    let msg = err(&engine, "x = [1]\nfor i in range(500):\n    x = [x]\n");
    assert!(msg.contains("nested deeper than 100"), "{msg}");
    let msg = err(&engine, "d = {\"a\": 1}\nfor i in range(500):\n    d[\"a\"] = d\n");
    assert!(msg.contains("nested deeper than 100"), "{msg}");
}

#[test]
fn deep_source_is_rejected_without_overflowing() {
    let engine = Engine::new();
    let parens = format!("x = {}1{}\n", "(".repeat(5000), ")".repeat(5000));
    assert!(err(&engine, &parens).contains("nested deeper than 100"));
    let chain = format!("x = {}\n", vec!["1"; 100_000].join("+"));
    assert!(err(&engine, &chain).contains("more than 1000 operators"));
    let unary = format!("x = {}1\n", "-".repeat(100_000));
    assert!(err(&engine, &unary).contains("more than 1000 operators"));
    let medium = format!("x = {}1\n", "-".repeat(500));
    assert!(err(&engine, &medium).contains("nested deeper than 100"));
    // The same limits apply to a streamed session.
    let mut session = engine.session();
    assert!(session.push(&chain).is_err());
}

#[test]
fn long_flat_literals_are_fine() {
    let engine = Engine::new();
    let src = format!("x = [{}]\nprint(len(x))\n", vec!["1"; 20_000].join(", "));
    assert_eq!(engine.run(&src).unwrap().stdout(), "20000\n");
}

#[test]
fn range_near_i64_max_does_not_overflow() {
    let engine = Engine::new();
    let out = engine
        .run("for i in range(9223372036854775806, 9223372036854775807, 5):\n    print(i)\n")
        .unwrap();
    assert_eq!(out.stdout(), "9223372036854775806\n");
}

#[test]
fn no_escape_hatches() {
    let engine = Engine::new();
    for src in [
        "import os\n",
        "from os import system\n",
        "__import__(\"os\")\n",
        "open(\"/etc/passwd\")\n",
        "eval(\"1\")\n",
        "exec(\"x = 1\")\n",
        "x = (1).__class__\n",
        "x = \"a\".__class__.__mro__\n",
        // Helpers are inlined; these shapes stay out.
        "def f():\n    yield 1\n",
        "def f():\n    def g():\n        pass\n",
        "def f(*a):\n    pass\n",
        "def f():\n    return f()\nf()\n",
        "def f():\n    global x\n",
        "def f():\n    return 1\ng = f\n",
        "class A:\n    pass\n",
        "while True:\n    pass\n",
        "x = lambda: 1\n",
        "globals()\n",
        "getattr(1, \"real\")\n",
    ] {
        assert!(engine.run(src).is_err(), "{src:?} should be rejected");
    }
}

#[test]
fn wall_time_limit_cancels_slow_tools() {
    use std::time::{Duration, Instant};
    use weave::{Effect, ToolSpec, Value};
    let mut engine = Engine::new();
    engine.set_limits(Limits::default().with_max_wall_time(Duration::from_millis(200)));
    engine.register_async("slow", ToolSpec::effect(Effect::Read).with_arity(0), |_, _, _| async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok(Value::Null)
    });
    let started = Instant::now();
    let msg = err(&engine, "x = slow()\nprint(x)\n");
    let elapsed = started.elapsed();
    assert!(msg.contains("time limit of 200ms exceeded"), "{msg}");
    assert!(elapsed < Duration::from_secs(3), "took {elapsed:?}");
}

#[test]
fn wall_time_limit_stops_compute_loops() {
    use std::time::{Duration, Instant};
    let mut engine = Engine::new();
    engine.set_limits(
        Limits::default()
            .with_max_steps(u64::MAX)
            .with_max_wall_time(Duration::from_millis(200)),
    );
    let started = Instant::now();
    let msg = err(&engine, "n = 0\nfor a in range(100000):\n    for b in range(100000):\n        n += 1\n");
    assert!(msg.contains("time limit"), "{msg}");
    assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
}

#[test]
fn wall_time_does_not_count_streaming_wait() {
    use std::time::Duration;
    let mut engine = Engine::new();
    engine.set_limits(Limits::default().with_max_wall_time(Duration::from_millis(100)));
    let mut session = engine.session();
    std::thread::sleep(Duration::from_millis(250));
    session.push("print(1)\n").unwrap();
    assert_eq!(session.finish().unwrap().stdout(), "1\n");
}
