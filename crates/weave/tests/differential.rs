//! Differential tests: random programs must give identical results under
//! sequential, parallel, streaming and speculative execution, and replaying
//! the recorded trace must reproduce the same output.
//!
//! "Sequential" tags every tool irreversible, which serialises every call in
//! program order after the whole program is received. That is the reference.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use weave::{Effect, Engine, PredictedCall, RunResult, ToolSpec, Trace, Value};

/// Small deterministic PRNG so failures reproduce from the seed alone.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

const KEYS: [&str; 3] = ["k0", "k1", "k2"];

struct Gen {
    rng: Rng,
    vars: Vec<String>,
    next_var: usize,
}

impl Gen {
    fn expr(&mut self, depth: u32) -> String {
        let pick = if depth == 0 { self.rng.below(3) } else { self.rng.below(7) };
        match pick {
            0 => format!("{}", self.rng.below(10)),
            1 if !self.vars.is_empty() => {
                self.vars[self.rng.below(self.vars.len() as u64) as usize].clone()
            }
            1 | 2 => format!("get(\"{}\")", KEYS[self.rng.below(3) as usize]),
            3 => format!("add({}, {})", self.expr(depth - 1), self.expr(depth - 1)),
            4 => format!("({} + {})", self.expr(depth - 1), self.expr(depth - 1)),
            5 => format!("({} - {})", self.expr(depth - 1), self.expr(depth - 1)),
            _ => format!("put(\"{}\", {})", KEYS[self.rng.below(3) as usize], self.expr(depth - 1)),
        }
    }

    fn stmt(&mut self, out: &mut String, indent: usize, depth: u32) {
        let pad = "    ".repeat(indent);
        match self.rng.below(if depth == 0 { 5 } else { 7 }) {
            0 | 1 => {
                // New variables only at top level so every use is defined.
                let name = if indent == 0 && (self.vars.is_empty() || self.rng.chance(60)) {
                    let name = format!("v{}", self.next_var);
                    self.next_var += 1;
                    name
                } else if let Some(existing) = self.pick_var() {
                    existing
                } else {
                    out.push_str(&format!("{pad}pass\n"));
                    return;
                };
                let value = self.expr(2);
                out.push_str(&format!("{pad}{name} = {value}\n"));
                if indent == 0 && !self.vars.contains(&name) {
                    self.vars.push(name);
                }
            }
            2 => {
                let key = KEYS[self.rng.below(3) as usize];
                let value = self.expr(2);
                out.push_str(&format!("{pad}put(\"{key}\", {value})\n"));
            }
            3 => {
                let value = self.expr(1);
                out.push_str(&format!("{pad}print({value})\n"));
            }
            4 => {
                let value = self.expr(1);
                out.push_str(&format!("{pad}log({value})\n"));
            }
            5 => {
                let test = self.expr(1);
                out.push_str(&format!("{pad}if {test} > 3:\n"));
                self.block(out, indent + 1, depth - 1);
                if self.rng.chance(50) {
                    out.push_str(&format!("{pad}else:\n"));
                    self.block(out, indent + 1, depth - 1);
                }
            }
            _ => {
                let n = self.rng.below(4);
                out.push_str(&format!("{pad}for i in range({n}):\n"));
                self.block(out, indent + 1, depth - 1);
            }
        }
    }

    fn block(&mut self, out: &mut String, indent: usize, depth: u32) {
        let n = 1 + self.rng.below(3);
        for _ in 0..n {
            self.stmt(out, indent, depth);
        }
    }

    fn pick_var(&mut self) -> Option<String> {
        if self.vars.is_empty() {
            None
        } else {
            Some(self.vars[self.rng.below(self.vars.len() as u64) as usize].clone())
        }
    }
}

fn program(seed: u64) -> String {
    let mut gen = Gen {
        rng: Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1),
        vars: Vec::new(),
        next_var: 0,
    };
    let mut out = String::new();
    let n = 2 + gen.rng.below(8);
    for _ in 0..n {
        gen.stmt(&mut out, 0, 2);
    }
    for var in gen.vars.clone() {
        out.push_str(&format!("print({var})\n"));
    }
    out
}

#[derive(Default)]
struct World {
    store: HashMap<String, i64>,
    log: Vec<i64>,
}

fn int(value: &Value) -> Result<i64, String> {
    match value {
        Value::Int(n) => Ok(*n),
        other => Err(format!("expected int, got {other:?}")),
    }
}

/// Tools over a fresh world. `jitter` adds small argument-dependent delays so
/// parallel schedules interleave differently.
fn engine(world: &Arc<Mutex<World>>, sequential: bool, jitter: bool) -> Engine {
    let tag = |effect| ToolSpec::effect(if sequential { Effect::Irreversible } else { effect });
    let delay = move |args: &[Value]| {
        if jitter {
            let h = args.iter().map(|a| format!("{a:?}").len() as u64).sum::<u64>();
            std::thread::sleep(Duration::from_micros(200 * (h % 7)));
        }
    };
    let mut engine = Engine::new();
    engine.register("add", tag(Effect::Pure).with_arity(2), move |args, _| {
        delay(&args);
        Ok(Value::Int(int(&args[0])?.wrapping_add(int(&args[1])?)))
    });
    let w = world.clone();
    engine.register("get", tag(Effect::Read).with_arity(1), move |args, _| {
        delay(&args);
        let key = args[0].as_str().unwrap_or_default().to_string();
        Ok(Value::Int(*w.lock().unwrap().store.get(&key).unwrap_or(&0)))
    });
    let w = world.clone();
    engine.register("put", tag(Effect::Reversible).with_arity(2), move |args, _| {
        delay(&args);
        let key = args[0].as_str().unwrap_or_default().to_string();
        let value = int(&args[1])?;
        let old = w.lock().unwrap().store.insert(key, value).unwrap_or(0);
        Ok(Value::Int(old))
    });
    let w = world.clone();
    engine.register("log", tag(Effect::Irreversible).with_arity(1), move |args, _| {
        delay(&args);
        let mut world = w.lock().unwrap();
        world.log.push(int(&args[0])?);
        Ok(Value::Int(world.log.len() as i64))
    });
    engine
}

/// Prints, final bindings, and calls of a successful run.
type Observed = (Vec<String>, BTreeMap<String, Value>, Vec<String>);

#[derive(Debug, PartialEq)]
struct Outcome {
    result: Result<Observed, String>,
    store: Vec<(String, i64)>,
    log: Vec<i64>,
}

/// Effect tags differ between modes on purpose, so compare calls without them.
fn calls(trace: &Trace) -> Vec<String> {
    trace
        .events
        .iter()
        .map(|e| format!("{}|{}|{:?}|{:?}|{:?}|{:?}", e.seq, e.tool, e.args, e.kwargs, e.result, e.error))
        .collect()
}

fn outcome(world: &Arc<Mutex<World>>, run: Result<RunResult, weave::WeaveError>) -> (Outcome, Option<Trace>) {
    let trace = run.as_ref().ok().map(|r| r.trace.clone());
    let result = run
        .map(|r| (r.prints.clone(), r.bindings.clone(), calls(&r.trace)))
        .map_err(|e| e.to_string());
    let world = world.lock().unwrap();
    let mut store: Vec<_> = world.store.iter().map(|(k, v)| (k.clone(), *v)).collect();
    store.sort();
    (
        Outcome {
            result,
            store,
            log: world.log.clone(),
        },
        trace,
    )
}

fn check_seed(seed: u64) {
    let source = program(seed);
    let ctx = |mode: &str| format!("seed {seed}, mode {mode}\n--- program ---\n{source}");

    let world = Arc::default();
    let (reference, trace) = outcome(&world, engine(&world, true, false).run(&source));

    let world = Arc::default();
    let (parallel, _) = outcome(&world, engine(&world, false, true).run(&source));
    assert_eq!(unordered(&parallel), unordered(&reference), "{}", ctx("parallel"));

    let world = Arc::default();
    let streamed = {
        let engine = engine(&world, false, true);
        let mut session = engine.session();
        let mut rng = Rng(seed | 1);
        let bytes = source.as_bytes();
        let mut start = 0;
        let mut failed = None;
        while start < bytes.len() {
            let mut end = (start + 1 + rng.below(12) as usize).min(bytes.len());
            while !source.is_char_boundary(end) {
                end += 1;
            }
            if let Err(err) = session.push(&source[start..end]) {
                failed = Some(err);
                break;
            }
            start = end;
        }
        match failed {
            Some(err) => Err(err),
            None => session.finish(),
        }
    };
    let (streaming, stream_trace) = outcome(&world, streamed);
    assert_eq!(unordered(&streaming), unordered(&reference), "{}", ctx("streaming"));
    if let Some(trace) = &stream_trace {
        let world = Arc::default();
        let replayed = engine_for_replay(&world).replay(&source, trace);
        assert!(replayed.is_ok(), "{}: {:?}", ctx("streaming replay"), replayed.err());
    }

    // Predict every read and pure call the reference made, with the reference
    // arguments. Reads of keys that the program later writes would be stale if
    // served; invalidation must make them re-run.
    if let Some(trace) = &trace {
        let predictions: Vec<PredictedCall> = trace
            .events
            .iter()
            .filter(|e| e.tool == "get" || e.tool == "add")
            .map(|e| PredictedCall::new(e.tool.clone(), e.args.clone()))
            .collect();
        let world = Arc::default();
        let engine = engine(&world, false, true);
        let cache = engine.speculate(&predictions);
        std::thread::sleep(Duration::from_millis(5));
        let (speculative, _) = outcome(&world, engine.run_with(&source, Some(&cache)));
        assert_eq!(unordered(&speculative), unordered(&reference), "{}", ctx("speculative"));

        let world = Arc::default();
        let replayed = engine_for_replay(&world).replay(&source, trace);
        let replay_ok = replayed.expect("replay of a successful run must succeed");
        let reference_ok = reference.result.as_ref().unwrap();
        assert_eq!(replay_ok.prints, reference_ok.0, "{}", ctx("replay"));
        assert_eq!(replay_ok.bindings, reference_ok.1, "{}", ctx("replay"));
        let mut replay_calls = unordered_calls(calls(&replay_ok.trace));
        replay_calls.sort();
        assert_eq!(replay_calls, unordered_calls(reference_ok.2.clone()), "{}", ctx("replay"));
        let untouched = world.lock().unwrap();
        assert!(untouched.store.is_empty() && untouched.log.is_empty(), "replay must not call tools");
    }
}

/// Loop bodies number their calls when the loop runs, so `seq` depends on
/// scheduling (and on how much of the program had arrived when streaming).
/// Calls are compared as a multiset without `seq`; prints, bindings, final
/// store and log must match exactly.
fn unordered_calls(calls: Vec<String>) -> Vec<String> {
    let mut calls: Vec<String> = calls
        .into_iter()
        .map(|c| c.split_once('|').map(|(_, rest)| rest.to_string()).unwrap_or(c))
        .collect();
    calls.sort();
    calls
}

fn unordered(outcome: &Outcome) -> Outcome {
    let result = outcome
        .result
        .clone()
        .map(|(prints, bindings, calls)| (prints, bindings, unordered_calls(calls)));
    Outcome {
        result,
        store: outcome.store.clone(),
        log: outcome.log.clone(),
    }
}

fn engine_for_replay(world: &Arc<Mutex<World>>) -> Engine {
    engine(world, true, false)
}

#[test]
fn random_programs_agree_across_execution_modes() {
    let seeds: u64 = std::env::var("WEAVE_DIFF_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    for seed in 0..seeds {
        check_seed(seed);
    }
}

#[test]
fn generator_produces_writes_after_reads() {
    // Guard that the generator actually exercises the invalidation path.
    let hits = (0..200)
        .map(program)
        .filter(|p| {
            let get = p.find("get(\"k0\")");
            let put = p.rfind("put(\"k0\"");
            matches!((get, put), (Some(g), Some(p)) if p > g)
        })
        .count();
    assert!(hits > 20, "only {hits} programs read then write the same key");
}
