//! Declared return types, checked before a script runs.
//!
//! A tool can declare what it returns, as a JSON Schema or as Weave's short
//! shape notation (`{"id": str, "items": list[{"price": float}]}`, what
//! `shape_of_samples` prints). The check then follows each tool result
//! through the script and rejects a lookup that the declaration says can
//! never work: a key the result does not have, or a string key on a list,
//! string, number, bool or `None`. These are the lookups that would
//! otherwise fail halfway through a run, after earlier calls (and their
//! writes) have already happened.
//!
//! It only reports what is certain under the declaration:
//!
//! - A missing key is reported only for a closed object: a JSON Schema
//!   object with `additionalProperties: false`, or a `{...}` shape. A key
//!   that is declared but optional (`?`, or not in `required`) is fine.
//! - Values whose declared type is a union (`anyOf`, `oneOf`, a list of
//!   types, `enum`, `const`, `$ref`, `allOf`) are not checked.
//! - Lookups inside a `try` body are not checked (the handler may catch
//!   them), nor any key the script tests with `in` / `not in` or reads with
//!   `.get(...)` anywhere, since such a test may guard the lookup. Type
//!   checks (a key on a list or string) are skipped when the script calls
//!   `isinstance` or `type`.
//! - A name stops being followed when it is reassigned to something else,
//!   changed in place (`x[k] = v`, `x += ...`, a method other than `get`,
//!   `keys`, `values`, `items`, `copy`), or assigned in a loop or `try`
//!   body that the lookup can see the end of.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Map, Value as Json};

use crate::ast::{CmpOp, Expr, ExprKind, FSeg, Stmt, StmtKind, Target};
use crate::error::Diagnostic;
use crate::tools::ToolRegistry;

/// A JSON Schema for a shape string such as
/// `{"id": str, "tags": list[str], "note": str?}`. Objects written with
/// `{...}` are closed (`additionalProperties: false`) unless they end with
/// `...` (`{"id": str, ...}`); a `?` after a field makes it optional. `dict` and `dict[str, V]` are open objects, `list` and
/// `list[T]` arrays, `A | B` a union, and `str`, `int`, `float`, `bool`,
/// `None`, `any` the plain types. JSON literals (`"a" | "b"`) become
/// `enum`s. Returns `None` when the text is not in this notation.
pub fn schema_from_shape(text: &str) -> Option<Json> {
    let mut p = ShapeParser { s: text.as_bytes(), i: 0, depth: 0 };
    let schema = p.union()?;
    p.ws();
    (p.i == p.s.len()).then_some(schema)
}

struct ShapeParser<'a> {
    s: &'a [u8],
    i: usize,
    depth: usize,
}

impl ShapeParser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn union(&mut self) -> Option<Json> {
        self.depth += 1;
        if self.depth > 64 {
            return None;
        }
        let mut parts = vec![self.atom()?];
        while self.eat(b'|') {
            parts.push(self.atom()?);
        }
        self.depth -= 1;
        if parts.len() == 1 {
            return parts.pop();
        }
        if parts.iter().all(|p| p.get("const").is_some()) {
            let values: Vec<Json> = parts.iter().map(|p| p["const"].clone()).collect();
            return Some(json!({ "enum": values }));
        }
        Some(json!({ "anyOf": parts }))
    }

    fn dots(&mut self) -> bool {
        self.ws();
        if self.s[self.i..].starts_with(b"...") {
            self.i += 3;
            true
        } else {
            false
        }
    }

    fn ident(&mut self) -> Option<String> {
        self.ws();
        let start = self.i;
        while self.i < self.s.len() && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'_') {
            self.i += 1;
        }
        (self.i > start).then(|| String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
    }

    /// A JSON string literal, decoded.
    fn string(&mut self) -> Option<String> {
        self.ws();
        if self.s.get(self.i) != Some(&b'"') {
            return None;
        }
        let start = self.i;
        self.i += 1;
        while self.i < self.s.len() {
            match self.s[self.i] {
                b'\\' => self.i += 2,
                b'"' => {
                    self.i += 1;
                    let raw = std::str::from_utf8(&self.s[start..self.i]).ok()?;
                    return serde_json::from_str(raw).ok();
                }
                _ => self.i += 1,
            }
        }
        None
    }

    fn atom(&mut self) -> Option<Json> {
        self.ws();
        match self.s.get(self.i)? {
            b'{' => {
                self.i += 1;
                let mut props = Map::new();
                let mut required = Vec::new();
                let mut open = false;
                if !self.eat(b'}') {
                    loop {
                        if self.dots() {
                            // `{"id": str, ...}`: other keys may be present.
                            open = true;
                            if !self.eat(b'}') {
                                return None;
                            }
                            break;
                        }
                        let key = self.string()?;
                        if !self.eat(b':') {
                            return None;
                        }
                        let value = self.union()?;
                        if !self.eat(b'?') {
                            required.push(Json::String(key.clone()));
                        }
                        props.insert(key, value);
                        if self.eat(b'}') {
                            break;
                        }
                        if !self.eat(b',') {
                            return None;
                        }
                    }
                }
                let mut schema = json!({ "type": "object", "properties": props, "required": required });
                if !open {
                    schema["additionalProperties"] = json!(false);
                }
                Some(schema)
            }
            b'"' => Some(json!({ "const": self.string()? })),
            b'-' | b'0'..=b'9' => {
                let start = self.i;
                self.i += 1;
                while self.i < self.s.len() && matches!(self.s[self.i], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-') {
                    self.i += 1;
                }
                let n: Json = serde_json::from_str(std::str::from_utf8(&self.s[start..self.i]).ok()?).ok()?;
                Some(json!({ "const": n }))
            }
            _ => {
                let name = self.ident()?;
                match name.as_str() {
                    "list" => {
                        if self.eat(b'[') {
                            let items = self.union()?;
                            if !self.eat(b']') {
                                return None;
                            }
                            Some(json!({ "type": "array", "items": items }))
                        } else {
                            Some(json!({ "type": "array" }))
                        }
                    }
                    "dict" => {
                        if self.eat(b'[') {
                            let _key = self.union()?;
                            if !self.eat(b',') {
                                return None;
                            }
                            let value = self.union()?;
                            if !self.eat(b']') {
                                return None;
                            }
                            Some(json!({ "type": "object", "additionalProperties": value }))
                        } else {
                            Some(json!({ "type": "object" }))
                        }
                    }
                    "str" => Some(json!({ "type": "string" })),
                    "int" => Some(json!({ "type": "integer" })),
                    "float" => Some(json!({ "type": "number" })),
                    "bool" => Some(json!({ "type": "boolean" })),
                    "None" | "null" => Some(json!({ "type": "null" })),
                    "true" => Some(json!({ "const": true })),
                    "false" => Some(json!({ "const": false })),
                    "any" => Some(json!({})),
                    _ => None,
                }
            }
        }
    }
}

/// What a value is known to be: its schema and a readable description of
/// where it came from, for messages.
#[derive(Clone, Debug, PartialEq)]
struct Known {
    schema: Json,
    from: String,
}

/// The single JSON type a schema allows, or `None` when it allows several or
/// it can't tell.
fn single_type(schema: &Json) -> Option<&str> {
    let obj = schema.as_object()?;
    for k in ["anyOf", "oneOf", "allOf", "enum", "const", "$ref", "not", "if"] {
        if obj.contains_key(k) {
            return None;
        }
    }
    match obj.get("type") {
        Some(Json::String(t)) => Some(t.as_str()),
        // `{"properties": ...}` without a type is not necessarily an object.
        _ => None,
    }
}

fn type_word(t: &str) -> &'static str {
    match t {
        "array" => "a list",
        "string" => "a str",
        "integer" => "an int",
        "number" => "a float",
        "boolean" => "a bool",
        "null" => "None",
        _ => "a value",
    }
}

const READ_ONLY_METHODS: [&str; 5] = ["get", "keys", "values", "items", "copy"];

/// Facts about the whole block that make a lookup safe however the code
/// flows: keys the script tests or reads with `.get`, names it changes in
/// place, and whether it inspects types.
#[derive(Default)]
struct Guards {
    tested_keys: HashSet<String>,
    mutated: HashSet<String>,
    type_tests: bool,
}

fn collect_guards(stmts: &[Stmt], g: &mut Guards) {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Assign { targets, value } => {
                for t in targets {
                    guard_target(t, g, false);
                }
                guard_expr(value, g);
            }
            StmtKind::AugAssign { target, value, .. } => {
                guard_target(target, g, true);
                guard_expr(value, g);
            }
            StmtKind::Expr(e) => guard_expr(e, g),
            StmtKind::If { test, body, orelse } => {
                guard_expr(test, g);
                collect_guards(body, g);
                collect_guards(orelse, g);
            }
            StmtKind::For { target, iter, body } => {
                guard_target(target, g, false);
                guard_expr(iter, g);
                collect_guards(body, g);
            }
            StmtKind::Try { body, handler } => {
                collect_guards(body, g);
                collect_guards(&handler.body, g);
            }
            StmtKind::Pass => {}
        }
    }
}

fn guard_target(t: &Target, g: &mut Guards, aug: bool) {
    match t {
        Target::Name { name, .. } => {
            if aug {
                g.mutated.insert(name.clone());
            }
        }
        Target::Subscript { base, index, .. } => {
            if let ExprKind::Name(n) = &base.kind {
                g.mutated.insert(n.clone());
            }
            guard_expr(base, g);
            guard_expr(index, g);
        }
        Target::Unpack { targets, .. } => {
            for t in targets {
                guard_target(t, g, aug);
            }
        }
    }
}

fn guard_expr(e: &Expr, g: &mut Guards) {
    match &e.kind {
        ExprKind::Compare { left, ops, comparators } => {
            let mut lhs: &Expr = left;
            for (op, rhs) in ops.iter().zip(comparators) {
                if matches!(op, CmpOp::In | CmpOp::NotIn) {
                    if let ExprKind::Str(k) = &lhs.kind {
                        g.tested_keys.insert(k.clone());
                    }
                }
                lhs = rhs;
            }
        }
        ExprKind::Call { func, args, .. } => {
            match &func.kind {
                ExprKind::Attribute { value, attr } => {
                    if attr == "get" {
                        if let Some(Expr { kind: ExprKind::Str(k), .. }) = args.first() {
                            g.tested_keys.insert(k.clone());
                        }
                    }
                    if !READ_ONLY_METHODS.contains(&attr.as_str()) {
                        if let ExprKind::Name(n) = &value.kind {
                            g.mutated.insert(n.clone());
                        }
                    }
                }
                // `isinstance(x, T)` is lowered to `__weave_isinstance(x, [...])`.
                ExprKind::Name(n) if n == "__weave_isinstance" || n == "isinstance" || n == "type" => g.type_tests = true,
                _ => {}
            }
        }
        _ => {}
    }
    for child in children(e) {
        guard_expr(child, g);
    }
}

fn children(e: &Expr) -> Vec<&Expr> {
    match &e.kind {
        ExprKind::List(xs) | ExprKind::Tuple(xs) => xs.iter().collect(),
        ExprKind::Dict(items) => items.iter().flat_map(|(k, v)| [k, v]).collect(),
        ExprKind::BinOp { left, right, .. } => vec![left, right],
        ExprKind::Unary { operand, .. } => vec![operand],
        ExprKind::BoolOp { values, .. } => values.iter().collect(),
        ExprKind::Compare { left, comparators, .. } => std::iter::once(&**left).chain(comparators.iter()).collect(),
        ExprKind::Call { func, args, keywords } => std::iter::once(&**func)
            .chain(args.iter())
            .chain(keywords.iter().map(|k| &k.value))
            .collect(),
        ExprKind::Attribute { value, .. } => vec![value],
        ExprKind::Subscript { value, index } => vec![value, index],
        ExprKind::Slice { lower, upper, step } => [lower, upper, step].into_iter().flatten().map(|b| &**b).collect(),
        ExprKind::FString(segs) => segs
            .iter()
            .filter_map(|s| match s {
                FSeg::Interp { expr, .. } => Some(expr),
                FSeg::Lit(_) => None,
            })
            .collect(),
        ExprKind::IfExp { test, body, orelse } => vec![test, body, orelse],
        ExprKind::None
        | ExprKind::Bool(_)
        | ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Name(_) => Vec::new(),
    }
}

struct Checker<'a> {
    tools: &'a ToolRegistry,
    guards: Guards,
    diags: Vec<Diagnostic>,
    /// Nesting depth of `try` bodies; lookups there are not reported.
    in_try: usize,
    /// Nesting depth of branches chosen by a key test (`"k" in x`), which
    /// may be unreachable for a declared result; lookups there are not
    /// reported.
    guarded: usize,
}

type Env = HashMap<String, Known>;

/// Check every lookup on a declared tool result in `stmts`.
pub fn check_returns(stmts: &[Stmt], tools: &ToolRegistry) -> Vec<Diagnostic> {
    let mut guards = Guards::default();
    collect_guards(stmts, &mut guards);
    let mut c = Checker { tools, guards, diags: Vec::new(), in_try: 0, guarded: 0 };
    let mut env = Env::new();
    c.block(stmts, &mut env);
    for d in &mut c.diags {
        d.message = crate::parse::unhide(&d.message);
    }
    c.diags
}

impl Checker<'_> {
    fn block(&mut self, stmts: &[Stmt], env: &mut Env) {
        for stmt in stmts {
            match &stmt.kind {
                StmtKind::Assign { targets, value } => {
                    let known = self.expr(value, env);
                    for t in targets {
                        self.target_exprs(t, env);
                        self.bind(t, known.clone(), env);
                    }
                }
                StmtKind::AugAssign { target, value, .. } => {
                    self.target_exprs(target, env);
                    self.expr(value, env);
                    self.bind(target, None, env);
                }
                StmtKind::Expr(e) => {
                    self.expr(e, env);
                }
                StmtKind::If { test, body, orelse } => {
                    self.expr(test, env);
                    let keyed = tests_key(test);
                    self.guarded += usize::from(keyed);
                    let mut then_env = env.clone();
                    let mut else_env = env.clone();
                    self.block(body, &mut then_env);
                    self.block(orelse, &mut else_env);
                    self.guarded -= usize::from(keyed);
                    env.retain(|_, _| false);
                    for (name, k) in then_env {
                        if else_env.get(&name) == Some(&k) {
                            env.insert(name, k);
                        }
                    }
                }
                StmtKind::For { target, iter, body } => {
                    let iter_known = self.expr(iter, env);
                    let assigned = crate::analysis::assigned_names(body);
                    let mut inner = env.clone();
                    inner.retain(|n, _| !assigned.contains(n));
                    let item = iter_known.and_then(|k| {
                        (single_type(&k.schema) == Some("array"))
                            .then(|| k.schema.get("items").cloned())
                            .flatten()
                            .map(|schema| Known { schema, from: format!("an item of {}", k.from) })
                    });
                    self.bind(target, item, &mut inner);
                    self.block(body, &mut inner);
                    env.retain(|n, _| !assigned.contains(n));
                    let mut tnames = HashSet::new();
                    target_names(target, &mut tnames);
                    env.retain(|n, _| !tnames.contains(n));
                }
                StmtKind::Try { body, handler } => {
                    let assigned = crate::analysis::assigned_names(body);
                    let mut try_env = env.clone();
                    self.in_try += 1;
                    self.block(body, &mut try_env);
                    self.in_try -= 1;
                    let mut handler_env = env.clone();
                    handler_env.retain(|n, _| !assigned.contains(n));
                    if let Some(n) = &handler.name {
                        handler_env.remove(n);
                    }
                    self.block(&handler.body, &mut handler_env);
                    let all = crate::analysis::assigned_names(std::slice::from_ref(stmt));
                    env.retain(|n, _| !all.contains(n));
                }
                StmtKind::Pass => {}
            }
        }
    }

    fn target_exprs(&mut self, t: &Target, env: &Env) {
        match t {
            Target::Name { .. } => {}
            Target::Subscript { base, index, .. } => {
                self.expr(base, env);
                self.expr(index, env);
            }
            Target::Unpack { targets, .. } => {
                for t in targets {
                    self.target_exprs(t, env);
                }
            }
        }
    }

    fn bind(&self, t: &Target, known: Option<Known>, env: &mut Env) {
        match t {
            Target::Name { name, .. } => match known {
                Some(k) if !self.guards.mutated.contains(name) => {
                    env.insert(name.clone(), k);
                }
                _ => {
                    env.remove(name);
                }
            },
            Target::Subscript { base, .. } => {
                if let ExprKind::Name(n) = &base.kind {
                    env.remove(n);
                }
            }
            Target::Unpack { .. } => {
                let mut names = HashSet::new();
                target_names(t, &mut names);
                for n in names {
                    env.remove(&n);
                }
            }
        }
    }

    /// Check `e` and return what its value is known to be.
    fn expr(&mut self, e: &Expr, env: &Env) -> Option<Known> {
        match &e.kind {
            ExprKind::Name(n) => env.get(n).cloned(),
            ExprKind::Call { func, args, keywords } => {
                for a in args {
                    self.expr(a, env);
                }
                for k in keywords {
                    self.expr(&k.value, env);
                }
                match &func.kind {
                    ExprKind::Name(name) => {
                        let schema = self.tools.get(name)?.spec.returns.clone()?;
                        Some(Known { schema, from: format!("{name}()") })
                    }
                    other => {
                        if let ExprKind::Attribute { value, .. } = other {
                            self.expr(value, env);
                        }
                        None
                    }
                }
            }
            ExprKind::Subscript { value, index } => {
                let base = self.expr(value, env);
                self.expr(index, env);
                let base = base?;
                self.lookup(e, &base, index)
            }
            ExprKind::IfExp { test, body, orelse } => {
                self.expr(test, env);
                let keyed = tests_key(test);
                self.guarded += usize::from(keyed);
                self.expr(body, env);
                self.expr(orelse, env);
                self.guarded -= usize::from(keyed);
                None
            }
            ExprKind::BoolOp { values, .. } => {
                // `"k" in x and x["k"]`: a key test guards what follows it.
                let mut keyed = 0;
                for v in values {
                    self.guarded += keyed;
                    self.expr(v, env);
                    self.guarded -= keyed;
                    if keyed == 0 && tests_key(v) {
                        keyed = 1;
                    }
                }
                None
            }
            _ => {
                for child in children(e) {
                    self.expr(child, env);
                }
                None
            }
        }
    }

    /// `base[index]`: report a lookup that can't work and return what the
    /// result is known to be.
    fn lookup(&mut self, at: &Expr, base: &Known, index: &Expr) -> Option<Known> {
        let kind = single_type(&base.schema)?;
        let props = base.schema.get("properties").and_then(Json::as_object);
        match (&index.kind, kind) {
            (ExprKind::Str(key), "object") => {
                if let Some(sub) = props.and_then(|p| p.get(key)) {
                    return Some(Known { schema: sub.clone(), from: format!("{}[{}]", base.from, py_str(key)) });
                }
                match base.schema.get("additionalProperties") {
                    Some(Json::Bool(false)) => {
                        if base.schema.get("patternProperties").is_none() && !self.quiet_key(key) {
                            // Sorted: the map keeps declaration order only when
                            // serde_json's `preserve_order` is on.
                            let mut keys: Vec<&String> = props.map(|p| p.keys().collect()).unwrap_or_default();
                            keys.sort();
                            self.report_missing(at, base, key, &keys);
                        }
                        None
                    }
                    Some(sub @ Json::Object(_)) if props.is_none_or(|p| p.is_empty()) => {
                        Some(Known { schema: sub.clone(), from: format!("{}[{}]", base.from, py_str(key)) })
                    }
                    _ => None,
                }
            }
            (ExprKind::Str(key), "array" | "string" | "integer" | "number" | "boolean" | "null") => {
                if !self.quiet_type() {
                    let what = type_word(kind);
                    let hint = if kind == "array" {
                        "it is a list: loop over it, or index it with a number first".to_string()
                    } else {
                        format!("{} is declared to return {what} here", base.from)
                    };
                    self.diags.push(
                        Diagnostic::static_err(
                            at.line,
                            at.col,
                            format!("can't look up {} in {}: it is declared as {what}", py_str(key), base.from),
                        )
                        .with_hint(hint),
                    );
                }
                None
            }
            (_, "array") => base
                .schema
                .get("items")
                .filter(|s| s.is_object())
                .map(|schema| Known { schema: schema.clone(), from: format!("an item of {}", base.from) }),
            (ExprKind::Int(_) | ExprKind::Str(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::None, _) => None,
            (_, "object") => {
                // A computed key: only `dict[str, V]` says what it gives.
                match base.schema.get("additionalProperties") {
                    Some(sub @ Json::Object(_)) if props.is_none_or(|p| p.is_empty()) => {
                        Some(Known { schema: sub.clone(), from: format!("a value of {}", base.from) })
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn quiet_key(&self, key: &str) -> bool {
        self.in_try > 0 || self.guarded > 0 || self.guards.tested_keys.contains(key)
    }

    fn quiet_type(&self) -> bool {
        self.in_try > 0 || self.guarded > 0 || self.guards.type_tests
    }

    fn report_missing(&mut self, at: &Expr, base: &Known, key: &str, keys: &[&String]) {
        let listed: Vec<String> = keys.iter().take(8).map(|k| py_str(k)).collect();
        let more = if keys.len() > 8 { format!(" and {} more", keys.len() - 8) } else { String::new() };
        let message = if keys.is_empty() {
            format!("{} is declared with no keys, so it has no key {}", base.from, py_str(key))
        } else {
            format!("{} has no key {}; its declared keys are {}{more}", base.from, py_str(key), listed.join(", "))
        };
        let lower = key.to_lowercase();
        let close = keys
            .iter()
            .map(|k| (edit_distance(&lower, &k.to_lowercase()), *k))
            .filter(|(d, k)| *d <= 2.max(k.len() / 3))
            .min()
            .or_else(|| {
                // `fuel` for `fuelLevel`, `order_history` for `history`.
                let mut part: Vec<&String> = keys
                    .iter()
                    .copied()
                    .filter(|k| {
                        let k = k.to_lowercase();
                        lower.len() >= 3 && k.len() >= 3 && (k.starts_with(&lower) || lower.ends_with(&k))
                    })
                    .collect();
                (part.len() == 1).then(|| (0, part.remove(0)))
            });
        let hint = match close {
            Some((_, k)) => format!("did you mean {}?", py_str(k)),
            None => "use one of the declared keys; if the tool really returns more, fix its declared return type".to_string(),
        };
        self.diags.push(Diagnostic::static_err(at.line, at.col, message).with_hint(hint));
    }
}

/// Whether `e` contains a key test: `"k" in x`, `"k" not in x`, or
/// `x.get("k")`.
fn tests_key(e: &Expr) -> bool {
    let here = match &e.kind {
        ExprKind::Compare { left, ops, .. } => {
            matches!(left.kind, ExprKind::Str(_)) && ops.iter().any(|op| matches!(op, CmpOp::In | CmpOp::NotIn))
        }
        ExprKind::Call { func, .. } => matches!(&func.kind, ExprKind::Attribute { attr, .. } if attr == "get"),
        _ => false,
    };
    here || children(e).into_iter().any(tests_key)
}

fn target_names(t: &Target, out: &mut HashSet<String>) {
    match t {
        Target::Name { name, .. } => {
            out.insert(name.clone());
        }
        Target::Subscript { base, .. } => {
            if let ExprKind::Name(n) = &base.kind {
                out.insert(n.clone());
            }
        }
        Target::Unpack { targets, .. } => {
            for t in targets {
                target_names(t, out);
            }
        }
    }
}

fn py_str(s: &str) -> String {
    crate::value::py_repr(&crate::value::Value::Str(s.to_string()))
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len() > 40 || b.len() > 40 {
        return usize::MAX;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}
