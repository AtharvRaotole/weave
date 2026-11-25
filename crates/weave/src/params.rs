//! Declared tool parameters, checked before a script runs.
//!
//! A tool can describe its parameters: their names, which are required,
//! whether each can be passed by position or by keyword, and optionally a
//! JSON Schema per parameter. The static check then catches a misspelled
//! keyword, a missing required argument, too many positional arguments and
//! literal arguments whose type does not match the schema, before any tool
//! runs. [`Params::validate_call`] applies the same rules to real values at
//! call time, which a host can use at its tool boundary.
//!
//! Only a subset of JSON Schema is checked: `type`, `enum`, `const`,
//! `anyOf` / `oneOf` (any branch may match), `allOf`, `items` (a single
//! schema), `properties`, `required`, `additionalProperties: false`, and
//! local `$ref`s (`#/...`). Every other keyword is ignored, so an unknown
//! constraint never rejects a call. `None` is always accepted for an
//! optional argument or field.

use std::collections::BTreeMap;

use serde_json::Value as Json;

use crate::value::Value;

/// One declared parameter.
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    pub required: bool,
    /// Can be filled by a positional argument.
    pub positional: bool,
    /// Can be passed as `name=value`.
    pub keyword: bool,
    /// JSON Schema for the value. `None` means any value.
    pub schema: Option<Json>,
}

impl Param {
    /// A parameter that can be passed by position or keyword.
    pub fn new(name: impl Into<String>, required: bool) -> Self {
        Self { name: name.into(), required, positional: true, keyword: true, schema: None }
    }
}

/// A tool's parameter list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Params {
    /// In positional order.
    pub params: Vec<Param>,
    /// Extra positional arguments are accepted (Python `*args`).
    pub extra_positional: bool,
    /// Keywords not in `params` are accepted (Python `**kwargs`, or a JSON
    /// Schema with `additionalProperties`).
    pub extra_keywords: bool,
    /// The whole schema the parameters came from, used to resolve `$ref`.
    pub root: Option<Json>,
}

/// One problem with a call's arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArgProblem {
    pub message: String,
    pub hint: Option<String>,
    /// Index into the call's positional arguments, or the keyword name, that
    /// the problem is about, when there is one.
    pub at: Option<ArgRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgRef {
    Positional(usize),
    Keyword(String),
}

impl Params {
    /// Parameters from an object JSON Schema, such as an MCP tool's
    /// `inputSchema`. Properties become parameters in the order the parsed
    /// map keeps them (schema order with serde_json's `preserve_order`, which
    /// `weave-mcp` turns on), each
    /// passable by position or keyword. Returns `None` when the schema has no
    /// `properties` object, since then nothing is known about the arguments.
    ///
    /// Keywords outside `properties` are rejected unless the schema sets
    /// `additionalProperties` to `true` or a schema.
    pub fn from_json_schema(schema: &Json) -> Option<Self> {
        let props = schema.get("properties")?.as_object()?;
        let required: Vec<&str> = schema
            .get("required")
            .and_then(Json::as_array)
            .map(|items| items.iter().filter_map(Json::as_str).collect())
            .unwrap_or_default();
        let params = props
            .iter()
            .map(|(name, prop)| Param {
                name: name.clone(),
                required: required.contains(&name.as_str()),
                positional: true,
                keyword: true,
                schema: Some(prop.clone()),
            })
            .collect();
        let extra_keywords = match schema.get("additionalProperties") {
            Some(Json::Bool(flag)) => *flag,
            Some(Json::Object(_)) => true,
            _ => false,
        };
        Some(Self { params, extra_positional: false, extra_keywords, root: Some(schema.clone()) })
    }

    fn get(&self, name: &str) -> Option<&Param> {
        self.params.iter().find(|p| p.name == name)
    }

    fn positional_slots(&self) -> Vec<&Param> {
        self.params.iter().filter(|p| p.positional).collect()
    }

    /// Bind a call's arguments to parameters. `args` and `kwargs` hold
    /// `Some(value)` when the value is known (a literal at check time, every
    /// value at run time) and `None` otherwise. Returns every problem found.
    pub fn problems(&self, tool: &str, args: &[Option<Value>], kwargs: &[(String, Option<Value>)]) -> Vec<ArgProblem> {
        let mut out = Vec::new();
        let slots = self.positional_slots();
        let mut filled: Vec<&str> = Vec::new();
        if args.len() > slots.len() && !self.extra_positional {
            let names: Vec<&str> = slots.iter().map(|p| p.name.as_str()).collect();
            let message = if names.is_empty() {
                format!("tool `{tool}` takes no positional arguments, got {}", args.len())
            } else {
                format!(
                    "tool `{tool}` takes at most {} positional argument(s) ({}), got {}",
                    names.len(),
                    names.join(", "),
                    args.len()
                )
            };
            // Point at a keyword the call still needs, else any keyword.
            let keyword = self
                .params
                .iter()
                .find(|p| p.keyword && !p.positional && p.required)
                .or_else(|| self.params.iter().find(|p| p.keyword && !p.positional))
                .or_else(|| self.params.iter().find(|p| p.keyword));
            let lead = if slots.is_empty() { "pass arguments by name" } else { "pass the rest by name" };
            let hint = keyword.map(|p| format!("{lead}, like `{tool}({}=...)`", p.name));
            out.push(ArgProblem { message, hint, at: Some(ArgRef::Positional(slots.len())) });
        }
        for (index, (param, value)) in slots.iter().zip(args).enumerate() {
            filled.push(&param.name);
            if let Some(value) = value {
                if let Some(problem) = self.value_problem(tool, param, value) {
                    out.push(ArgProblem { at: Some(ArgRef::Positional(index)), ..problem });
                }
            }
        }
        for (name, value) in kwargs {
            let at = Some(ArgRef::Keyword(name.clone()));
            match self.get(name) {
                None if self.extra_keywords => {}
                None => {
                    let names: Vec<&str> = self.params.iter().filter(|p| p.keyword).map(|p| p.name.as_str()).collect();
                    let hint = match closest(name, &names) {
                        Some(best) => format!("did you mean `{best}`?"),
                        None if names.is_empty() => format!("`{tool}` takes no keyword arguments"),
                        None => format!("its keyword parameters are: {}", names.join(", ")),
                    };
                    out.push(ArgProblem {
                        message: format!("tool `{tool}` has no parameter `{name}`"),
                        hint: Some(hint),
                        at,
                    });
                }
                Some(param) if !param.keyword => out.push(ArgProblem {
                    message: format!("parameter `{name}` of tool `{tool}` is positional-only"),
                    hint: Some(format!("pass `{name}` by position")),
                    at,
                }),
                Some(param) if filled.contains(&param.name.as_str()) => out.push(ArgProblem {
                    message: format!("tool `{tool}` got `{name}` twice, by position and by name"),
                    hint: None,
                    at,
                }),
                Some(param) => {
                    filled.push(&param.name);
                    if let Some(value) = value {
                        if let Some(problem) = self.value_problem(tool, param, value) {
                            out.push(ArgProblem { at, ..problem });
                        }
                    }
                }
            }
        }
        let missing: Vec<&str> = self
            .params
            .iter()
            .filter(|p| p.required && !filled.contains(&p.name.as_str()))
            .map(|p| p.name.as_str())
            .collect();
        if !missing.is_empty() {
            let example = missing.iter().map(|m| format!("{m}=...")).collect::<Vec<_>>().join(", ");
            out.push(ArgProblem {
                message: format!("tool `{tool}` is missing required argument(s): {}", missing.join(", ")),
                hint: Some(format!("pass them, like `{tool}({example})`")),
                at: None,
            });
        }
        out
    }

    /// Check real argument values at call time. `Err` holds every problem,
    /// one per line.
    pub fn validate_call(&self, tool: &str, args: &[Value], kwargs: &BTreeMap<String, Value>) -> Result<(), String> {
        let args: Vec<Option<Value>> = args.iter().cloned().map(Some).collect();
        let kwargs: Vec<(String, Option<Value>)> = kwargs.iter().map(|(k, v)| (k.clone(), Some(v.clone()))).collect();
        let problems = self.problems(tool, &args, &kwargs);
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.into_iter().map(|p| p.message).collect::<Vec<_>>().join("\n"))
        }
    }

    fn value_problem(&self, tool: &str, param: &Param, value: &Value) -> Option<ArgProblem> {
        let schema = param.schema.as_ref()?;
        if *value == Value::Null && !param.required {
            return None;
        }
        let checker = Checker { root: self.root.as_ref() };
        let found = checker.mismatch(value, schema, &param.name, 0)?;
        let sep = if found.starts_with("item ") { ": " } else { " " };
        Some(ArgProblem {
            message: format!("argument `{}` of tool `{tool}`{sep}{found}", param.name),
            hint: None,
            at: None,
        })
    }
}

fn closest<'a>(name: &str, names: &[&'a str]) -> Option<&'a str> {
    let mut best: Option<(usize, &str)> = None;
    for cand in names {
        let dist = typo_distance(&name.to_lowercase(), &cand.to_lowercase());
        let limit = if name.chars().count() <= 3 { 1 } else { 2 };
        if dist <= limit && best.map(|(d, _)| dist < d).unwrap_or(true) {
            best = Some((dist, cand));
        }
    }
    best.map(|(_, name)| name)
}

/// Edit distance where swapping two neighbouring letters counts as one edit
/// (optimal string alignment), so `qyt` is one edit from `qty`.
fn typo_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len() > 64 || b.len() > 64 {
        return usize::MAX;
    }
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}

const MAX_SCHEMA_DEPTH: usize = 32;

struct Checker<'a> {
    root: Option<&'a Json>,
}

impl Checker<'_> {
    /// `Some(description)` when `value` certainly does not match `schema`.
    fn mismatch(&self, value: &Value, schema: &Json, path: &str, depth: usize) -> Option<String> {
        if depth > MAX_SCHEMA_DEPTH {
            return None;
        }
        let Json::Object(obj) = schema else { return None };
        if let Some(reference) = obj.get("$ref").and_then(Json::as_str) {
            if let Some(target) = self.resolve(reference) {
                if let Some(found) = self.mismatch(value, target, path, depth + 1) {
                    return Some(found);
                }
            }
        }
        for key in ["anyOf", "oneOf"] {
            if let Some(options) = obj.get(key).and_then(Json::as_array) {
                if !options.is_empty() && options.iter().all(|o| self.mismatch(value, o, path, depth + 1).is_some()) {
                    return Some(format!("{}matches none of the allowed types ({}), got {}", here(path), describe_options(options, self), describe(value)));
                }
            }
        }
        if let Some(parts) = obj.get("allOf").and_then(Json::as_array) {
            for part in parts {
                if let Some(found) = self.mismatch(value, part, path, depth + 1) {
                    return Some(found);
                }
            }
        }
        if let Some(allowed) = obj.get("enum").and_then(Json::as_array) {
            if !allowed.iter().any(|a| json_equals(value, a)) {
                let shown: Vec<String> = allowed.iter().take(8).map(json_repr).collect();
                let more = if allowed.len() > 8 { ", ..." } else { "" };
                return Some(format!("{}must be one of {}{more}, got {}", here(path), shown.join(", "), describe(value)));
            }
        }
        if let Some(constant) = obj.get("const") {
            if !json_equals(value, constant) {
                return Some(format!("{}must be {}, got {}", here(path), json_repr(constant), describe(value)));
            }
        }
        if let Some(types) = obj.get("type") {
            let names: Vec<&str> = match types {
                Json::String(name) => vec![name.as_str()],
                Json::Array(items) => items.iter().filter_map(Json::as_str).collect(),
                _ => Vec::new(),
            };
            let known = names.iter().all(|n| KNOWN_TYPES.contains(n));
            if !names.is_empty() && known && !names.iter().any(|n| has_type(value, n)) {
                return Some(format!("{}should be {}, got {}", here(path), names.iter().map(|n| python_type(n)).collect::<Vec<_>>().join(" or "), describe(value)));
            }
        }
        match value {
            Value::List(items) => {
                if let Some(item_schema) = obj.get("items").filter(|s| s.is_object()) {
                    for (i, item) in items.iter().enumerate() {
                        if let Some(found) = self.mismatch(item, item_schema, &format!("{path}[{i}]"), depth + 1) {
                            return Some(found);
                        }
                    }
                }
            }
            Value::Dict(pairs) => {
                let props = obj.get("properties").and_then(Json::as_object);
                let required: Vec<&str> = obj
                    .get("required")
                    .and_then(Json::as_array)
                    .map(|items| items.iter().filter_map(Json::as_str).collect())
                    .unwrap_or_default();
                let closed = matches!(obj.get("additionalProperties"), Some(Json::Bool(false)));
                for (key, item) in pairs {
                    let Value::Str(key) = key else { continue };
                    let sub = format!("{path}[{}]", crate::value::py_repr(&Value::Str(key.clone())));
                    match props.and_then(|p| p.get(key)) {
                        Some(prop) => {
                            if *item == Value::Null && !required.contains(&key.as_str()) {
                                continue;
                            }
                            if let Some(found) = self.mismatch(item, prop, &sub, depth + 1) {
                                return Some(found);
                            }
                        }
                        None if closed => {
                            let names: Vec<&str> = props.map(|p| p.keys().map(String::as_str).collect()).unwrap_or_default();
                            return Some(format!("{}has no key {} (allowed: {})", here(path), crate::value::py_repr(&Value::Str(key.clone())), names.join(", ")));
                        }
                        None => {}
                    }
                }
                let missing: Vec<&str> = required
                    .iter()
                    .copied()
                    .filter(|r| !pairs.iter().any(|(k, _)| matches!(k, Value::Str(s) if s == r)))
                    .collect();
                if !missing.is_empty() {
                    return Some(format!("{}is missing key(s): {}", here(path), missing.join(", ")));
                }
            }
            _ => {}
        }
        None
    }

    fn resolve(&self, reference: &str) -> Option<&Json> {
        let pointer = reference.strip_prefix('#')?;
        self.root?.pointer(pointer)
    }
}

const KNOWN_TYPES: [&str; 7] = ["string", "integer", "number", "boolean", "null", "array", "object"];

fn has_type(value: &Value, name: &str) -> bool {
    match (name, value) {
        ("string", Value::Str(_)) => true,
        ("integer", Value::Int(_)) => true,
        // JSON has one number type: 2.0 is a valid integer.
        ("integer", Value::Float(f)) => f.is_finite() && f.fract() == 0.0,
        ("number", Value::Int(_) | Value::Float(_)) => true,
        ("boolean", Value::Bool(_)) => true,
        ("null", Value::Null) => true,
        ("array", Value::List(_)) => true,
        ("object", Value::Dict(_)) => true,
        _ => false,
    }
}

fn python_type(name: &str) -> &str {
    match name {
        "string" => "str",
        "integer" => "int",
        "number" => "a number",
        "boolean" => "bool",
        "null" => "None",
        "array" => "a list",
        "object" => "a dict",
        other => other,
    }
}

fn describe(value: &Value) -> String {
    let kind = match value {
        Value::Null => return "None".into(),
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Str(_) => "str",
        Value::List(_) => "list",
        Value::Dict(_) => "dict",
    };
    let shown = crate::value::py_repr(value);
    if shown.chars().count() <= 40 {
        format!("{kind} {shown}")
    } else {
        kind.to_string()
    }
}

fn describe_options(options: &[Json], checker: &Checker) -> String {
    let mut names = Vec::new();
    for option in options {
        let option = option
            .get("$ref")
            .and_then(Json::as_str)
            .and_then(|r| checker.resolve(r))
            .unwrap_or(option);
        let name = match option.get("type") {
            Some(Json::String(t)) => python_type(t).to_string(),
            _ if option.get("enum").is_some() || option.get("const").is_some() => "a fixed value".into(),
            _ => "a schema".into(),
        };
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names.join(", ")
}

/// The subject of a message about the value at `path`: empty for the
/// argument itself, `item `path` ` for something inside it.
fn here(path: &str) -> String {
    if path.contains('[') {
        format!("item `{path}` ")
    } else {
        String::new()
    }
}

/// Python-style text for a plain JSON value, so messages quote like scripts do.
fn json_repr(json: &Json) -> String {
    fn plain(json: &Json) -> Value {
        match json {
            Json::Null => Value::Null,
            Json::Bool(b) => Value::Bool(*b),
            Json::Number(n) => n.as_i64().map(Value::Int).unwrap_or_else(|| Value::Float(n.as_f64().unwrap_or(0.0))),
            Json::String(s) => Value::Str(s.clone()),
            Json::Array(items) => Value::List(items.iter().map(plain).collect()),
            Json::Object(map) => Value::Dict(map.iter().map(|(k, v)| (Value::Str(k.clone()), plain(v))).collect()),
        }
    }
    crate::value::py_repr(&plain(json))
}

/// JSON equality with numbers compared by value, so `1` equals `1.0`.
fn json_equals(value: &Value, json: &Json) -> bool {
    match (value, json) {
        (Value::Null, Json::Null) => true,
        (Value::Bool(a), Json::Bool(b)) => a == b,
        (Value::Int(a), Json::Number(b)) => b.as_i64() == Some(*a) || b.as_f64() == Some(*a as f64),
        (Value::Float(a), Json::Number(b)) => b.as_f64() == Some(*a),
        (Value::Str(a), Json::String(b)) => a == b,
        (Value::List(a), Json::Array(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| json_equals(x, y)),
        (Value::Dict(a), Json::Object(b)) => {
            a.len() == b.len()
                && a.iter().all(|(k, v)| matches!(k, Value::Str(k) if b.get(k).is_some_and(|bv| json_equals(v, bv))))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn search() -> Params {
        Params::from_json_schema(&json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "size": {"type": "integer"},
                "sort": {"enum": ["new", "top"]}
            },
            "required": ["query"]
        }))
        .unwrap()
    }

    fn msgs(problems: Vec<ArgProblem>) -> Vec<String> {
        problems.into_iter().map(|p| p.message).collect()
    }

    #[test]
    fn schema_without_properties_gives_nothing_to_check() {
        assert!(Params::from_json_schema(&json!({"type": "object"})).is_none());
        assert!(Params::from_json_schema(&json!({"properties": []})).is_none());
        let empty = Params::from_json_schema(&json!({"type": "object", "properties": {}})).unwrap();
        assert!(empty.params.is_empty());
        assert!(!empty.extra_keywords);
    }

    #[test]
    fn a_good_call_has_no_problems() {
        let p = search();
        assert!(p.validate_call("search", &[Value::str("x")], &BTreeMap::new()).is_ok());
        let kw = BTreeMap::from([("size".to_string(), Value::Int(5)), ("sort".to_string(), Value::str("top"))]);
        assert!(p.validate_call("search", &[Value::str("x")], &kw).is_ok());
    }

    #[test]
    fn unknown_keyword_suggests_the_closest_name() {
        let problems = search().problems("search", &[None], &[("sise".into(), None)]);
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].message, "tool `search` has no parameter `sise`");
        assert_eq!(problems[0].hint.as_deref(), Some("did you mean `size`?"));
        let far = search().problems("search", &[None], &[("colour".into(), None)]);
        assert_eq!(far[0].hint.as_deref(), Some("its keyword parameters are: query, size, sort"));
    }

    #[test]
    fn typo_distance_counts_a_swap_once() {
        assert_eq!(typo_distance("qyt", "qty"), 1);
        assert_eq!(typo_distance("limt", "limit"), 1);
        assert_eq!(typo_distance("abc", "abc"), 0);
        assert_eq!(typo_distance("", "ab"), 2);
        assert_eq!(closest("ID", &["id", "name"]), Some("id"));
        assert_eq!(closest("xyz", &["id", "name"]), None);
    }

    #[test]
    fn missing_required_and_too_many_positional() {
        assert_eq!(msgs(search().problems("search", &[], &[])), ["tool `search` is missing required argument(s): query"]);
        let many = search().problems("search", &[None, None, None, None], &[]);
        assert_eq!(msgs(many), ["tool `search` takes at most 3 positional argument(s) (query, size, sort), got 4"]);
    }

    #[test]
    fn same_parameter_twice() {
        let problems = search().problems("search", &[None], &[("query".into(), None)]);
        assert_eq!(msgs(problems), ["tool `search` got `query` twice, by position and by name"]);
    }

    #[test]
    fn literal_types_and_enums() {
        let p = search();
        let err = p.validate_call("search", &[Value::Int(3)], &BTreeMap::new()).unwrap_err();
        assert_eq!(err, "argument `query` of tool `search` should be str, got int 3");
        let kw = BTreeMap::from([("sort".to_string(), Value::str("old"))]);
        let err = p.validate_call("search", &[Value::str("q")], &kw).unwrap_err();
        assert_eq!(err, "argument `sort` of tool `search` must be one of 'new', 'top', got str 'old'");
        let kw = BTreeMap::from([("size".to_string(), Value::str("5"))]);
        assert!(p.validate_call("search", &[Value::str("q")], &kw).unwrap_err().contains("should be int, got str '5'"));
    }

    #[test]
    fn numbers_follow_json_rules() {
        let p = Params::from_json_schema(&json!({"properties": {
            "n": {"type": "integer"}, "x": {"type": "number"}, "b": {"type": "boolean"}}})).unwrap();
        let ok = BTreeMap::from([("n".to_string(), Value::Float(2.0)), ("x".to_string(), Value::Int(1))]);
        assert!(p.validate_call("t", &[], &ok).is_ok());
        let bad = BTreeMap::from([("n".to_string(), Value::Float(2.5))]);
        assert!(p.validate_call("t", &[], &bad).is_err());
        let bad = BTreeMap::from([("x".to_string(), Value::Bool(true))]);
        assert!(p.validate_call("t", &[], &bad).is_err());
        let bad = BTreeMap::from([("b".to_string(), Value::Int(1))]);
        assert!(p.validate_call("t", &[], &bad).is_err());
    }

    #[test]
    fn none_is_fine_for_optional_but_not_required() {
        let p = search();
        let kw = BTreeMap::from([("size".to_string(), Value::Null)]);
        assert!(p.validate_call("search", &[Value::str("q")], &kw).is_ok());
        assert!(p.validate_call("search", &[Value::Null], &BTreeMap::new()).is_err());
    }

    #[test]
    fn nested_items_properties_and_refs() {
        let p = Params::from_json_schema(&json!({
            "type": "object",
            "$defs": {"Entity": {"type": "object", "properties": {"name": {"type": "string"}, "tags": {"type": "array", "items": {"type": "string"}}}, "required": ["name"], "additionalProperties": false}},
            "properties": {"entities": {"type": "array", "items": {"$ref": "#/$defs/Entity"}}},
            "required": ["entities"]
        }))
        .unwrap();
        let entity = |pairs: Vec<(&str, Value)>| Value::Dict(pairs.into_iter().map(|(k, v)| (Value::str(k), v)).collect());
        let good = Value::List(vec![entity(vec![("name", Value::str("a")), ("tags", Value::List(vec![Value::str("x")]))])]);
        assert!(p.validate_call("t", &[good], &BTreeMap::new()).is_ok());
        let bad_tag = Value::List(vec![entity(vec![("name", Value::str("a")), ("tags", Value::List(vec![Value::Int(1)]))])]);
        assert_eq!(
            p.validate_call("t", &[bad_tag], &BTreeMap::new()).unwrap_err(),
            "argument `entities` of tool `t`: item `entities[0]['tags'][0]` should be str, got int 1"
        );
        let missing = Value::List(vec![entity(vec![("tags", Value::List(vec![]))])]);
        assert!(p.validate_call("t", &[missing], &BTreeMap::new()).unwrap_err().contains("is missing key(s): name"));
        let extra = Value::List(vec![entity(vec![("name", Value::str("a")), ("nmae", Value::str("b"))])]);
        assert!(p.validate_call("t", &[extra], &BTreeMap::new()).unwrap_err().contains("has no key 'nmae'"));
    }

    #[test]
    fn any_of_accepts_any_branch_and_unknown_keywords_are_ignored() {
        let p = Params::from_json_schema(&json!({"properties": {
            "v": {"anyOf": [{"type": "string"}, {"type": "integer"}]},
            "w": {"type": "string", "pattern": "^[a-z]+$", "maxLength": 2, "format": "email"},
            "z": {"type": "weird"}
        }})).unwrap();
        let ok = BTreeMap::from([
            ("v".to_string(), Value::Int(1)),
            ("w".to_string(), Value::str("NOT-matching-pattern")),
            ("z".to_string(), Value::List(vec![])),
        ]);
        assert!(p.validate_call("t", &[], &ok).is_ok());
        let bad = BTreeMap::from([("v".to_string(), Value::Float(1.5))]);
        assert_eq!(
            p.validate_call("t", &[], &bad).unwrap_err(),
            "argument `v` of tool `t` matches none of the allowed types (str, int), got float 1.5"
        );
    }

    #[test]
    fn additional_properties_opens_keywords() {
        let open = Params::from_json_schema(&json!({"properties": {"a": {}}, "additionalProperties": true})).unwrap();
        assert!(open.problems("t", &[], &[("zzz".into(), None)]).is_empty());
        let schema_open = Params::from_json_schema(&json!({"properties": {}, "additionalProperties": {"type": "string"}})).unwrap();
        assert!(schema_open.extra_keywords);
    }

    #[test]
    fn cyclic_refs_stop() {
        let p = Params::from_json_schema(&json!({
            "$defs": {"Node": {"$ref": "#/$defs/Node"}},
            "properties": {"n": {"$ref": "#/$defs/Node"}}
        })).unwrap();
        assert!(p.validate_call("t", &[Value::Int(1)], &BTreeMap::new()).is_ok());
        let bad_ref = Params::from_json_schema(&json!({"properties": {"n": {"$ref": "http://x/y"}}})).unwrap();
        assert!(bad_ref.validate_call("t", &[Value::Int(1)], &BTreeMap::new()).is_ok());
    }

    #[test]
    fn python_style_kinds() {
        let p = Params {
            params: vec![
                Param { keyword: false, ..Param::new("a", true) },
                Param::new("b", false),
                Param { positional: false, ..Param::new("c", true) },
            ],
            extra_positional: false,
            extra_keywords: false,
            root: None,
        };
        assert_eq!(msgs(p.problems("f", &[None], &[("a".into(), None), ("c".into(), None)])), [
            "parameter `a` of tool `f` is positional-only"
        ]);
        assert_eq!(msgs(p.problems("f", &[None, None, None], &[("c".into(), None)])), [
            "tool `f` takes at most 2 positional argument(s) (a, b), got 3"
        ]);
        assert_eq!(msgs(p.problems("f", &[None], &[])), ["tool `f` is missing required argument(s): c"]);
        let star = Params { extra_positional: true, extra_keywords: true, ..p };
        assert!(star.problems("f", &[None, None, None, None], &[("c".into(), None), ("q".into(), None)]).is_empty());
    }
}
