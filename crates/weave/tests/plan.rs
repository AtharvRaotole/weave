//! `Engine::plan`: the static plan of a script, made before any tool runs.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use weave::{ArgSource, Effect, Engine, Footprint, Param, Params, Plan, PlannedFootprint, ToolSpec, Value};

/// Tools that must never run: planning is static.
fn engine() -> Engine {
    let mut e = Engine::new();
    let never = |_: Vec<Value>, _: BTreeMap<String, Value>| -> Result<Value, String> { panic!("plan ran a tool") };
    e.register("get_weather", ToolSpec::effect(Effect::Read).with_arity(1), never);
    e.register("lookup_owner", ToolSpec::effect(Effect::Read).with_arity(1), never);
    e.register("list_page", ToolSpec::effect(Effect::Read).with_arity(1), never);
    e.register("send_email", ToolSpec::effect(Effect::Irreversible).with_arity(2), never);
    e.register(
        "close_ticket",
        ToolSpec::effect(Effect::Reversible)
            .with_writes(Footprint::parse("ticket:{0|ticket_id}"))
            .with_params(Params { params: vec![Param::new("ticket_id", true), Param::new("reason", false)], ..Params::default() }),
        never,
    );
    e
}

fn plan(src: &str) -> Plan {
    engine().plan(src).unwrap_or_else(|e| panic!("plan failed: {e}"))
}

fn counts(p: &Plan) -> Vec<(String, u64, Option<u64>)> {
    p.calls.iter().map(|c| (c.tool.clone(), c.min_count, c.max_count)).collect()
}

#[test]
fn fan_out_then_dependent_write() {
    let p = plan(
        "temps = {c: get_weather(c) for c in [\"Paris\", \"Tokyo\", \"Lima\"]}\n\
         warmest = max(temps, key=lambda c: temps[c])\n\
         owner = lookup_owner(warmest)\n\
         if temps[warmest] > 20:\n    send_email(owner, f\"hot in {warmest}\")\n\
         print(warmest)\n",
    );
    assert_eq!(
        counts(&p),
        vec![("get_weather".into(), 3, Some(3)), ("lookup_owner".into(), 1, Some(1)), ("send_email".into(), 0, Some(1))]
    );
    assert_eq!(p.calls[0].args[0].source, ArgSource::Literal { values: vec!["'Paris'".into(), "'Tokyo'".into(), "'Lima'".into()] });
    assert_eq!(p.calls[1].data_from, vec![0]);
    assert_eq!(p.calls[2].data_from, vec![0, 1]);
    assert_eq!(p.calls[2].control_from, vec![0]);
    assert_eq!(p.calls[2].args[0].source, ArgSource::Tools { calls: vec![1] });
    assert_eq!(p.calls.iter().map(|c| c.stage).collect::<Vec<_>>(), vec![Some(1), Some(2), Some(3)]);
    assert_eq!((p.min_calls, p.max_calls, p.stages), (4, Some(5), Some(3)));
    assert_eq!((p.max_effect, p.may_write), ("irreversible", true));
    assert_eq!(p.calls[2].writes, Some(PlannedFootprint::All));
}

#[test]
fn literal_write_footprints_are_resolved() {
    let p = plan("for t in [\"T-1\", \"T-2\", \"T-1\"]:\n    close_ticket(t, reason=\"dup\")\nclose_ticket(ticket_id=\"T-9\")\n");
    assert_eq!(counts(&p), vec![("close_ticket".into(), 3, Some(3)), ("close_ticket".into(), 1, Some(1))]);
    assert_eq!(p.calls[0].writes, Some(PlannedFootprint::Names { names: vec!["ticket:T-1".into(), "ticket:T-2".into()] }));
    assert_eq!(p.calls[1].writes, Some(PlannedFootprint::Names { names: vec!["ticket:T-9".into()] }));
    assert_eq!(p.calls[0].args[0].name.as_deref(), Some("ticket_id"));
    assert_eq!(p.calls[0].args[1].name.as_deref(), Some("reason"));
    assert_eq!(p.stages, Some(1));
}

#[test]
fn computed_write_footprints_stay_templates() {
    let p = plan("for t in list_page(\"open\"):\n    close_ticket(t)\n");
    assert_eq!(p.calls[1].writes, Some(PlannedFootprint::Templates { templates: vec!["ticket:{0|ticket_id}".into()] }));
    assert_eq!((p.calls[1].min_count, p.calls[1].max_count), (0, None));
    assert_eq!(p.calls[1].control_from, vec![0]);
    assert_eq!((p.min_calls, p.max_calls), (1, None));
    assert!(p.render().contains("(filled at run time)"));
}

#[test]
fn reads_have_no_write_footprint() {
    let p = plan("get_weather(\"Paris\")\n");
    assert_eq!(p.calls[0].writes, None);
    assert!(!p.may_write);
    assert_eq!(p.max_effect, "read");
}

#[test]
fn loop_bounds() {
    let p = plan(
        "for i in range(2, 10, 3):\n    get_weather(i)\n\
         for i in range(4):\n    for j in [1, 2]:\n        lookup_owner(j)\n\
         cities = [\"a\", \"b\"]\nfor c in cities:\n    list_page(c)\n\
         for i in range(5):\n    if i > 2:\n        break\n    send_email(i, \"x\")\n",
    );
    assert_eq!(
        counts(&p),
        vec![
            ("get_weather".into(), 3, Some(3)),
            ("lookup_owner".into(), 8, Some(8)),
            ("list_page".into(), 2, Some(2)),
            ("send_email".into(), 0, Some(5)),
        ]
    );
    assert_eq!(p.calls[2].args[0].source, ArgSource::Literal { values: vec!["'a'".into(), "'b'".into()] });
}

#[test]
fn rebound_lists_have_unknown_length() {
    let p = plan("ys = [1]\nys = [1, 2]\nfor y in ys:\n    lookup_owner(y)\nzs = {\"a\": [1]}\nzs[\"b\"] = [2]\nfor z in zs:\n    get_weather(z)\n");
    assert_eq!(counts(&p), vec![("lookup_owner".into(), 0, None), ("get_weather".into(), 0, None)]);
    assert_eq!(p.calls[0].args[0].source, ArgSource::Computed);
}

#[test]
fn appended_lists_are_counted_once_they_are_complete() {
    // `append` and comprehensions grow a list; a later loop over it has a known length.
    let p = plan(
        "xs = [\"a\"]\nxs.append(\"b\")\nfor x in xs:\n    get_weather(x)\n\
         owners = [lookup_owner(c) for c in [\"p\", \"q\", \"r\"]]\nfor o in owners:\n    send_email(o, \"hi\")\n\
         picked = [c for c in [\"p\", \"q\"] if c != \"q\"]\nfor c in picked:\n    list_page(c)\n",
    );
    assert_eq!(
        counts(&p),
        vec![
            ("get_weather".into(), 2, Some(2)),
            ("lookup_owner".into(), 3, Some(3)),
            ("send_email".into(), 3, Some(3)),
            ("list_page".into(), 0, Some(2)),
        ]
    );
    // The list changes, so its items are not treated as constants.
    assert_eq!(p.calls[0].args[0].source, ArgSource::Computed);
}

#[test]
fn lists_still_growing_have_unknown_or_partial_length() {
    // Growing inside the loop that reads it: unknown.
    let p = plan("xs = [\"a\"]\nfor x in xs:\n    get_weather(x)\n    xs.append(x)\n");
    assert_eq!(counts(&p), vec![("get_weather".into(), 0, None)]);
    // Read inside the loop that grows it: at most the final length.
    let p = plan("xs = []\nfor i in range(3):\n    xs.append(i)\n    for x in xs:\n        get_weather(x)\n");
    assert_eq!(counts(&p), vec![("get_weather".into(), 0, Some(9))]);
    // A copy taken before the list grows: unknown.
    let p = plan("xs = []\nys = xs\nxs.append(1)\nfor y in ys:\n    get_weather(y)\n");
    assert_eq!(counts(&p), vec![("get_weather".into(), 0, None)]);
}

#[test]
fn pagination_chain_has_no_stage() {
    let p = plan("cursor = \"start\"\nfor _ in range(10):\n    cursor = list_page(cursor)\n    if cursor == \"\":\n        break\nget_weather(cursor)\n");
    assert_eq!(p.calls[0].stage, None);
    assert_eq!(p.calls[1].stage, None);
    assert_eq!(p.stages, None);
    assert_eq!(p.calls[0].data_from, vec![0]);
    assert!(p.render().contains("chains calls on their own results"));
}

#[test]
fn helpers_try_and_short_circuit() {
    let p = plan(
        "def owner_of(city):\n    return lookup_owner(get_weather(city))\n\
         a = owner_of(\"Paris\")\n\
         try:\n    b = list_page(a)\nexcept Exception as err:\n    send_email(\"ops\", str(err))\n\
         ok = a or get_weather(\"Lima\")\n",
    );
    assert_eq!(
        counts(&p),
        vec![
            ("get_weather".into(), 1, Some(1)),
            ("lookup_owner".into(), 1, Some(1)),
            // A caught error could skip it.
            ("list_page".into(), 0, Some(1)),
            ("send_email".into(), 0, Some(1)),
            ("get_weather".into(), 0, Some(1)),
        ]
    );
    // The handler's message can carry the failed call's error.
    assert_eq!(p.calls[3].args[1].source, ArgSource::Tools { calls: vec![2] });
    assert_eq!(p.calls[4].control_from, vec![1]);
}

#[test]
fn constant_conditions_are_folded() {
    let p = plan("if True:\n    get_weather(\"a\")\nelse:\n    lookup_owner(\"b\")\n");
    assert_eq!(counts(&p), vec![("get_weather".into(), 1, Some(1))]);
}

#[test]
fn a_script_that_fails_the_check_has_no_plan() {
    let err = engine().plan("get_wether(\"Paris\")\n").unwrap_err().to_string();
    assert!(err.contains("get_wether"), "{err}");
}

#[test]
fn plan_json_round_trips_through_serde() {
    let p = plan("for t in [\"T-1\"]:\n    close_ticket(t)\n");
    let json: serde_json::Value = serde_json::from_str(&p.to_json()).unwrap();
    assert_eq!(json["calls"][0]["writes"]["kind"], "names");
    assert_eq!(json["calls"][0]["args"][0]["source"]["kind"], "literal");
    assert_eq!(json["max_calls"], 1);
}

// ---- random programs: what the plan says must hold for the real run -------

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

struct Gen {
    rng: Rng,
    vars: Vec<String>,
    lists: Vec<String>,
    next: usize,
    out: String,
}

impl Gen {
    fn lit(&mut self) -> String {
        format!("\"s{}\"", self.rng.below(4))
    }

    fn expr(&mut self, depth: u32) -> String {
        let pick = if depth == 0 { self.rng.below(2) } else { self.rng.below(10) };
        match pick {
            0 => self.lit(),
            1 if !self.vars.is_empty() => self.vars[self.rng.below(self.vars.len() as u64) as usize].clone(),
            1 => self.lit(),
            2 => format!("fetch({})", self.expr(depth - 1)),
            3 => format!("combine({}, {})", self.expr(depth - 1), self.expr(depth - 1)),
            4 => format!("f'{{{}}}-{{{}}}'", self.expr(0), self.expr(0)),
            5 => format!("({} + {})", self.expr(depth - 1), self.expr(depth - 1)),
            6 => format!("{{\"k\": {}}}[\"k\"]", self.expr(depth - 1)),
            7 => format!("({} if \"1\" in {} else {})", self.expr(depth - 1), self.expr(depth - 1), self.expr(depth - 1)),
            8 => format!("wrap({})", self.expr(depth - 1)),
            _ => format!("\"\".join([{} for _q in [1, 2]])", self.expr(depth - 1)),
        }
    }

    fn line(&mut self, indent: usize, text: &str) {
        self.out.push_str(&" ".repeat(indent * 4));
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn block(&mut self, indent: usize, depth: u32, n: u64) {
        for _ in 0..n {
            self.stmt(indent, depth);
        }
    }

    fn stmt(&mut self, indent: usize, depth: u32) {
        let pick = if depth == 0 { self.rng.below(4) } else { self.rng.below(9) };
        match pick {
            0 | 1 => {
                let value = self.expr(2);
                let name = if indent == 0 || self.vars.is_empty() {
                    let n = format!("v{}", self.next);
                    self.next += 1;
                    n
                } else {
                    self.vars[self.rng.below(self.vars.len() as u64) as usize].clone()
                };
                self.line(indent, &format!("{name} = {value}"));
                if indent == 0 && !self.vars.contains(&name) {
                    self.vars.push(name);
                }
            }
            2 => {
                let (k, v) = (self.expr(1), self.expr(2));
                self.line(indent, &format!("store({k}, {v})"));
            }
            3 => {
                let e = self.expr(1);
                self.line(indent, &format!("acc.append({e})"));
            }
            4 => {
                let iter = match self.rng.below(7) {
                    0 => "[\"a\", \"b\"]".to_string(),
                    1 => "acc[:2]".to_string(),
                    2 => "acc".to_string(),
                    3 => "range(3)".to_string(),
                    4 if !self.lists.is_empty() => self.lists[self.rng.below(self.lists.len() as u64) as usize].clone(),
                    5 => format!("[fetch(q) for q in {}]", if self.rng.below(2) == 0 { "acc" } else { "[\"x\", \"y\"]" }),
                    _ => format!("listing({})", self.expr(1)),
                };
                let var = format!("it{}", self.next);
                self.next += 1;
                self.line(indent, &format!("for {var} in {iter}:"));
                self.vars.push(var.clone());
                let n = 1 + self.rng.below(2);
                self.block(indent + 1, depth - 1, n);
                if self.rng.below(4) == 0 {
                    self.line(indent + 1, &format!("if \"1\" in str({var}):"));
                    let jump = if self.rng.below(2) == 0 { "break" } else { "continue" };
                    self.line(indent + 2, jump);
                    self.stmt(indent + 1, 0);
                }
                self.vars.retain(|v| v != &var);
            }
            5 => {
                let t = self.expr(1);
                self.line(indent, &format!("if \"1\" in {t}:"));
                self.block(indent + 1, depth - 1, 1);
                self.line(indent, "else:");
                self.block(indent + 1, depth - 1, 1);
            }
            6 => {
                self.line(indent, "try:");
                self.block(indent + 1, depth - 1, 1);
                self.line(indent, "except Exception as err:");
                let e = self.expr(1);
                self.line(indent + 1, &format!("store(str(err)[:3], {e})"));
            }
            7 if indent == 0 && self.rng.below(2) == 0 => {
                let name = format!("l{}", self.next);
                self.next += 1;
                let text = match self.rng.below(4) {
                    0 => format!("{name} = acc"),
                    1 => format!("{name} = [fetch(q) for q in [\"m\", \"n\", \"o\"] if q != {}]", self.lit()),
                    2 => format!("{name} = [combine(q, \"z\") for q in acc]"),
                    _ => format!("{name} = []"),
                };
                self.line(indent, &text);
                self.lists.push(name);
            }
            _ => {
                if !self.lists.is_empty() && self.rng.below(3) == 0 {
                    let l = self.lists[self.rng.below(self.lists.len() as u64) as usize].clone();
                    let e = self.expr(1);
                    self.line(indent, &format!("{l}.append({e})"));
                    return;
                }
                let e = self.expr(2);
                self.line(indent, &format!("print({e})"));
            }
        }
    }
}

fn random_program(seed: u64) -> String {
    let mut g = Gen { rng: Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1), vars: Vec::new(), lists: Vec::new(), next: 0, out: String::new() };
    g.out.push_str("def wrap(x):\n    return combine(x, \"w\")\nacc = []\n");
    let n = 2 + g.rng.below(5);
    g.block(0, 2, n);
    g.out
}

/// Every result carries a fresh marker `<mN>` followed by (a prefix of) its
/// inputs, so a marker inside a call's arguments proves data flowed from the
/// call that produced it.
fn marker_engine() -> Engine {
    let counter = Arc::new(AtomicU64::new(0));
    let fresh = move |inputs: &[Value]| {
        let n = counter.fetch_add(1, Ordering::SeqCst);
        let mut s = format!("<m{n}>");
        for v in inputs {
            if let Some(t) = v.as_str() {
                s.push_str(t);
            }
        }
        s.truncate(400);
        s
    };
    let mut e = Engine::new();
    let f = fresh.clone();
    e.register("fetch", ToolSpec::effect(Effect::Read).with_arity(1), move |a, _| {
        let s = f(&a);
        if s.contains("s3s3") { Err("boom".into()) } else { Ok(Value::str(&s)) }
    });
    let f = fresh.clone();
    e.register("combine", ToolSpec::effect(Effect::Pure).with_arity(2), move |a, _| Ok(Value::str(f(&a))));
    let f = fresh.clone();
    e.register("listing", ToolSpec::effect(Effect::Read).with_arity(1), move |a, _| {
        let s = f(&a);
        Ok(Value::List(vec![Value::str(&s), Value::str(format!("{s}2"))]))
    });
    let f = fresh;
    e.register("store", ToolSpec::effect(Effect::Reversible).with_arity(2), move |a, _| Ok(Value::str(f(&a))));
    e
}

fn markers(v: &Value, out: &mut BTreeSet<u64>) {
    match v {
        Value::Str(s) => {
            let mut rest = s.as_str();
            while let Some(i) = rest.find("<m") {
                let after = &rest[i + 2..];
                if let Some(j) = after.find('>') {
                    if let Ok(n) = after[..j].parse() {
                        out.insert(n);
                    }
                }
                rest = after;
            }
        }
        Value::List(xs) => xs.iter().for_each(|x| markers(x, out)),
        Value::Dict(xs) => xs.iter().for_each(|(k, x)| {
            markers(k, out);
            markers(x, out);
        }),
        _ => {}
    }
}

fn own_marker(v: &Value) -> Option<u64> {
    let s = match v {
        Value::Str(s) => s.as_str(),
        Value::List(xs) => xs.first()?.as_str()?,
        _ => return None,
    };
    s.strip_prefix("<m")?.split('>').next()?.parse().ok()
}

#[test]
fn plan_provenance_covers_random_runs() {
    let seeds: u64 = std::env::var("WEAVE_PLAN_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(400);
    let (mut flows_checked, mut literals_checked, mut runs, mut exact_multi) = (0u64, 0u64, 0u64, 0u64);
    for seed in 0..seeds {
        let src = random_program(seed);
        let e = marker_engine();
        let p = e.plan(&src).unwrap_or_else(|err| panic!("seed {seed}: {err}\n{src}"));
        let Ok(run) = e.run(&src) else { continue };
        runs += 1;
        exact_multi += p.calls.iter().filter(|c| c.min_count > 1 && c.max_count == Some(c.min_count)).count() as u64;
        let events = &run.trace.events;
        let ok_run = events.iter().all(|ev| ev.error.is_none());
        let mut seen_counts: BTreeMap<(usize, String), u64> = BTreeMap::new();
        for ev in events {
            *seen_counts.entry((ev.line, ev.tool.clone())).or_default() += 1;
        }
        for (key, n) in &seen_counts {
            let sites = p.calls.iter().filter(|c| (c.line, c.tool.clone()) == *key);
            let (min, max) = sites.fold((0u64, Some(0u64)), |(a, b), c| (a + c.min_count, b.zip(c.max_count).map(|(x, y)| x + y)));
            assert!(max.is_none_or(|m| *n <= m), "seed {seed}: {key:?} ran {n}, max {max:?}\n{src}\n{}", p.render());
            assert!(!ok_run || *n >= min, "seed {seed}: {key:?} ran {n}, min {min}\n{src}\n{}", p.render());
        }
        if ok_run {
            for c in &p.calls {
                if c.min_count > 0 {
                    assert!(seen_counts.contains_key(&(c.line, c.tool.clone())), "seed {seed}: #{} never ran\n{src}\n{}", c.id + 1, p.render());
                }
            }
        }
        let origin: BTreeMap<u64, (usize, String)> =
            events.iter().filter_map(|ev| own_marker(&ev.result).map(|m| (m, (ev.line, ev.tool.clone())))).collect();
        for ev in events {
            let sites: Vec<_> = p.calls.iter().filter(|c| c.line == ev.line && c.tool == ev.tool).collect();
            assert!(!sites.is_empty(), "seed {seed}: {}@{} not planned\n{src}", ev.tool, ev.line);
            let mut found = BTreeSet::new();
            for a in ev.args.iter().chain(ev.kwargs.values()) {
                markers(a, &mut found);
            }
            for m in found {
                let Some(from) = origin.get(&m) else { continue };
                // Results carry their inputs, so a marker can arrive through a
                // chain of calls: look in the transitive closure of `data_from`.
                let ok = sites.iter().any(|s| {
                    let mut seen = BTreeSet::new();
                    let mut stack = s.data_from.clone();
                    while let Some(d) = stack.pop() {
                        if seen.insert(d) {
                            stack.extend(p.calls[d].data_from.iter().copied());
                        }
                    }
                    seen.iter().any(|&d| p.calls[d].line == from.0 && p.calls[d].tool == from.1)
                });
                assert!(ok, "seed {seed}: {}@{} got data from {}@{} not in data_from\n{src}\n{}", ev.tool, ev.line, from.1, from.0, p.render());
                flows_checked += 1;
            }
            if sites.len() == 1 {
                for (i, a) in ev.args.iter().enumerate() {
                    if let Some(pa) = sites[0].args.iter().find(|x| x.position == Some(i)) {
                        if let ArgSource::Literal { values } = &pa.source {
                            assert!(values.contains(&weave_repr(a)), "seed {seed}: arg {i} of {}@{} = {a:?}, plan said {values:?}\n{src}", ev.tool, ev.line);
                            literals_checked += 1;
                        }
                    }
                }
            }
        }
    }
    eprintln!("runs {runs}, flows {flows_checked}, literals {literals_checked}, exact repeated sites {exact_multi}");
    assert!(exact_multi > seeds / 10, "only {exact_multi} sites with exact repeat counts");
    // The generator must actually exercise the checks.
    assert!(runs > seeds / 2, "only {runs} of {seeds} runs succeeded");
    assert!(flows_checked > seeds, "only {flows_checked} flows checked");
    assert!(literals_checked > seeds / 4, "only {literals_checked} literal arguments checked");
}

fn weave_repr(v: &Value) -> String {
    match v {
        Value::Str(s) => format!("'{s}'"),
        Value::Int(i) => i.to_string(),
        other => format!("{other:?}"),
    }
}
