//! Differential tests: random programs must give identical results under
//! sequential, parallel, streaming and speculative execution, and replaying
//! the recorded trace must reproduce the same output.
//!
//! An independent tree-walking oracle (in this file, sharing no code with
//! Weave) gives the expected result. "Sequential" tags every tool
//! irreversible, which serialises every call in program order after the
//! whole program is received; it must match the oracle, and every other mode
//! must match it.

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

#[derive(Clone, Debug)]
enum E {
    Lit(i64),
    Var(String),
    Get(usize),
    Add(Box<E>, Box<E>),
    Plus(Box<E>, Box<E>),
    Minus(Box<E>, Box<E>),
    Put(usize, Box<E>),
}

#[derive(Clone, Debug)]
enum S {
    Assign(String, E),
    AugAdd(String, E),
    Put(usize, E),
    Print(E),
    Log(E),
    If(E, Vec<S>, Vec<S>),
    For(i64, Vec<S>),
    /// `if e > 3: break` / `if e > 3: continue`; only generated inside loops.
    BreakIf(E),
    ContinueIf(E),
    Pass,
}

struct Gen {
    rng: Rng,
    vars: Vec<String>,
    next_var: usize,
    loop_depth: u32,
}

impl Gen {
    fn expr(&mut self, depth: u32) -> E {
        let pick = if depth == 0 { self.rng.below(3) } else { self.rng.below(7) };
        let key = |g: &mut Gen| g.rng.below(3) as usize;
        match pick {
            0 => E::Lit(self.rng.below(10) as i64),
            1 if !self.vars.is_empty() => E::Var(self.vars[self.rng.below(self.vars.len() as u64) as usize].clone()),
            1 | 2 => E::Get(key(self)),
            3 => E::Add(Box::new(self.expr(depth - 1)), Box::new(self.expr(depth - 1))),
            4 => E::Plus(Box::new(self.expr(depth - 1)), Box::new(self.expr(depth - 1))),
            5 => E::Minus(Box::new(self.expr(depth - 1)), Box::new(self.expr(depth - 1))),
            _ => {
                let k = key(self);
                E::Put(k, Box::new(self.expr(depth - 1)))
            }
        }
    }

    fn stmt(&mut self, top: bool, depth: u32) -> S {
        if self.loop_depth > 0 && self.rng.chance(15) {
            // Conditional assignment that only some iterations make, in a
            // body that can still run in parallel: the value after the loop
            // must come from the last iteration that assigned it.
            if let Some(name) = self.pick_var() {
                let k = self.rng.below(5) as i64;
                // True for the first k + 1 iterations only, so later ones skip it.
                        let test = E::Minus(Box::new(E::Lit(4 + k)), Box::new(E::Var("i".into())));
                let value = E::Plus(Box::new(E::Var("i".into())), Box::new(E::Lit(10 * k)));
                return S::If(test, vec![S::Assign(name, value)], Vec::new());
            }
        }
        if self.loop_depth > 0 && self.rng.chance(20) {
            let test = self.expr(1);
            return if self.rng.chance(50) { S::BreakIf(test) } else { S::ContinueIf(test) };
        }
        match self.rng.below(if depth == 0 { 6 } else { 8 }) {
            0 | 1 => {
                // New variables only at top level so every use is defined.
                let name = if top && (self.vars.is_empty() || self.rng.chance(60)) {
                    let name = format!("v{}", self.next_var);
                    self.next_var += 1;
                    name
                } else if let Some(existing) = self.pick_var() {
                    existing
                } else {
                    return S::Pass;
                };
                let value = self.expr(2);
                if top && !self.vars.contains(&name) {
                    self.vars.push(name.clone());
                }
                S::Assign(name, value)
            }
            2 => S::Put(self.rng.below(3) as usize, self.expr(2)),
            3 => S::Print(self.expr(1)),
            4 => S::Log(self.expr(1)),
            5 => match self.pick_var() {
                Some(name) => S::AugAdd(name, self.expr(1)),
                None => S::Pass,
            },
            6 => {
                let test = self.expr(1);
                let body = self.block(depth - 1);
                let orelse = if self.rng.chance(50) { self.block(depth - 1) } else { Vec::new() };
                S::If(test, body, orelse)
            }
            _ => {
                let n = self.rng.below(4) as i64;
                self.loop_depth += 1;
                let mut body = self.block(depth - 1);
                self.loop_depth -= 1;
                // Sometimes a pure body (no store or log effects) so the loop
                // takes the parallel path with a conditional assignment.
                if self.rng.chance(25) {
                    if let Some(name) = self.pick_var() {
                        let k = self.rng.below(5) as i64;
                        // True for the first k + 1 iterations only, so later ones skip it.
                        let test = E::Minus(Box::new(E::Lit(4 + k)), Box::new(E::Var("i".into())));
                        let value = E::Plus(Box::new(E::Var("i".into())), Box::new(E::Lit(10 * k)));
                        body = vec![S::If(test, vec![S::Assign(name, value)], Vec::new())];
                    }
                }
                S::For(n, body)
            }
        }
    }

    fn block(&mut self, depth: u32) -> Vec<S> {
        let n = 1 + self.rng.below(3);
        (0..n).map(|_| self.stmt(false, depth)).collect()
    }

    fn pick_var(&mut self) -> Option<String> {
        if self.vars.is_empty() {
            None
        } else {
            Some(self.vars[self.rng.below(self.vars.len() as u64) as usize].clone())
        }
    }
}

fn render_e(e: &E) -> String {
    match e {
        E::Lit(n) => n.to_string(),
        E::Var(v) => v.clone(),
        E::Get(k) => format!("get(\"{}\")", KEYS[*k]),
        E::Add(a, b) => format!("add({}, {})", render_e(a), render_e(b)),
        E::Plus(a, b) => format!("({} + {})", render_e(a), render_e(b)),
        E::Minus(a, b) => format!("({} - {})", render_e(a), render_e(b)),
        E::Put(k, v) => format!("put(\"{}\", {})", KEYS[*k], render_e(v)),
    }
}

fn render(stmts: &[S], indent: usize, out: &mut String) {
    let pad = "    ".repeat(indent);
    for s in stmts {
        match s {
            S::Assign(n, e) => out.push_str(&format!("{pad}{n} = {}\n", render_e(e))),
            S::AugAdd(n, e) => out.push_str(&format!("{pad}{n} += {}\n", render_e(e))),
            S::Put(k, e) => out.push_str(&format!("{pad}put(\"{}\", {})\n", KEYS[*k], render_e(e))),
            S::Print(e) => out.push_str(&format!("{pad}print({})\n", render_e(e))),
            S::Log(e) => out.push_str(&format!("{pad}log({})\n", render_e(e))),
            S::If(t, body, orelse) => {
                out.push_str(&format!("{pad}if {} > 3:\n", render_e(t)));
                render(body, indent + 1, out);
                if !orelse.is_empty() {
                    out.push_str(&format!("{pad}else:\n"));
                    render(orelse, indent + 1, out);
                }
            }
            S::For(n, body) => {
                out.push_str(&format!("{pad}for i in range({n}):\n"));
                render(body, indent + 1, out);
            }
            S::BreakIf(t) => out.push_str(&format!("{pad}if {} > 3:\n{pad}    break\n", render_e(t))),
            S::ContinueIf(t) => out.push_str(&format!("{pad}if {} > 3:\n{pad}    continue\n", render_e(t))),
            S::Pass => out.push_str(&format!("{pad}pass\n")),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Flow {
    Next,
    Break,
    Continue,
}

/// Independent tree-walking oracle with Python semantics (left-to-right
/// evaluation, one statement at a time). It shares no code with Weave.
#[derive(Default)]
struct Oracle {
    vars: BTreeMap<String, i64>,
    store: HashMap<String, i64>,
    log: Vec<i64>,
    prints: Vec<String>,
    calls: Vec<String>,
}

impl Oracle {
    fn call(&mut self, tool: &str, args: Vec<Value>, result: i64) -> i64 {
        self.calls.push(format!("{tool}|{args:?}|{{}}|{:?}|None", Value::Int(result)));
        result
    }

    fn eval(&mut self, e: &E) -> i64 {
        match e {
            E::Lit(n) => *n,
            E::Var(v) => self.vars[v],
            E::Get(k) => {
                let r = *self.store.get(KEYS[*k]).unwrap_or(&0);
                self.call("get", vec![Value::str(KEYS[*k])], r)
            }
            E::Add(a, b) => {
                let (x, y) = (self.eval(a), self.eval(b));
                self.call("add", vec![Value::Int(x), Value::Int(y)], x.wrapping_add(y))
            }
            E::Plus(a, b) => {
                let x = self.eval(a);
                x + self.eval(b)
            }
            E::Minus(a, b) => {
                let x = self.eval(a);
                x - self.eval(b)
            }
            E::Put(k, v) => {
                let v = self.eval(v);
                let old = self.store.insert(KEYS[*k].to_string(), v).unwrap_or(0);
                self.call("put", vec![Value::str(KEYS[*k]), Value::Int(v)], old)
            }
        }
    }

    fn run(&mut self, stmts: &[S]) -> Flow {
        for s in stmts {
            match s {
                S::Assign(n, e) => {
                    let v = self.eval(e);
                    self.vars.insert(n.clone(), v);
                }
                S::AugAdd(n, e) => {
                    let old = self.vars[n];
                    let v = self.eval(e);
                    self.vars.insert(n.clone(), old + v);
                }
                S::Put(k, e) => {
                    self.eval(&E::Put(*k, Box::new(e.clone())));
                }
                S::Print(e) => {
                    let v = self.eval(e);
                    self.prints.push(format!("{v}\n"));
                }
                S::Log(e) => {
                    let v = self.eval(e);
                    self.log.push(v);
                    let len = self.log.len() as i64;
                    self.call("log", vec![Value::Int(v)], len);
                }
                S::If(t, body, orelse) => {
                    let flow = if self.eval(t) > 3 { self.run(body) } else { self.run(orelse) };
                    if flow != Flow::Next {
                        return flow;
                    }
                }
                S::For(n, body) => {
                    for i in 0..*n {
                        self.vars.insert("i".into(), i);
                        if self.run(body) == Flow::Break {
                            break;
                        }
                    }
                }
                S::BreakIf(t) => {
                    if self.eval(t) > 3 {
                        return Flow::Break;
                    }
                }
                S::ContinueIf(t) => {
                    if self.eval(t) > 3 {
                        return Flow::Continue;
                    }
                }
                S::Pass => {}
            }
        }
        Flow::Next
    }
}

fn build(seed: u64) -> Vec<S> {
    let mut gen = Gen {
        rng: Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1),
        vars: Vec::new(),
        next_var: 0,
        loop_depth: 0,
    };
    let n = 2 + gen.rng.below(8);
    let mut stmts: Vec<S> = (0..n).map(|_| gen.stmt(true, 2)).collect();
    for var in gen.vars.clone() {
        stmts.push(S::Print(E::Var(var)));
    }
    stmts
}

fn program(seed: u64) -> String {
    let mut out = String::new();
    render(&build(seed), 0, &mut out);
    out
}

fn oracle(seed: u64) -> Outcome {
    let mut o = Oracle::default();
    o.run(&build(seed));
    let mut store: Vec<_> = o.store.iter().map(|(k, v)| (k.clone(), *v)).collect();
    store.sort();
    let bindings = o.vars.iter().map(|(k, v)| (k.clone(), Value::Int(*v))).collect();
    let calls = o.calls.iter().enumerate().map(|(i, c)| format!("{i}|{c}")).collect();
    Outcome {
        result: Ok((o.prints, bindings, calls)),
        store,
        log: o.log,
    }
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
    assert_eq!(unordered(&reference), unordered(&oracle(seed)), "{}", ctx("sequential vs oracle"));

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
        let replayed = engine(&world, false, false).replay(&source, trace);
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

#[test]
fn generator_produces_break_and_continue() {
    let programs: Vec<String> = (0..300).map(program).collect();
    let breaks = programs.iter().filter(|p| p.contains("break")).count();
    let continues = programs.iter().filter(|p| p.contains("continue")).count();
    assert!(breaks >= 20 && continues >= 20, "breaks={breaks} continues={continues}");
}
