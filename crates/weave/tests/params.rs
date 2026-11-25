//! Declared parameters are checked before any tool runs.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::json;
use weave::{Effect, Engine, Param, Params, ToolSpec, Value};

/// `search(query, size=None, sort=None)` and an irreversible `save(key, text)`.
/// Property names are alphabetical so positional order is the same with or
/// without serde_json's `preserve_order`.
fn engine(calls: Arc<AtomicUsize>) -> Engine {
    let mut engine = Engine::new();
    let search = Params::from_json_schema(&json!({
        "type": "object",
        "properties": {
            "query": {"type": "string"},
            "size": {"type": "integer"},
            "sort": {"enum": ["new", "top"]}
        },
        "required": ["query"]
    }))
    .unwrap();
    let counter = calls.clone();
    engine.register("search", ToolSpec::effect(Effect::Read).with_params(search), move |args, kwargs| {
        counter.fetch_add(1, Ordering::SeqCst);
        let q = args.first().or_else(|| kwargs.get("query")).cloned().unwrap_or(Value::Null);
        Ok(Value::List(vec![q]))
    });
    let save = Params::from_json_schema(&json!({
        "properties": {"key": {"type": "string"}, "text": {"type": "string"}},
        "required": ["key", "text"]
    }))
    .unwrap();
    engine.register("save", ToolSpec::effect(Effect::Irreversible).with_params(save), move |_args, _kwargs| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(Value::Null)
    });
    engine
}

fn check(source: &str) -> Vec<String> {
    engine(Arc::new(AtomicUsize::new(0))).check(source).iter().map(ToString::to_string).collect()
}

#[test]
fn good_calls_pass() {
    assert!(check(r#"
r = search("weave", size=5, sort="top")
xs = search(query="a")
n = 3
ys = search("b", n)
save("k", "v")
save(key="k", text=str(len(r)))
"#)
    .is_empty());
}

#[test]
fn misspelled_keyword_points_at_the_argument() {
    assert_eq!(
        check("r = search(\"weave\", szie=5)"),
        ["L1:C26: tool `search` has no parameter `szie` hint: did you mean `size`?"]
    );
}

#[test]
fn missing_required_argument() {
    assert_eq!(
        check("save(\"k\")"),
        ["L1:C1: tool `save` is missing required argument(s): text hint: pass them, like `save(text=...)`"]
    );
}

#[test]
fn too_many_positional_arguments() {
    let diags = check("r = search(\"a\", 1, \"new\", 4)");
    assert_eq!(diags.len(), 1);
    assert!(diags[0].starts_with("L1:C27: tool `search` takes at most 3 positional argument(s) (query, size, sort), got 4"), "{diags:?}");
}

#[test]
fn literal_type_and_enum_mismatches() {
    assert_eq!(
        check("r = search(42)\ns = search(\"x\", sort=\"old\")\nt = search(\"x\", size=\"5\")\nu = search(\"x\", size=-2)\nv = search(\"x\", size=2.0)"),
        [
            "L1:C12: argument `query` of tool `search` should be str, got int 42",
            "L2:C22: argument `sort` of tool `search` must be one of 'new', 'top', got str 'old'",
            "L3:C22: argument `size` of tool `search` should be int, got str '5'",
        ]
    );
}

#[test]
fn values_known_only_at_run_time_are_not_guessed() {
    // `n` could be anything at check time; the check stays quiet.
    assert!(check("n = \"5\"\nr = search(\"x\", size=n)\ns = search(str(1))").is_empty());
}

#[test]
fn optional_none_is_allowed() {
    assert!(check("r = search(\"x\", size=None, sort=None)").is_empty());
}

#[test]
fn problems_in_loops_helpers_and_comprehensions_are_found() {
    let diags = check(r#"
def find(q):
    return search(q, limt=3)
x = find("a")
for c in ["a", "b"]:
    save(c)
rs = [search(q, srot="new") for q in ["x"]]
"#);
    assert_eq!(diags.len(), 3, "{diags:?}");
    assert!(diags.iter().any(|d| d.contains("tool `search` has no parameter `limt` hint: its keyword parameters are: query, size, sort")));
    assert!(diags.iter().any(|d| d.contains("tool `save` is missing required argument(s): text")));
    assert!(diags.iter().any(|d| d.contains("has no parameter `srot` hint: did you mean `sort`?")));
}

#[test]
fn a_bad_call_stops_the_run_before_any_tool_runs() {
    let calls = Arc::new(AtomicUsize::new(0));
    let engine = engine(calls.clone());
    let err = engine.run("save(\"a\", \"first\")\nr = search(\"x\", szie=1)").unwrap_err();
    assert!(err.to_string().contains("has no parameter `szie`"), "{err}");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no tool may run when the check fails");
    let ok = engine.run("r = search(\"x\", size=1)\nprint(r)").unwrap();
    assert_eq!(ok.stdout(), "['x']\n");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn arity_and_params_do_not_report_the_same_problem_twice() {
    let mut engine = Engine::new();
    let params = Params { params: vec![Param::new("a", true)], ..Params::default() };
    engine.register("f", ToolSpec::effect(Effect::Read).with_arity(1).with_params(params), |_, _| Ok(Value::Null));
    let diags: Vec<String> = engine.check("x = f()").iter().map(ToString::to_string).collect();
    assert_eq!(diags, ["L1:C5: tool `f` takes 1 argument(s), got 0 hint: pass 1 positional argument(s) to `f`"]);
    assert!(engine.check("x = f(a=1)").is_empty());
}

#[test]
fn tools_without_params_keep_the_old_rules() {
    let mut engine = Engine::new();
    engine.register("g", ToolSpec::effect(Effect::Read), |_, _| Ok(Value::Null));
    let diags: Vec<String> = engine.check("x = g(1, anything=2)").iter().map(ToString::to_string).collect();
    assert_eq!(diags, ["L1:C5: tool `g` does not take keyword arguments"]);
    let mut open = ToolSpec::effect(Effect::Read);
    open.accepts_kwargs = true;
    engine.register("h", open, |_, _| Ok(Value::Null));
    assert!(engine.check("x = h(1, 2, anything=3)").is_empty());
}

#[test]
fn nested_literals_are_checked_against_the_schema() {
    let mut engine = Engine::new();
    let params = Params::from_json_schema(&json!({
        "properties": {"entities": {"type": "array", "items": {"type": "object",
            "properties": {"name": {"type": "string"}}, "required": ["name"]}}},
        "required": ["entities"]
    }))
    .unwrap();
    engine.register("create_entities", ToolSpec::effect(Effect::Reversible).with_params(params), |_, _| Ok(Value::Null));
    let diags: Vec<String> = engine
        .check("create_entities([{\"name\": \"a\"}, {\"nme\": \"b\"}])")
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(diags, ["L1:C17: argument `entities` of tool `create_entities`: item `entities[1]` is missing key(s): name"]);
    assert!(engine.check("create_entities([{\"name\": \"a\"}])").is_empty());
}

