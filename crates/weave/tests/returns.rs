//! Declared return types (`ToolSpec::with_returns`, `schema_from_shape`):
//! lookups on a tool result that the declaration says can never work are
//! rejected before any tool runs.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::{json, Value as Json};
use weave::{schema_from_shape, Effect, Engine, ToolSpec, Value};

fn shape(text: &str) -> Json {
    schema_from_shape(text).unwrap_or_else(|| panic!("not a shape: {text}"))
}

#[test]
fn shapes_become_json_schemas() {
    assert_eq!(
        shape(r#"{"id": int, "note": str?}"#),
        json!({"type": "object", "properties": {"id": {"type": "integer"}, "note": {"type": "string"}},
               "required": ["id"], "additionalProperties": false})
    );
    assert_eq!(
        shape(r#"{"id": int, ...}"#),
        json!({"type": "object", "properties": {"id": {"type": "integer"}}, "required": ["id"]})
    );
    assert_eq!(shape("{...}"), json!({"type": "object", "properties": {}, "required": []}));
    assert_eq!(shape("{}"), json!({"type": "object", "properties": {}, "required": [], "additionalProperties": false}));
    assert_eq!(shape("list[float]"), json!({"type": "array", "items": {"type": "number"}}));
    assert_eq!(shape("list"), json!({"type": "array"}));
    assert_eq!(shape("dict"), json!({"type": "object"}));
    assert_eq!(shape("dict[str, bool]"), json!({"type": "object", "additionalProperties": {"type": "boolean"}}));
    assert_eq!(shape("int | None"), json!({"anyOf": [{"type": "integer"}, {"type": "null"}]}));
    assert_eq!(shape(r#""a" | "b""#), json!({"enum": ["a", "b"]}));
    assert_eq!(shape("any"), json!({}));
    assert_eq!(
        shape(r#"{"a\"b": list[{"x": str}]}"#)["properties"]["a\"b"]["items"]["required"],
        json!(["x"])
    );
    for bad in ["", "{'id': str}", "List[int]", "{\"a\": }", "list[int", "{\"a\": int,}", "dict[str]", "Order", "{\"a\": int} x"] {
        assert_eq!(schema_from_shape(bad), None, "{bad}");
    }
    // Deep nesting is refused instead of overflowing the stack.
    let deep = "list[".repeat(10_000) + "int" + &"]".repeat(10_000);
    assert_eq!(schema_from_shape(&deep), None);
}

fn engine_with(calls: Arc<AtomicUsize>) -> Engine {
    let mut e = Engine::new();
    let status = |_: Vec<Value>, _: BTreeMap<String, Value>| -> Result<Value, String> {
        Ok(Value::Dict(vec![(Value::Str("fuelLevel".into()), Value::Float(3.0))]))
    };
    e.register(
        "car_status",
        ToolSpec::effect(Effect::Read)
            .with_returns(shape(r#"{"fuelLevel": float?, "doors": {"driver": str}?, "tags": list[str]}"#)),
        status,
    );
    let c = calls.clone();
    e.register(
        "fill_tank",
        ToolSpec::effect(Effect::Reversible).with_returns(json!({"type": "object", "properties": {"ok": {"type": "boolean"}}})),
        move |_, _| {
            c.fetch_add(1, Ordering::SeqCst);
            Ok(Value::Dict(vec![(Value::Str("ok".into()), Value::Bool(true))]))
        },
    );
    let c = calls.clone();
    e.register(
        "orders",
        ToolSpec::effect(Effect::Read).with_returns(shape(r#"list[{"id": int, "name": str}]"#)),
        move |_, _| {
            c.fetch_add(1, Ordering::SeqCst);
            Ok(Value::List(vec![]))
        },
    );
    e.register(
        "catalog",
        ToolSpec::effect(Effect::Read).with_returns(shape(r#"{"variants": dict[str, {"price": float}]}"#)),
        |_, _| Ok(Value::Null),
    );
    e.register("untyped", ToolSpec::effect(Effect::Read), |_, _| Ok(Value::Null));
    e
}

fn messages(src: &str) -> Vec<String> {
    engine_with(Arc::new(AtomicUsize::new(0)))
        .check(src)
        .into_iter()
        .map(|d| format!("L{}:C{} {} | {}", d.line, d.col, d.message, d.hint.unwrap_or_default()))
        .collect()
}

#[track_caller]
fn flags(src: &str, expect: &[&str]) {
    let got = messages(src);
    assert_eq!(got.len(), expect.len(), "{src}\n{got:#?}");
    for (g, e) in got.iter().zip(expect) {
        assert!(g.contains(e), "{src}\nexpected `{e}` in `{g}`");
    }
}

#[track_caller]
fn clean(src: &str) {
    let got = messages(src);
    assert!(got.is_empty(), "{src}\n{got:#?}");
}

#[test]
fn missing_keys_on_closed_objects() {
    flags(
        "s = car_status(\"fuel\")\nprint(s[\"status\"][\"fuelLevel\"])\n",
        &["L2:C7 car_status() has no key 'status'; its declared keys are 'doors', 'fuelLevel', 'tags'"],
    );
    flags("print(car_status(1)[\"fuel\"])\n", &["did you mean 'fuelLevel'?"]);
    flags("print(car_status(1)[\"fuellevel\"])\n", &["did you mean 'fuelLevel'?"]);
    flags("s = car_status(1)\nprint(s[\"doors\"][\"drivr\"])\n", &["car_status()['doors'] has no key 'drivr'; its declared keys are 'driver' | did you mean 'driver'?"]);
    flags("d = car_status(1)[\"doors\"]\nx = d\nprint(x[\"rear\"])\n", &["car_status()['doors'] has no key 'rear'"]);
    // Optional and required keys are both fine.
    clean("s = car_status(1)\nprint(s[\"fuelLevel\"], s[\"doors\"][\"driver\"], s[\"tags\"][0])\n");
}

#[test]
fn open_objects_and_untyped_tools_are_not_checked() {
    clean("r = fill_tank(1)\nprint(r[\"anything\"])\n");
    clean("r = untyped()\nprint(r[\"anything\"][0][\"x\"])\n");
    clean("print(catalog()[\"variants\"][\"v1\"][\"price\"])\n");
    flags("print(catalog()[\"variants\"][\"v1\"][\"cost\"])\n", &["catalog()['variants']['v1'] has no key 'cost'"]);
    flags("c = catalog()\nfor k in c[\"variants\"]:\n    print(c[\"variants\"][k][\"cost\"])\n", &["has no key 'cost'"]);
}

#[test]
fn wrong_types() {
    flags("r = orders()\nprint(r[\"id\"])\n", &["can't look up 'id' in orders(): it is declared as a list"]);
    flags("print(car_status(1)[\"tags\"][\"x\"])\n", &["it is declared as a list"]);
    flags("print(car_status(1)[\"tags\"][0][\"x\"])\n", &["can't look up 'x' in an item of car_status()['tags']: it is declared as a str"]);
    clean("r = orders()\nprint(r[0][\"id\"], r[-1][\"name\"])\n");
}

#[test]
fn loop_items_and_comprehensions() {
    flags("for o in orders():\n    print(o[\"nme\"])\n", &["L2:C11 an item of orders() has no key 'nme'; its declared keys are 'id', 'name' | did you mean 'name'?"]);
    flags("print([o[\"idd\"] for o in orders()])\n", &["has no key 'idd'"]);
    flags("ids = {o[\"id\"]: o[\"title\"] for o in orders()}\n", &["has no key 'title'"]);
    flags("def name_of(o):\n    return o[\"title\"]\nfor o in orders():\n    print(name_of(o))\n", &["L2:C12 an item of orders() has no key 'title'"]);
    clean("for o in orders():\n    print(o[\"id\"], o[\"name\"])\n");
}

#[test]
fn guarded_lookups_are_not_reported() {
    clean("s = car_status(1)\nif \"error\" in s:\n    print(s[\"error\"])\n");
    clean("s = car_status(1)\nif s.get(\"error\"):\n    print(s[\"error\"])\n");
    clean("s = car_status(1)\nprint(s[\"error\"] if \"error\" in s else 0)\n");
    clean("s = car_status(1)\nprint(\"x\" in s and s[\"y\"])\n");
    clean("s = car_status(1)\nh = s[\"fuelLevel\"] if \"fuelLevel\" in s else s[\"fuel\"]\nprint(h)\n");
    clean("s = car_status(1)\ntry:\n    print(s[\"error\"])\nexcept KeyError:\n    print(0)\n");
    clean("r = orders()\nif isinstance(r, dict):\n    print(r[\"id\"])\n");
    // A test on one value does not hide an unguarded lookup elsewhere.
    flags("s = car_status(1)\nif \"x\" in s:\n    print(1)\nprint(s[\"y\"])\n", &["L4:C7 car_status() has no key 'y'"]);
}

#[test]
fn rebound_or_changed_names_are_not_followed() {
    clean("s = car_status(1)\ns = {\"error\": 1}\nprint(s[\"error\"])\n");
    clean("s = car_status(1)\ns[\"error\"] = 1\nprint(s[\"error\"])\n");
    clean("s = car_status(1)\ns.update({\"error\": 1})\nprint(s[\"error\"])\n");
    clean("if len(\"ab\") > 1:\n    s = car_status(1)\nelse:\n    s = {\"z\": 1}\nprint(s[\"z\"])\n");
    clean("s = car_status(1)\nfor i in range(2):\n    print(s[\"zz\"])\n    s = {\"zz\": 1}\n");
    clean("s = car_status(1)\ntry:\n    s = {\"zz\": 1}\nexcept Exception:\n    pass\nprint(s[\"zz\"])\n");
    // The same tool on both branches stays known.
    flags("if len(\"ab\") > 1:\n    s = car_status(1)\nelse:\n    s = car_status(2)\nprint(s[\"zz\"])\n", &["has no key 'zz'"]);
}

#[test]
fn a_bad_lookup_stops_the_run_before_any_tool() {
    let calls = Arc::new(AtomicUsize::new(0));
    let e = engine_with(calls.clone());
    let err = e
        .run("fill_tank(10)\nfor o in orders():\n    print(o[\"title\"])\n")
        .expect_err("should be rejected");
    assert!(err.to_string().contains("has no key 'title'"), "{err}");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "a tool ran before the check");
    // Without the declaration it runs the write first.
    assert!(e.run("fill_tank(10)\nprint(1)\n").is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // Streaming sessions check each chunk the same way.
    let mut session = e.session();
    let err = session.push("o = orders()\nprint(o[\"x\"])\n").expect_err("session should refuse");
    assert!(err.to_string().contains("declared as a list"), "{err}");
}

// ---- random programs: a reported lookup must fail when it runs ------------

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
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

const KEYS: [&str; 6] = ["id", "name", "items", "price", "meta", "tags"];

/// A random schema, as a shape string, for an object or list.
fn gen_shape(rng: &mut Rng, depth: usize) -> String {
    if depth >= 2 || rng.chance(35) {
        return ["int", "str", "float", "bool"][rng.below(4) as usize].to_string();
    }
    if rng.chance(30) {
        return format!("list[{}]", gen_shape(rng, depth + 1));
    }
    let n = 1 + rng.below(3) as usize;
    let mut used = Vec::new();
    let mut fields = Vec::new();
    while fields.len() < n {
        let k = KEYS[rng.below(KEYS.len() as u64) as usize];
        if used.contains(&k) {
            continue;
        }
        used.push(k);
        let opt = if rng.chance(25) { "?" } else { "" };
        fields.push(format!("\"{k}\": {}{opt}", gen_shape(rng, depth + 1)));
    }
    let open = if rng.chance(25) { ", ..." } else { "" };
    format!("{{{}{open}}}", fields.join(", "))
}

/// A random value that matches `schema`. Lists are never empty, so a loop
/// body always runs.
fn gen_value(rng: &mut Rng, schema: &Json) -> Value {
    match schema.get("type").and_then(Json::as_str) {
        Some("integer") => Value::Int(rng.below(100) as i64),
        Some("number") => Value::Float(rng.below(100) as f64 / 4.0),
        Some("string") => Value::Str(format!("s{}", rng.below(10))),
        Some("boolean") => Value::Bool(rng.chance(50)),
        Some("array") => Value::List((0..1 + rng.below(3)).map(|_| gen_value(rng, &schema["items"])).collect()),
        Some("object") => {
            let required: Vec<&str> = schema["required"].as_array().unwrap().iter().filter_map(Json::as_str).collect();
            let mut out = Vec::new();
            for (k, sub) in schema["properties"].as_object().unwrap() {
                if required.contains(&k.as_str()) || rng.chance(50) {
                    out.push((Value::Str(k.clone()), gen_value(rng, sub)));
                }
            }
            if schema.get("additionalProperties").is_none() && rng.chance(50) {
                out.push((Value::Str("extra".into()), Value::Int(1)));
            }
            Value::Dict(out)
        }
        _ => Value::Null,
    }
}

#[test]
fn reported_lookups_fail_when_they_run() {
    let seeds: u64 = std::env::var("WEAVE_RETURNS_SEEDS").ok().and_then(|s| s.parse().ok()).unwrap_or(400);
    let (mut flagged, mut ran_ok, mut planted) = (0, 0, 0);
    for seed in 1..=seeds {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut typed = Engine::new();
        let mut plain = Engine::new();
        let mut schemas = Vec::new();
        for t in 0..3 {
            let text = gen_shape(&mut rng, 0);
            let schema = shape(&text);
            let value = gen_value(&mut rng, &schema);
            let v2 = value.clone();
            typed.register(&format!("t{t}"), ToolSpec::effect(Effect::Read).with_returns(schema.clone()), move |_, _| Ok(value.clone()));
            plain.register(&format!("t{t}"), ToolSpec::effect(Effect::Read), move |_, _| Ok(v2.clone()));
            schemas.push(schema);
        }
        // Straight-line code and loops over non-empty lists: every line runs
        // unless an earlier one fails.
        let mut src = String::new();
        let mut vars: Vec<(String, Json, String)> = Vec::new(); // name, schema, indent
        let mut indent = String::new();
        let mut certain = false;
        for i in 0..4 + rng.below(5) {
            let name = format!("v{i}");
            if vars.is_empty() || rng.chance(25) {
                let t = rng.below(3) as usize;
                src.push_str(&format!("{indent}{name} = t{t}()\n"));
                vars.push((name, schemas[t].clone(), indent.clone()));
                continue;
            }
            let (base, schema, _) = vars[rng.below(vars.len() as u64) as usize].clone();
            let kind = schema.get("type").and_then(Json::as_str).unwrap_or("");
            if kind == "array" && rng.chance(40) {
                src.push_str(&format!("{indent}for {name} in {base}:\n"));
                indent.push_str("    ");
                vars.push((name, schema["items"].clone(), indent.clone()));
                continue;
            }
            let props = schema.get("properties").and_then(Json::as_object);
            let (index, sub) = if kind == "array" && rng.chance(60) {
                ("0".to_string(), Some(schema["items"].clone()))
            } else if let (Some(p), true) = (props, rng.chance(70)) {
                match p.iter().nth(rng.below(p.len().max(1) as u64) as usize) {
                    Some((k, s)) => (format!("\"{k}\""), Some(s.clone())),
                    None => ("\"zz\"".to_string(), None),
                }
            } else {
                let k = if rng.chance(50) { "zz" } else { KEYS[rng.below(KEYS.len() as u64) as usize] };
                (format!("\"{k}\""), props.and_then(|p| p.get(k)).cloned())
            };
            let is_str_key = index.starts_with('"');
            let closed = schema.get("additionalProperties") == Some(&json!(false));
            if is_str_key && ((kind == "object" && closed && sub.is_none()) || (kind != "object" && !kind.is_empty())) {
                certain = true;
            }
            src.push_str(&format!("{indent}{name} = {base}[{index}]\n"));
            if let Some(s) = sub {
                vars.push((name, s, indent.clone()));
            }
        }
        src.push_str(&format!("{indent}print(1)\n"));
        let diags = typed.check(&src);
        if certain {
            planted += 1;
        }
        let run = plain.run(&src);
        if diags.is_empty() {
            assert!(!certain, "seed {seed}: a certain failure was not reported\n{src}");
            if run.is_ok() {
                ran_ok += 1;
            }
            continue;
        }
        flagged += 1;
        let err = run.expect_err(&format!("seed {seed}: reported but ran fine\n{src}\n{diags:?}"));
        let text = err.to_string();
        assert!(text.contains("is missing") || text.contains("cannot look up"), "seed {seed}: {text}\n{src}");
        // The first runtime failure is at or before the first reported lookup.
        let first = diags.iter().map(|d| (d.line, d.col)).min().unwrap();
        let at = err.diagnostics.iter().map(|d| (d.line, d.col)).min().unwrap();
        assert!(at <= first, "seed {seed}: failed at {at:?}, reported {first:?}\n{src}\n{text}");
        assert!(typed.run(&src).is_err());
    }
    assert!(flagged > seeds / 10 && ran_ok > seeds / 10 && planted > seeds / 10, "vacuous: {flagged} {ran_ok} {planted}");
    eprintln!("{seeds} programs: {flagged} reported, {planted} with a planted certain failure, {ran_ok} ran clean");
}
