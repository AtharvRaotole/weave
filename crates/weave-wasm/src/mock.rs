//! Mock tool host for the playground.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value as Json};
use weave::{vclock, Diagnostic, Effect, Engine, Footprint, Param, Params, ToolSpec, Value};

/// One mock tool as shown in the playground's tool list.
struct MockTool {
    name: &'static str,
    effect: Effect,
    /// (name, required, JSON Schema)
    params: &'static [(&'static str, bool, &'static str)],
    latency_ms: u64,
    reads: Option<&'static str>,
    writes: Option<&'static str>,
    /// Declared return type, in Weave's shape notation.
    returns: Option<&'static str>,
    about: &'static str,
}

const TOOLS: &[MockTool] = &[
    MockTool {
        name: "weather",
        effect: Effect::Read,
        params: &[("city", true, r#"{"type":"string"}"#)],
        latency_ms: 120,
        reads: None,
        writes: None,
        returns: Some(r#"{"city": str, "temp_c": int, "sky": str}"#),
        about: "Current weather for a city. Latency is 120 to 210 ms depending on the city name.",
    },
    MockTool {
        name: "search",
        effect: Effect::Read,
        params: &[("query", true, r#"{"type":"string"}"#), ("limit", false, r#"{"type":"integer"}"#)],
        latency_ms: 250,
        reads: None,
        writes: None,
        returns: Some(r#"list[{"title": str, "url": str}]"#),
        about: "Web search. Returns a list of {title, url} dicts (fake example.com results).",
    },
    MockTool {
        name: "gh_issues",
        effect: Effect::Read,
        params: &[
            ("repo", true, r#"{"type":"string"}"#),
            ("state", false, r#"{"type":"string","enum":["open","closed"]}"#),
        ],
        latency_ms: 180,
        reads: None,
        writes: None,
        returns: Some(r#"list[{"repo": str, "number": int, "title": str, "label": str, "state": str}]"#),
        about: "Issues of a GitHub repo (fake data). state is \"open\" or \"closed\".",
    },
    MockTool {
        name: "db_get",
        effect: Effect::Read,
        params: &[("key", true, r#"{"type":"string"}"#)],
        latency_ms: 60,
        reads: Some("db:{0|key}"),
        writes: None,
        returns: None,
        about: "Read a key from an in-memory store. Returns None when missing.",
    },
    MockTool {
        name: "db_put",
        effect: Effect::Reversible,
        params: &[("key", true, r#"{"type":"string"}"#), ("value", true, "")],
        latency_ms: 90,
        reads: None,
        writes: Some("db:{0|key}"),
        returns: Some("bool"),
        about: "Write a key. Reversible: runs once earlier conflicting work is done.",
    },
    MockTool {
        name: "summarize",
        effect: Effect::Pure,
        params: &[("text", true, r#"{"type":"string"}"#), ("max_words", false, r#"{"type":"integer"}"#)],
        latency_ms: 40,
        reads: None,
        writes: None,
        returns: Some("str"),
        about: "Keep the first max_words words (default 12). Pure.",
    },
    MockTool {
        name: "post_slack",
        effect: Effect::Irreversible,
        params: &[("channel", true, r#"{"type":"string"}"#), ("text", true, r#"{"type":"string"}"#)],
        latency_ms: 50,
        reads: None,
        writes: None,
        returns: Some(r#"{"ok": bool, "channel": str}"#),
        about: "Post a message (nothing is sent anywhere). Irreversible: runs in program order after earlier work.",
    },
    MockTool {
        name: "send_email",
        effect: Effect::Irreversible,
        params: &[
            ("to", true, r#"{"type":"string"}"#),
            ("subject", true, r#"{"type":"string"}"#),
            ("body", false, r#"{"type":"string"}"#),
        ],
        latency_ms: 70,
        reads: None,
        writes: None,
        returns: Some(r#"{"queued": bool, "to": str}"#),
        about: "Send an email (nothing is sent anywhere). Irreversible.",
    },
];

fn params_of(tool: &MockTool) -> Params {
    Params {
        params: tool
            .params
            .iter()
            .map(|(name, required, schema)| {
                let mut p = Param::new(*name, *required);
                if !schema.is_empty() {
                    p.schema = Some(serde_json::from_str(schema).expect("mock schema"));
                }
                p
            })
            .collect(),
        extra_positional: false,
        extra_keywords: false,
        root: None,
    }
}

fn latency(tool: &MockTool, args: &[Value]) -> u64 {
    if tool.name == "weather" {
        let city = args.first().and_then(Value::as_str).unwrap_or("");
        let spread: u64 = city.bytes().map(u64::from).sum::<u64>() % 10;
        return tool.latency_ms + spread * 10;
    }
    tool.latency_ms
}

#[derive(Clone)]
struct CallRec {
    tool: &'static str,
    effect: Effect,
    args: Vec<String>,
    kwargs: BTreeMap<String, String>,
    start: Duration,
    end: Duration,
    result: Result<String, String>,
}

type Calls = Arc<Mutex<Vec<CallRec>>>;
type Store = Arc<Mutex<BTreeMap<String, Value>>>;

fn arg<'a>(
    tool: &MockTool,
    args: &'a [Value],
    kwargs: &'a BTreeMap<String, Value>,
    name: &str,
) -> Option<&'a Value> {
    let index = tool.params.iter().position(|(n, _, _)| *n == name)?;
    args.get(index).or_else(|| kwargs.get(name))
}

fn dict(items: Vec<(&str, Value)>) -> Value {
    Value::Dict(items.into_iter().map(|(k, v)| (Value::str(k), v)).collect())
}

fn behave(
    tool: &MockTool,
    args: &[Value],
    kwargs: &BTreeMap<String, Value>,
    store: &Store,
) -> Result<Value, String> {
    let s = |name: &str| {
        arg(tool, args, kwargs, name)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    match tool.name {
        "weather" => {
            let city = s("city").unwrap_or_default();
            let (temp, sky) = match city.to_ascii_lowercase().as_str() {
                "paris" => (14, "cloudy"),
                "tokyo" => (19, "clear"),
                "london" => (11, "rain"),
                "new york" | "nyc" => (9, "windy"),
                "mumbai" => (31, "humid"),
                "pune" => (27, "clear"),
                "sydney" => (23, "sunny"),
                "berlin" => (8, "overcast"),
                _ => return Err(format!("no weather data for {city:?} (try Paris, Tokyo, London, New York, Mumbai, Pune, Sydney, Berlin)")),
            };
            Ok(dict(vec![
                ("city", Value::str(city)),
                ("temp_c", Value::int(temp)),
                ("sky", Value::str(sky)),
            ]))
        }
        "search" => {
            let query = s("query").unwrap_or_default();
            let limit = arg(tool, args, kwargs, "limit")
                .and_then(Value::as_int)
                .unwrap_or(3)
                .clamp(0, 5);
            let slug: String = query
                .to_ascii_lowercase()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect();
            Ok(Value::List(
                (1..=limit)
                    .map(|i| {
                        let title = match i {
                            1 => format!("{query}: a practical guide"),
                            2 => format!("Benchmarking {query} on real workloads"),
                            3 => format!("Why {query} matters more than you think"),
                            4 => format!("Common pitfalls with {query}"),
                            _ => format!("{query}, explained simply"),
                        };
                        dict(vec![
                            ("title", Value::str(title)),
                            ("url", Value::str(format!("https://example.com/{slug}/{i}"))),
                        ])
                    })
                    .collect(),
            ))
        }
        "gh_issues" => {
            let repo = s("repo").unwrap_or_default();
            let state = s("state").unwrap_or_else(|| "open".into());
            let titles: &[(i64, &str, &str)] = if state == "open" {
                &[
                    (41, "Add a timeout per tool", "enhancement"),
                    (46, "Docs: MCP setup on Windows", "docs"),
                    (52, "Crash on empty dict literal", "bug"),
                ]
            } else {
                &[
                    (12, "Typo in README", "docs"),
                    (30, "Support break in for loops", "enhancement"),
                ]
            };
            Ok(Value::List(
                titles
                    .iter()
                    .map(|(n, title, label)| {
                        dict(vec![
                            ("repo", Value::str(repo.clone())),
                            ("number", Value::int(*n)),
                            ("title", Value::str(*title)),
                            ("label", Value::str(*label)),
                            ("state", Value::str(state.clone())),
                        ])
                    })
                    .collect(),
            ))
        }
        "db_get" => {
            let key = s("key").unwrap_or_default();
            Ok(store
                .lock()
                .expect("store")
                .get(&key)
                .cloned()
                .unwrap_or(Value::Null))
        }
        "db_put" => {
            let key = s("key").unwrap_or_default();
            let value = arg(tool, args, kwargs, "value")
                .cloned()
                .unwrap_or(Value::Null);
            store.lock().expect("store").insert(key, value);
            Ok(Value::Bool(true))
        }
        "summarize" => {
            let text = s("text").unwrap_or_default();
            let max = arg(tool, args, kwargs, "max_words")
                .and_then(Value::as_int)
                .unwrap_or(12)
                .max(0) as usize;
            let words: Vec<&str> = text.split_whitespace().collect();
            let mut out = words
                .iter()
                .take(max)
                .copied()
                .collect::<Vec<_>>()
                .join(" ");
            if words.len() > max {
                out.push_str(" ...");
            }
            Ok(Value::str(out))
        }
        "post_slack" => Ok(dict(vec![
            ("ok", Value::Bool(true)),
            ("channel", Value::str(s("channel").unwrap_or_default())),
        ])),
        "send_email" => Ok(dict(vec![
            ("queued", Value::Bool(true)),
            ("to", Value::str(s("to").unwrap_or_default())),
        ])),
        _ => Err("unknown mock tool".into()),
    }
}

fn engine(calls: &Calls, store: &Store) -> Engine {
    let mut engine = Engine::new();
    for tool in TOOLS {
        let mut spec = ToolSpec::effect(tool.effect).with_params(params_of(tool));
        if let Some(r) = tool.reads {
            spec = spec.with_reads(Footprint::only([r]));
        }
        if let Some(w) = tool.writes {
            spec = spec.with_writes(Footprint::only([w]));
        }
        if let Some(r) = tool.returns {
            spec = spec.with_returns(weave::schema_from_shape(r).expect("mock return shape"));
        }
        let (calls, store) = (calls.clone(), store.clone());
        engine.register_async(tool.name, spec, move |args, kwargs, _token| {
            let (calls, store) = (calls.clone(), store.clone());
            async move {
                let start = vclock::now();
                vclock::sleep(Duration::from_millis(latency(tool, &args))).await;
                let result = behave(tool, &args, &kwargs, &store);
                calls.lock().expect("calls").push(CallRec {
                    tool: tool.name,
                    effect: tool.effect,
                    args: args.iter().map(Value::to_string).collect(),
                    kwargs: kwargs
                        .iter()
                        .map(|(k, v)| (k.clone(), v.to_string()))
                        .collect(),
                    start,
                    end: vclock::now(),
                    result: result.as_ref().map(Value::to_string).map_err(Clone::clone),
                });
                result
            }
        });
    }
    engine
}

fn diag_json(d: &Diagnostic) -> Json {
    json!({
        "line": d.line,
        "col": d.col,
        "message": d.message,
        "hint": d.hint,
        "class": format!("{:?}", d.class).to_lowercase(),
        "text": d.to_string(),
    })
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// The mock tool list.
pub fn tools_json() -> String {
    let tools: Vec<Json> = TOOLS
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "effect": t.effect.as_str(),
                "params": t.params.iter().map(|(n, r, _)| json!({"name": n, "required": r})).collect::<Vec<_>>(),
                "latency_ms": t.latency_ms,
                "reads": t.reads,
                "writes": t.writes,
                "returns": t.returns,
                "about": t.about,
            })
        })
        .collect();
    Json::Array(tools).to_string()
}

/// Pre-run check only.
pub fn check_json(source: &str) -> String {
    let calls: Calls = Arc::default();
    let store: Store = Arc::default();
    let engine = engine(&calls, &store);
    let diags = engine.check(source);
    // When the check passes, the static plan: every call and write the
    // script can make, before anything runs.
    let plan = if diags.is_empty() {
        engine.plan(source).ok().map(|p| {
            let mut v: Json = serde_json::from_str(&p.to_json()).expect("plan json");
            v["text"] = json!(p.render());
            v
        })
    } else {
        None
    };
    json!({ "ok": diags.is_empty(), "diagnostics": diags.iter().map(diag_json).collect::<Vec<_>>(), "plan": plan }).to_string()
}

/// Check, then run when the check passes.
pub fn run_json(source: &str) -> String {
    let calls: Calls = Arc::default();
    let store: Store = Arc::new(Mutex::new(seed()));
    let engine = engine(&calls, &store);
    let diags = engine.check(source);
    if !diags.is_empty() {
        return json!({
            "ok": false,
            "stage": "check",
            "diagnostics": diags.iter().map(diag_json).collect::<Vec<_>>(),
            "calls": [],
        })
        .to_string();
    }
    let outcome = engine.run(source);
    let total = vclock::now();
    let mut recs = calls.lock().expect("calls").clone();
    recs.sort_by_key(|c| (c.start, c.end));
    let sequential: Duration = recs.iter().map(|c| c.end - c.start).sum();
    let calls_json: Vec<Json> = recs
        .iter()
        .enumerate()
        .map(|(i, c)| {
            json!({
                "id": i,
                "tool": c.tool,
                "effect": c.effect.as_str(),
                "args": c.args,
                "kwargs": c.kwargs,
                "start_ms": ms(c.start),
                "end_ms": ms(c.end),
                "ok": c.result.is_ok(),
                "result": c.result.as_ref().ok(),
                "error": c.result.as_ref().err(),
            })
        })
        .collect();
    let mut out = json!({
        "stage": "run",
        "calls": calls_json,
        "total_ms": ms(total),
        "sequential_ms": ms(sequential),
        "diagnostics": [],
    });
    match outcome {
        Ok(result) => {
            out["ok"] = json!(true);
            out["stdout"] = json!(result.stdout());
            out["bindings"] = Json::Object(
                result
                    .bindings
                    .iter()
                    .filter(|(k, _)| !k.starts_with('_'))
                    .map(|(k, v)| (k.clone(), json!(v.to_string())))
                    .collect(),
            );
        }
        Err(err) => {
            out["ok"] = json!(false);
            out["diagnostics"] = Json::Array(err.diagnostics.iter().map(diag_json).collect());
        }
    }
    out.to_string()
}

fn seed() -> BTreeMap<String, Value> {
    let mut m = BTreeMap::new();
    m.insert("visits".to_string(), Value::int(41));
    m.insert("owner".to_string(), Value::str("ada"));
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> Json {
        serde_json::from_str(&run_json(src)).unwrap()
    }

    #[test]
    fn fan_out_runs_in_parallel() {
        let out = run("cities = [\"Paris\", \"Tokyo\", \"London\"]\nfor c in cities:\n    w = weather(c)\n    print(c, w[\"temp_c\"])\n");
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["stdout"], "Paris 14\nTokyo 19\nLondon 11\n");
        let calls = out["calls"].as_array().unwrap();
        assert_eq!(calls.len(), 3);
        assert!(calls.iter().all(|c| c["start_ms"] == 0.0), "{out}");
        assert!(out["total_ms"].as_f64().unwrap() < out["sequential_ms"].as_f64().unwrap());
    }

    #[test]
    fn unknown_tool_gets_a_suggestion() {
        let out = run("w = wether(\"Paris\")\nprint(w)\n");
        assert_eq!(out["ok"], false);
        assert_eq!(out["stage"], "check");
        let d = &out["diagnostics"][0];
        assert_eq!(d["line"], 1);
        assert!(
            d["hint"]
                .as_str()
                .unwrap()
                .contains("did you mean `weather`"),
            "{out}"
        );
    }

    #[test]
    fn bad_keyword_gets_a_suggestion() {
        let out = run("r = search(\"rust wasm\", limt=2)\nprint(r)\n");
        assert_eq!(out["stage"], "check");
        let text = out["diagnostics"].to_string();
        assert!(text.contains("did you mean `limit`"), "{text}");
    }

    #[test]
    fn bad_enum_literal_is_caught() {
        let out = run("i = gh_issues(\"a/b\", state=\"opened\")\nprint(i)\n");
        assert_eq!(out["stage"], "check", "{out}");
    }

    #[test]
    fn missing_required_argument_is_caught() {
        let out = run("send_email(\"a@example.com\")\n");
        assert_eq!(out["stage"], "check", "{out}");
    }

    #[test]
    fn writes_are_ordered_and_reads_see_them() {
        let out = run("v = db_get(\"visits\")\ndb_put(\"visits\", v + 1)\nafter = db_get(\"visits\")\nprint(after)\npost_slack(\"#ops\", f\"visits is now {after}\")\n");
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["stdout"], "42\n");
        let calls = out["calls"].as_array().unwrap();
        let put = calls.iter().find(|c| c["tool"] == "db_put").unwrap();
        let post = calls.iter().find(|c| c["tool"] == "post_slack").unwrap();
        assert!(post["start_ms"].as_f64() >= put["end_ms"].as_f64(), "{out}");
    }

    #[test]
    fn check_includes_the_plan_when_it_passes() {
        let out: Json = serde_json::from_str(&check_json(
            "visits = db_get(\"visits\")\ndb_put(\"visits\", visits + 1)\nfor to in [\"a@x.io\", \"b@x.io\"]:\n    send_email(to, subject=\"hi\", body=\"n\")\n",
        ))
        .unwrap();
        assert_eq!(out["ok"], true, "{out}");
        let plan = &out["plan"];
        assert_eq!(plan["may_write"], true);
        assert_eq!(plan["calls"][1]["tool"], "db_put");
        assert_eq!(plan["calls"][1]["writes"]["names"][0], "db:visits", "{plan}");
        assert_eq!(plan["calls"][2]["min_count"], 2);
        assert!(plan["text"].as_str().unwrap().starts_with("plan: 4 tool calls"), "{plan}");
        let bad: Json = serde_json::from_str(&check_json("nope(1)\n")).unwrap();
        assert!(bad["plan"].is_null());
    }

    #[test]
    fn declared_results_are_checked() {
        let out: Json = serde_json::from_str(&check_json("t = weather(\"Tokyo\")\nprint(t[\"temp\"])\n")).unwrap();
        assert_eq!(out["ok"], false, "{out}");
        let d = &out["diagnostics"][0];
        assert_eq!((d["line"].as_u64(), d["col"].as_u64()), (Some(2), Some(7)), "{out}");
        assert!(d["message"].as_str().unwrap().contains("weather() has no key 'temp'"), "{out}");
        assert_eq!(d["hint"], "did you mean 'temp_c'?");
        let tools: Json = serde_json::from_str(&tools_json()).unwrap();
        assert_eq!(tools[0]["returns"], r#"{"city": str, "temp_c": int, "sky": str}"#);
    }

    #[test]
    fn tool_error_is_catchable() {
        let out = run(
            "try:\n    w = weather(\"Atlantis\")\nexcept Exception as e:\n    w = None\nprint(w)\n",
        );
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["stdout"], "None\n");
    }

    #[test]
    fn runtime_error_keeps_partial_calls() {
        let out = run("w = weather(\"Atlantis\")\nprint(w)\n");
        assert_eq!(out["ok"], false);
        assert_eq!(out["stage"], "run");
        assert!(
            out["diagnostics"][0]["message"]
                .as_str()
                .unwrap()
                .contains("Atlantis"),
            "{out}"
        );
    }

    #[test]
    fn tools_json_lists_every_tool() {
        let tools: Json = serde_json::from_str(&tools_json()).unwrap();
        assert_eq!(tools.as_array().unwrap().len(), TOOLS.len());
    }
}

#[cfg(test)]
mod preset_tests {
    use serde_json::Value as Json;

    /// Every preset shipped with the playground behaves as its manifest says.
    #[test]
    fn playground_presets_behave_as_described() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../playground/presets");
        let manifest: Json =
            serde_json::from_str(&std::fs::read_to_string(format!("{dir}/manifest.json")).unwrap())
                .unwrap();
        let presets = manifest.as_array().unwrap();
        assert!((4..=8).contains(&presets.len()));
        for preset in presets {
            let id = preset["id"].as_str().unwrap();
            let src = std::fs::read_to_string(format!("{dir}/{id}.weave")).unwrap();
            let out: Json = serde_json::from_str(&super::run_json(&src)).unwrap();
            match preset["expect"].as_str().unwrap() {
                "ok" => {
                    assert_eq!(out["ok"], true, "{id}: {out}");
                    assert!(!out["calls"].as_array().unwrap().is_empty(), "{id}");
                    assert!(
                        out["total_ms"].as_f64().unwrap() <= out["sequential_ms"].as_f64().unwrap(),
                        "{id}"
                    );
                }
                "check" => {
                    assert_eq!(out["stage"], "check", "{id}: {out}");
                    assert!(out["diagnostics"].as_array().unwrap().len() >= 3, "{id}");
                    assert!(
                        out["diagnostics"].to_string().contains("did you mean"),
                        "{id}"
                    );
                }
                other => panic!("unknown expectation {other}"),
            }
        }
    }

    #[test]
    fn same_script_gives_same_schedule() {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../playground/presets/writes.weave"
        ))
        .unwrap();
        let a = super::run_json(&src);
        let b = super::run_json(&src);
        assert_eq!(a, b);
    }

    #[test]
    fn hostile_input_does_not_hang_or_crash() {
        for src in [
            "while True:\n    pass\n",
            "x = 1\nfor i in range(10000000):\n    x = x + 1\n",
            "x = \"a\" * 100000000000\n",
            &"(".repeat(5000),
            &format!("x = {}1{}\n", "[".repeat(300), "]".repeat(300)),
            "import os\n",
            "def f():\n    return 1\n",
            "",
            "print(\n",
        ] {
            let out: Json = serde_json::from_str(&super::run_json(src)).unwrap();
            assert!(out["ok"].is_boolean(), "{src:.40}: {out}");
        }
    }
}
