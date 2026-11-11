//! Lowering of the Python that models reach for: comprehensions, generator
//! expressions, `key=` functions, `map`/`filter`, sets, and top-level `def`
//! helpers. Everything becomes ordinary Weave statements (loops, `+=`
//! accumulators, flag-guarded early exits) placed by [`crate::anf`], so effect
//! analysis, scheduling, replay and limits see exactly what a hand-written
//! loop would give them.
//!
//! Helpers are inlined at each call site with their parameters and locals
//! renamed, so a helper's free names are the script's globals at the time of
//! the call, as in Python. A helper body becomes a one-iteration loop when it
//! returns early; `return` sets the break flag of every enclosing loop.

use std::collections::{HashMap, HashSet};

use ruff_python_ast as ast;
use ruff_python_ast::{Expr, Stmt};
use ruff_python_parser::parse_module;
use ruff_text_size::{Ranged, TextSize};

use super::{assign_flag, desugar_jumps, name_expr, DefInfo, FuncCtx, LineIndex, Lowerer, HIDDEN_PREFIX};
use crate::anf::{self, Anf};
use crate::ast::{BinOp, CmpOp, Expr as WExpr, ExprKind, Stmt as WStmt, StmtKind, Target, UnaryOp};

/// Helper call sites expanded per program (inlining is the only cost).
pub(super) const MAX_HELPER_EXPANSIONS: usize = 200;
/// Helpers calling helpers, deepest chain.
const MAX_HELPER_NESTING: usize = 8;

const MUTATORS: &[&str] = &[
    "append", "extend", "insert", "pop", "remove", "clear", "update", "sort", "reverse", "setdefault", "popitem", "add",
    "discard",
];

const RESERVED: &[&str] = &["map", "filter", "set", "dict", "tuple", "ToolError", "print"];

fn ex(line: usize, col: usize, kind: ExprKind) -> WExpr {
    WExpr { line, col, kind }
}

fn st(line: usize, col: usize, kind: StmtKind) -> WStmt {
    WStmt { line, col, kind }
}

fn assign(line: usize, col: usize, name: &str, value: WExpr) -> WStmt {
    st(line, col, StmtKind::Assign { targets: vec![Target::Name { line, col, name: name.to_string() }], value })
}

fn append(line: usize, col: usize, name: &str, item: WExpr) -> WStmt {
    st(
        line,
        col,
        StmtKind::AugAssign {
            target: Target::Name { line, col, name: name.to_string() },
            op: BinOp::Add,
            value: ex(line, col, ExprKind::List(vec![item])),
        },
    )
}

fn call(line: usize, col: usize, func: &str, args: Vec<WExpr>) -> WExpr {
    ex(line, col, ExprKind::Call { func: Box::new(name_expr(line, col, func)), args, keywords: Vec::new() })
}

fn none(line: usize, col: usize) -> WExpr {
    ex(line, col, ExprKind::None)
}

fn if_stmt(line: usize, col: usize, test: WExpr, body: Vec<WStmt>) -> WStmt {
    st(line, col, StmtKind::If { test, body, orelse: Vec::new() })
}

fn for_stmt(line: usize, col: usize, target: Target, iter: WExpr, body: Vec<WStmt>) -> WStmt {
    st(line, col, StmtKind::For { target, iter, body })
}

fn target_names(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::Name(n) => out.push(n.id.as_str().to_string()),
        Expr::Tuple(t) => t.elts.iter().for_each(|e| target_names(e, out)),
        Expr::List(t) => t.elts.iter().for_each(|e| target_names(e, out)),
        Expr::Starred(s) => target_names(&s.value, out),
        _ => {}
    }
}

/// Names a function body binds (Python makes them local to the function).
fn collect_locals(stmts: &[Stmt], out: &mut Vec<String>) {
    for stmt in stmts {
        match stmt {
            Stmt::Assign(s) => s.targets.iter().for_each(|t| target_names(t, out)),
            Stmt::AugAssign(s) => target_names(&s.target, out),
            Stmt::AnnAssign(s) => target_names(&s.target, out),
            Stmt::For(s) => {
                target_names(&s.target, out);
                collect_locals(&s.body, out);
                collect_locals(&s.orelse, out);
            }
            Stmt::While(s) => {
                collect_locals(&s.body, out);
                collect_locals(&s.orelse, out);
            }
            Stmt::If(s) => {
                collect_locals(&s.body, out);
                for clause in &s.elif_else_clauses {
                    collect_locals(&clause.body, out);
                }
            }
            Stmt::Try(s) => {
                collect_locals(&s.body, out);
                for handler in &s.handlers {
                    let ast::ExceptHandler::ExceptHandler(h) = handler;
                    if let Some(name) = &h.name {
                        out.push(name.id.as_str().to_string());
                    }
                    collect_locals(&h.body, out);
                }
                collect_locals(&s.orelse, out);
                collect_locals(&s.finalbody, out);
            }
            Stmt::With(s) => collect_locals(&s.body, out),
            _ => {}
        }
    }
}

fn count_returns(stmts: &[Stmt]) -> usize {
    stmts
        .iter()
        .map(|stmt| match stmt {
            Stmt::Return(_) => 1,
            Stmt::For(s) => count_returns(&s.body) + count_returns(&s.orelse),
            Stmt::While(s) => count_returns(&s.body) + count_returns(&s.orelse),
            Stmt::If(s) => count_returns(&s.body) + s.elif_else_clauses.iter().map(|c| count_returns(&c.body)).sum::<usize>(),
            Stmt::Try(s) => {
                count_returns(&s.body)
                    + s.handlers
                        .iter()
                        .map(|h| {
                            let ast::ExceptHandler::ExceptHandler(h) = h;
                            count_returns(&h.body)
                        })
                        .sum::<usize>()
                    + count_returns(&s.orelse)
                    + count_returns(&s.finalbody)
            }
            Stmt::With(s) => count_returns(&s.body),
            _ => 0,
        })
        .sum()
}

fn root_name(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Name(n) => Some(n.id.as_str()),
        Expr::Subscript(s) => root_name(&s.value),
        Expr::Attribute(a) => root_name(&a.value),
        _ => None,
    }
}

/// In-place changes to a parameter. Python would change the caller's value
/// too; Weave values are immutable, so these are rejected rather than
/// silently diverging. A parameter rebound with `p = ...` first is a copy.
fn param_mutations(stmts: &[Stmt], params: &mut HashSet<String>, out: &mut Vec<(TextSize, String)>) {
    for stmt in stmts {
        match stmt {
            Stmt::Expr(s) => {
                if let Expr::Call(c) = s.value.as_ref() {
                    if let Expr::Attribute(a) = c.func.as_ref() {
                        if MUTATORS.contains(&a.attr.id.as_str()) {
                            if let Some(root) = root_name(&a.value) {
                                if params.contains(root) {
                                    out.push((stmt.start(), root.to_string()));
                                }
                            }
                        }
                    }
                }
            }
            Stmt::Assign(s) => {
                for t in &s.targets {
                    match t {
                        Expr::Subscript(_) | Expr::Attribute(_) => {
                            if let Some(root) = root_name(t) {
                                if params.contains(root) {
                                    out.push((stmt.start(), root.to_string()));
                                }
                            }
                        }
                        Expr::Name(n) => {
                            params.remove(n.id.as_str());
                        }
                        _ => {}
                    }
                }
            }
            Stmt::AugAssign(s) => match s.target.as_ref() {
                Expr::Name(n)
                    if params.contains(n.id.as_str())
                        && matches!(s.value.as_ref(), Expr::List(_) | Expr::ListComp(_)) =>
                {
                    out.push((stmt.start(), n.id.as_str().to_string()));
                }
                t @ (Expr::Subscript(_) | Expr::Attribute(_)) => {
                    if let Some(root) = root_name(t) {
                        if params.contains(root) {
                            out.push((stmt.start(), root.to_string()));
                        }
                    }
                }
                _ => {}
            },
            Stmt::For(s) => {
                param_mutations(&s.body, params, out);
            }
            Stmt::If(s) => {
                param_mutations(&s.body, params, out);
                for clause in &s.elif_else_clauses {
                    param_mutations(&clause.body, params, out);
                }
            }
            Stmt::Try(s) => {
                param_mutations(&s.body, params, out);
                for handler in &s.handlers {
                    let ast::ExceptHandler::ExceptHandler(h) = handler;
                    param_mutations(&h.body, params, out);
                }
            }
            _ => {}
        }
    }
}

fn is_literal(expr: &Expr) -> bool {
    match expr {
        Expr::NoneLiteral(_) | Expr::BooleanLiteral(_) | Expr::NumberLiteral(_) | Expr::StringLiteral(_) => true,
        Expr::UnaryOp(u) => matches!(u.operand.as_ref(), Expr::NumberLiteral(_)),
        Expr::Tuple(t) => t.elts.iter().all(is_literal),
        _ => false,
    }
}

fn params_of(p: &ast::Parameters) -> Vec<(&ast::ParameterWithDefault, bool, bool)> {
    // (param, accepts positional, accepts keyword)
    let mut out = Vec::new();
    out.extend(p.posonlyargs.iter().map(|x| (x, true, false)));
    out.extend(p.args.iter().map(|x| (x, true, true)));
    out.extend(p.kwonlyargs.iter().map(|x| (x, false, true)));
    out
}

enum Comp {
    List,
    Set,
    Dict,
}

type CallArgs = (Vec<WExpr>, Vec<(String, WExpr, TextSize)>, TextSize);

impl Lowerer<'_, '_> {
    pub(super) fn fresh(&mut self) -> usize {
        self.st.fresh += 1;
        self.st.fresh
    }

    fn tmp(&mut self, tag: &str) -> String {
        let n = self.fresh();
        format!("{HIDDEN_PREFIX}{tag}{n}")
    }

    /// A renamed user variable; diagnostics strip the prefix back off.
    fn var(&mut self, name: &str) -> String {
        let n = self.fresh();
        format!("{HIDDEN_PREFIX}v{n}_{name}")
    }

    fn hole(&mut self, line: usize, col: usize, stmts: Vec<WStmt>, result: WExpr) -> WExpr {
        let id = self.holes.len();
        self.holes.push(Some((stmts, result)));
        anf::hole_expr(line, col, id)
    }

    pub(super) fn resolve(&self, name: &str) -> String {
        for (map, inherit) in self.scopes.iter().rev() {
            if let Some(n) = map.get(name) {
                return n.clone();
            }
            if !inherit {
                break;
            }
        }
        name.to_string()
    }

    fn shadowed(&self, name: &str) -> bool {
        self.resolve(name) != name
    }

    pub(super) fn is_helper(&self, name: &str) -> bool {
        !self.shadowed(name) && (self.st.defs.contains_key(name) || self.later_defs.contains(name))
    }

    /// Splice every hole into place (see [`crate::anf`]).
    pub(super) fn place(&mut self, stmts: Vec<WStmt>) -> Vec<WStmt> {
        let mut holes = std::mem::take(&mut self.holes);
        let mut errors = Vec::new();
        let out = Anf { holes: &mut holes, fresh: &mut self.st.fresh, errors: &mut errors }.block(stmts);
        self.errors.extend(errors);
        out
    }

    /// Expression sugar; `None` means "lower it the ordinary way".
    pub(super) fn sugar(&mut self, expr: &Expr) -> Option<WExpr> {
        let (line, col) = self.loc(expr.start());
        match expr {
            Expr::ListComp(c) => Some(self.comp(line, col, Comp::List, Some(&c.elt), None, &c.generators)),
            Expr::Generator(c) => Some(self.comp(line, col, Comp::List, Some(&c.elt), None, &c.generators)),
            Expr::SetComp(c) => Some(self.comp(line, col, Comp::Set, Some(&c.elt), None, &c.generators)),
            Expr::DictComp(c) => match c.key.as_deref() {
                Some(key) => Some(self.comp(line, col, Comp::Dict, None, Some((key, &c.value)), &c.generators)),
                None => {
                    self.err(expr.start(), "dict unpacking in a comprehension is not supported");
                    Some(none(line, col))
                }
            },
            Expr::Set(set) => {
                if set.elts.iter().any(|e| matches!(e, Expr::Starred(_))) {
                    self.err(expr.start(), "starred set items are not supported");
                }
                let items = set.elts.iter().map(|e| self.expr(e)).collect();
                Some(call(line, col, "__weave_unique", vec![ex(line, col, ExprKind::List(items))]))
            }
            Expr::Lambda(_) => {
                self.err_hint(
                    expr.start(),
                    "`lambda` is only supported as `key=` (sorted, min, max, list.sort) or as the function given to map() or filter()",
                    "use a comprehension, or write a `def` helper at the top level and call it",
                );
                Some(none(line, col))
            }
            Expr::Call(c) => self.sugar_call(expr, c),
            _ => None,
        }
    }

    fn sugar_call(&mut self, at: &Expr, c: &ast::ExprCall) -> Option<WExpr> {
        if let Expr::Attribute(a) = c.func.as_ref() {
            return self.sugar_module_call(at, c, a);
        }
        let Expr::Name(f) = c.func.as_ref() else {
            return None;
        };
        let fname = f.id.as_str();
        if self.shadowed(fname) {
            return None;
        }
        if self.is_helper(fname) {
            return Some(self.call_helper(at, c, fname));
        }
        let (line, col) = self.loc(at.start());
        let args = &c.arguments.args;
        let kws = &c.arguments.keywords;
        match fname {
            "sorted" | "min" | "max" if !kws.is_empty() => {
                if args.len() != 1 || matches!(args[0], Expr::Starred(_)) {
                    self.err_hint(
                        at.start(),
                        format!("`{fname}()` with keyword arguments takes exactly one iterable"),
                        format!("pass one list, e.g. `{fname}(items, key=lambda x: x[\"score\"])`"),
                    );
                    return Some(none(line, col));
                }
                let items = self.expr(&args[0]);
                Some(self.keyed(line, col, fname, items, kws))
            }
            "map" | "filter" => Some(self.map_filter(at, fname, args, kws)),
            "isinstance" => Some(self.isinstance(at, args, kws)),
            "dict" if args.len() <= 1 && !args.iter().any(|a| matches!(a, Expr::Starred(_))) => {
                let mut out = match args.first() {
                    Some(a) => {
                        let v = self.expr(a);
                        call(line, col, "__weave_dict_from", vec![v])
                    }
                    None => ex(line, col, ExprKind::Dict(Vec::new())),
                };
                if !kws.is_empty() {
                    let mut pairs = Vec::new();
                    for kw in kws {
                        let Some(n) = &kw.arg else {
                            self.err_hint(kw.start(), "`dict(**d)` is not supported", "copy with `dict(d)` or `{**d}` is not available; use `d.copy()`");
                            return Some(none(line, col));
                        };
                        pairs.push((ex(line, col, ExprKind::Str(n.id.as_str().to_string())), self.expr(&kw.value)));
                    }
                    out = call(line, col, "__weave_merge", vec![out, ex(line, col, ExprKind::Dict(pairs))]);
                }
                Some(out)
            }
            "tuple" if args.len() <= 1 && kws.is_empty() => {
                let inner = match args.first() {
                    None => ex(line, col, ExprKind::List(Vec::new())),
                    Some(a) => self.expr(a),
                };
                Some(call(line, col, "list", vec![inner]))
            }
            "any" | "all" if args.len() == 1 && kws.is_empty() => match &args[0] {
                Expr::Generator(g) if g.generators.len() == 1 && !g.generators[0].is_async => {
                    Some(self.short_circuit(line, col, fname == "any", &g.elt, &g.generators[0]))
                }
                _ => None,
            },
            "set" if kws.is_empty() && args.len() <= 1 => {
                let inner = match args.first() {
                    None => ex(line, col, ExprKind::List(Vec::new())),
                    Some(a) => self.expr(a),
                };
                Some(call(line, col, "__weave_unique", vec![inner]))
            }
            _ => None,
        }
    }

    /// `json.dumps(v)` and `json.loads(s)`: the scripts cannot import, but
    /// models write the module prefix anyway.
    fn sugar_module_call(&mut self, at: &Expr, c: &ast::ExprCall, a: &ast::ExprAttribute) -> Option<WExpr> {
        let Expr::Name(m) = a.value.as_ref() else { return None };
        if m.id.as_str() != "json" || self.shadowed("json") {
            return None;
        }
        let (line, col) = self.loc(at.start());
        let args = &c.arguments.args;
        let target = match a.attr.id.as_str() {
            "dumps" => "__weave_json_dumps",
            "loads" => "parse_json",
            other => {
                self.err_hint(at.start(), format!("`json.{other}` is not supported"), "use `json.dumps(value)` or `json.loads(text)`");
                return Some(none(line, col));
            }
        };
        if args.len() != 1 || matches!(args[0], Expr::Starred(_)) || !c.arguments.keywords.is_empty() {
            self.err_hint(
                at.start(),
                format!("`json.{}()` takes exactly one argument here", a.attr.id.as_str()),
                "drop `indent=`/other keywords; the output uses Python's default separators",
            );
            return Some(none(line, col));
        }
        let v = self.expr(&args[0]);
        Some(call(line, col, target, vec![v]))
    }

    /// `isinstance(x, T)` or `isinstance(x, (T1, T2))` over builtin types.
    fn isinstance(&mut self, at: &Expr, args: &[Expr], kws: &[ast::Keyword]) -> WExpr {
        let (line, col) = self.loc(at.start());
        const TYPES: &[&str] = &["int", "float", "str", "bool", "list", "dict", "tuple", "set", "NoneType"];
        let hint = "use builtin types, e.g. `isinstance(x, dict)` or `isinstance(x, (int, float))`";
        if args.len() != 2 || !kws.is_empty() {
            self.err_hint(at.start(), "`isinstance()` takes a value and a type", hint);
            return none(line, col);
        }
        let mut names = Vec::new();
        let elts: Vec<&Expr> = match &args[1] {
            Expr::Tuple(t) => t.elts.iter().collect(),
            other => vec![other],
        };
        for e in elts {
            match e {
                Expr::Name(n) if TYPES.contains(&n.id.as_str()) && !self.shadowed(n.id.as_str()) => {
                    names.push(ex(line, col, ExprKind::Str(n.id.as_str().to_string())));
                }
                Expr::NoneLiteral(_) => names.push(ex(line, col, ExprKind::Str("NoneType".into()))),
                other => {
                    self.err_hint(other.start(), "`isinstance()` supports only builtin types", hint);
                    return none(line, col);
                }
            }
        }
        let v = self.expr(&args[0]);
        call(line, col, "__weave_isinstance", vec![v, ex(line, col, ExprKind::List(names))])
    }

    /// `sorted`/`min`/`max`/`list.sort` with `key=`, `reverse=` or `default=`.
    pub(super) fn keyed(&mut self, line: usize, col: usize, kind: &str, items: WExpr, kws: &[ast::Keyword]) -> WExpr {
        let mut key = None;
        let mut rev = None;
        let mut default = None;
        for kw in kws {
            let n = kw.arg.as_ref().map(|a| a.id.as_str());
            match (kind, n) {
                (_, Some("key")) => key = Some(&kw.value),
                ("sorted", Some("reverse")) => rev = Some(&kw.value),
                ("min" | "max", Some("default")) => default = Some(&kw.value),
                _ => {
                    let what = n.map(|n| format!("`{n}=`")).unwrap_or_else(|| "`**` keywords".to_string());
                    let hint = if kind == "sorted" { "use `key=` and `reverse=`" } else { "use `key=` and `default=`" };
                    self.err_hint(kw.start(), format!("`{kind}()` does not take {what}"), hint);
                }
            }
        }
        let it = self.tmp("items");
        let mut stmts = vec![assign(line, col, &it, call(line, col, "list", vec![items]))];
        let extra = |this: &mut Self, value: Option<&Expr>, tag: &str, stmts: &mut Vec<WStmt>| {
            value.map(|v| {
                let v = this.expr(v);
                let t = this.tmp(tag);
                stmts.push(assign(line, col, &t, v));
                t
            })
        };
        let revn = extra(self, rev, "rev", &mut stmts);
        let defn = extra(self, default, "default", &mut stmts);
        let keys = match key {
            None | Some(Expr::NoneLiteral(_)) => it.clone(),
            Some(k) => {
                let ks = self.tmp("keys");
                stmts.push(assign(line, col, &ks, ex(line, col, ExprKind::List(Vec::new()))));
                if let Some((v, applied)) = self.apply_fn(k, "key") {
                    stmts.push(for_stmt(
                        line,
                        col,
                        Target::Name { line, col, name: v },
                        name_expr(line, col, &it),
                        vec![append(line, col, &ks, applied)],
                    ));
                }
                ks
            }
        };
        let mut args = vec![
            ex(line, col, ExprKind::Str(kind.to_string())),
            name_expr(line, col, &it),
            name_expr(line, col, &keys),
            revn.map(|r| name_expr(line, col, &r)).unwrap_or(ex(line, col, ExprKind::Bool(false))),
        ];
        if let Some(d) = defn {
            args.push(name_expr(line, col, &d));
        }
        self.hole(line, col, stmts, call(line, col, "__weave_by_key", args))
    }

    /// A one-argument function value applied to a fresh loop variable:
    /// returns the variable to bind and the applied expression.
    fn apply_fn(&mut self, f: &Expr, role: &str) -> Option<(String, WExpr)> {
        let (line, col) = self.loc(f.start());
        match f {
            Expr::Lambda(l) => {
                let one = l.parameters.as_deref().filter(|p| {
                    p.posonlyargs.is_empty()
                        && p.kwonlyargs.is_empty()
                        && p.vararg.is_none()
                        && p.kwarg.is_none()
                        && p.args.len() == 1
                        && p.args[0].default.is_none()
                });
                let Some(p) = one else {
                    self.err_hint(
                        f.start(),
                        format!("the {role} function must take exactly one argument"),
                        "write `lambda x: ...`; to use two values, take one pair `lambda p: p[0] + p[1]`",
                    );
                    return None;
                };
                let pname = p.args[0].parameter.name.id.as_str().to_string();
                let v = self.var(&pname);
                self.scopes.push((HashMap::from([(pname, v.clone())]), true));
                let body = self.expr(&l.body);
                self.scopes.pop();
                Some((v, body))
            }
            Expr::Name(n) => {
                let v = self.tmp("x");
                let arg = name_expr(line, col, &v);
                let id = n.id.as_str();
                let applied = if self.is_helper(id) {
                    self.expand_helper(id, f.start(), vec![arg], Vec::new())
                } else {
                    let func = name_expr(line, col, &self.resolve(id));
                    ex(line, col, ExprKind::Call { func: Box::new(func), args: vec![arg], keywords: Vec::new() })
                };
                Some((v, applied))
            }
            Expr::Attribute(a) => {
                let v = self.tmp("x");
                let arg = name_expr(line, col, &v);
                let attr = a.attr.id.as_str().to_string();
                let applied = match a.value.as_ref() {
                    // `key=str.lower` calls the method on each item.
                    Expr::Name(t) if matches!(t.id.as_str(), "str" | "dict" | "list") && !self.shadowed(t.id.as_str()) => ex(
                        line,
                        col,
                        ExprKind::Call {
                            func: Box::new(ex(line, col, ExprKind::Attribute { value: Box::new(arg), attr })),
                            args: Vec::new(),
                            keywords: Vec::new(),
                        },
                    ),
                    // `key=counts.get` calls the bound method with each item.
                    recv => {
                        let recv = self.expr(recv);
                        ex(
                            line,
                            col,
                            ExprKind::Call {
                                func: Box::new(ex(line, col, ExprKind::Attribute { value: Box::new(recv), attr })),
                                args: vec![arg],
                                keywords: Vec::new(),
                            },
                        )
                    }
                };
                Some((v, applied))
            }
            _ => {
                self.err_hint(
                    f.start(),
                    format!("this {role} function is not supported"),
                    "use `lambda x: ...`, a builtin, a tool, or a `def` helper",
                );
                None
            }
        }
    }

    fn map_filter(&mut self, at: &Expr, fname: &str, args: &[Expr], kws: &[ast::Keyword]) -> WExpr {
        let (line, col) = self.loc(at.start());
        if !kws.is_empty() || args.len() != 2 {
            self.err_hint(
                at.start(),
                format!("`{fname}()` takes a function and one list"),
                "use a comprehension, e.g. `[f(x) for x in xs]` or `[f(a, b) for a, b in zip(xs, ys)]`",
            );
            return none(line, col);
        }
        let iter = self.expr(&args[1]);
        let r = self.tmp("r");
        let mut stmts = vec![assign(line, col, &r, ex(line, col, ExprKind::List(Vec::new())))];
        if fname == "filter" && matches!(args[0], Expr::NoneLiteral(_)) {
            let v = self.tmp("x");
            let body = vec![if_stmt(line, col, name_expr(line, col, &v), vec![append(line, col, &r, name_expr(line, col, &v))])];
            stmts.push(for_stmt(line, col, Target::Name { line, col, name: v }, iter, body));
        } else if let Some((v, applied)) = self.apply_fn(&args[0], fname) {
            let body = if fname == "map" {
                vec![append(line, col, &r, applied)]
            } else {
                vec![if_stmt(line, col, applied, vec![append(line, col, &r, name_expr(line, col, &v))])]
            };
            stmts.push(for_stmt(line, col, Target::Name { line, col, name: v }, iter, body));
        }
        self.hole(line, col, stmts, name_expr(line, col, &r))
    }

    fn comp(
        &mut self,
        line: usize,
        col: usize,
        kind: Comp,
        elt: Option<&Expr>,
        kv: Option<(&Expr, &Expr)>,
        gens: &[ast::Comprehension],
    ) -> WExpr {
        for g in gens {
            if g.is_async {
                self.err(g.start(), "`async for` in a comprehension is not supported");
            }
        }
        // The first iterable is evaluated in the enclosing scope.
        let mut iter0 = Some(self.expr(&gens[0].iter));
        let mut names = Vec::new();
        for g in gens {
            target_names(&g.target, &mut names);
        }
        let map: HashMap<String, String> = names.iter().map(|n| (n.clone(), self.var(n))).collect();
        self.scopes.push((map, true));
        let mut levels = Vec::new();
        for g in gens {
            let target = self.target(&g.target);
            let iter = match iter0.take() {
                Some(i) => i,
                None => self.expr(&g.iter),
            };
            let ifs: Vec<WExpr> = g.ifs.iter().map(|c| self.expr(c)).collect();
            levels.push((target, iter, ifs));
        }
        let r = self.tmp("r");
        let mut body = match (elt, kv) {
            (Some(elt), _) => {
                let v = self.expr(elt);
                vec![append(line, col, &r, v)]
            }
            (None, Some((k, v))) => {
                // Key before value, as in Python 3.8+; pairs keep the loop an
                // accumulator, so its calls still run in parallel.
                let k = self.expr(k);
                let v = self.expr(v);
                vec![append(line, col, &r, ex(line, col, ExprKind::List(vec![k, v])))]
            }
            (None, None) => unreachable!("a comprehension has an element"),
        };
        self.scopes.pop();
        for (target, iter, ifs) in levels.into_iter().rev() {
            for c in ifs.into_iter().rev() {
                body = vec![if_stmt(line, col, c, body)];
            }
            body = vec![for_stmt(line, col, target, iter, body)];
        }
        let mut stmts = vec![assign(line, col, &r, ex(line, col, ExprKind::List(Vec::new())))];
        stmts.extend(body);
        let result = match kind {
            Comp::List => name_expr(line, col, &r),
            Comp::Set => call(line, col, "__weave_unique", vec![name_expr(line, col, &r)]),
            Comp::Dict => call(line, col, "__weave_dict", vec![name_expr(line, col, &r)]),
        };
        self.hole(line, col, stmts, result)
    }

    /// `any(... for ...)` / `all(... for ...)` stop at the first decisive item,
    /// as in Python, so later calls are not made.
    fn short_circuit(&mut self, line: usize, col: usize, is_any: bool, elt: &Expr, g: &ast::Comprehension) -> WExpr {
        let iter = self.expr(&g.iter);
        let mut names = Vec::new();
        target_names(&g.target, &mut names);
        let map: HashMap<String, String> = names.iter().map(|n| (n.clone(), self.var(n))).collect();
        self.scopes.push((map, true));
        let target = self.target(&g.target);
        let ifs: Vec<WExpr> = g.ifs.iter().map(|c| self.expr(c)).collect();
        let cond = self.expr(elt);
        self.scopes.pop();
        let r = self.tmp("r");
        let brk = self.tmp("brk");
        let cont = self.tmp("cont");
        let hit = if is_any {
            cond
        } else {
            ex(line, col, ExprKind::Unary { op: UnaryOp::Not, operand: Box::new(cond) })
        };
        let mut body = vec![if_stmt(
            line,
            col,
            hit,
            vec![assign(line, col, &r, ex(line, col, ExprKind::Bool(is_any))), st(line, col, assign_flag(line, col, &brk, true))],
        )];
        for c in ifs.into_iter().rev() {
            body = vec![if_stmt(line, col, c, body)];
        }
        let lp = st(line, col, desugar_jumps(line, col, target, iter, body, &brk, &cont));
        self.hole(line, col, vec![assign(line, col, &r, ex(line, col, ExprKind::Bool(!is_any))), lp], name_expr(line, col, &r))
    }

    /// `xs.sort(...)`, `xs.reverse()` and `s.add(v)` on a plain name.
    pub(super) fn sugar_method_stmt(&mut self, value: &Expr) -> Option<StmtKind> {
        let Expr::Call(c) = value else { return None };
        let Expr::Attribute(a) = c.func.as_ref() else { return None };
        let Expr::Name(recv) = a.value.as_ref() else { return None };
        let (line, col) = self.loc(value.start());
        let rname = self.resolve(recv.id.as_str());
        let args = &c.arguments.args;
        let kws = &c.arguments.keywords;
        let rebind = |value: WExpr| StmtKind::Assign { targets: vec![Target::Name { line, col, name: rname.clone() }], value };
        match a.attr.id.as_str() {
            "sort" if args.is_empty() => {
                let v = self.keyed(line, col, "sorted", name_expr(line, col, &rname), kws);
                Some(rebind(v))
            }
            "reverse" if args.is_empty() && kws.is_empty() => {
                Some(rebind(call(line, col, "reversed", vec![name_expr(line, col, &rname)])))
            }
            "update" if args.len() <= 1 && !matches!(args.first(), Some(Expr::Starred(_))) => {
                let mut extra = match args.first() {
                    Some(a) => self.expr(a),
                    None => ex(line, col, ExprKind::Dict(Vec::new())),
                };
                if !kws.is_empty() {
                    let mut pairs = Vec::new();
                    for kw in kws {
                        let Some(n) = &kw.arg else {
                            self.err_hint(kw.start(), "`update(**d)` is not supported", "pass the dict: `d.update(other)`");
                            return Some(StmtKind::Pass);
                        };
                        pairs.push((ex(line, col, ExprKind::Str(n.id.as_str().to_string())), self.expr(&kw.value)));
                    }
                    let kwd = ex(line, col, ExprKind::Dict(pairs));
                    extra = if args.is_empty() { kwd } else { call(line, col, "__weave_merge", vec![extra, kwd]) };
                }
                Some(rebind(call(line, col, "__weave_merge", vec![name_expr(line, col, &rname), extra])))
            }
            "pop" if args.len() <= 2 && kws.is_empty() && !args.iter().any(|a| matches!(a, Expr::Starred(_))) => {
                let mut a: Vec<WExpr> = vec![name_expr(line, col, &rname)];
                a.extend(args.iter().map(|x| self.expr(x)));
                Some(rebind(call(line, col, "__weave_without", a)))
            }
            "add" if args.len() == 1 && kws.is_empty() => {
                let v = self.expr(&args[0]);
                let t = self.tmp("item");
                let test = ex(
                    line,
                    col,
                    ExprKind::Compare {
                        left: Box::new(name_expr(line, col, &t)),
                        ops: vec![CmpOp::NotIn],
                        comparators: vec![name_expr(line, col, &rname)],
                    },
                );
                let stmts = vec![assign(line, col, &t, v), if_stmt(line, col, test, vec![append(line, col, &rname, name_expr(line, col, &t))])];
                Some(StmtKind::Expr(self.hole(line, col, stmts, none(line, col))))
            }
            _ => None,
        }
    }

    fn call_helper(&mut self, at: &Expr, c: &ast::ExprCall, name: &str) -> WExpr {
        let mut args = Vec::new();
        for a in &c.arguments.args {
            if matches!(a, Expr::Starred(_)) {
                self.err_hint(a.start(), "starred arguments are not supported", "pass the arguments one by one");
            }
            args.push(self.expr(a));
        }
        let mut kws = Vec::new();
        for kw in &c.arguments.keywords {
            match &kw.arg {
                None => self.err_hint(kw.start(), "keyword splatting is not supported", "pass keywords by name"),
                Some(id) => {
                    let v = self.expr(&kw.value);
                    kws.push((id.id.as_str().to_string(), v, kw.start()));
                }
            }
        }
        self.expand_helper(name, at.start(), args, kws)
    }

    fn expand_helper(&mut self, name: &str, at: TextSize, args: Vec<WExpr>, kws: Vec<(String, WExpr, TextSize)>) -> WExpr {
        let (line, col) = self.loc(at);
        if self.st.stack.iter().any(|n| n == name) {
            self.err_hint(at, format!("helper `{name}` calls itself"), "recursion is not supported; rewrite it as a loop");
            return none(line, col);
        }
        if self.func.as_ref().is_some_and(|f| f.dry) {
            return none(line, col);
        }
        let Some(info) = self.st.defs.get(name).cloned() else {
            self.err_hint(at, format!("helper `{name}` is called before its `def`"), "move the `def` above the first call");
            return none(line, col);
        };
        if self.st.stack.len() >= MAX_HELPER_NESTING {
            self.err_hint(
                at,
                format!("helpers call each other more than {MAX_HELPER_NESTING} levels deep"),
                "flatten the helpers into fewer levels",
            );
            return none(line, col);
        }
        if self.st.expansions >= MAX_HELPER_EXPANSIONS {
            self.err_hint(
                at,
                format!("more than {MAX_HELPER_EXPANSIONS} helper calls after inlining"),
                "call the helper from inside a loop instead of writing out many calls",
            );
            return none(line, col);
        }
        self.st.expansions += 1;
        let (stmts, ret) = self.inline(name, &info, Some((args, kws, at)));
        self.hole(line, col, stmts, name_expr(line, col, &ret))
    }

    /// Inline a helper body. With `call == None` the body is only checked.
    fn inline(&mut self, name: &str, info: &DefInfo, call: Option<CallArgs>) -> (Vec<WStmt>, String) {
        let id = self.fresh();
        let parsed = parse_module(&info.text).expect("a registered helper parses");
        let Some(Stmt::FunctionDef(fd)) = parsed.syntax().body.first() else {
            unreachable!("a registered helper is a def");
        };
        let params = params_of(&fd.parameters);
        let pnames: Vec<String> = params.iter().map(|(p, ..)| format!("{HIDDEN_PREFIX}v{id}_{}", p.parameter.name.id)).collect();
        let mut map: HashMap<String, String> = params
            .iter()
            .zip(&pnames)
            .map(|((p, ..), v)| (p.parameter.name.id.as_str().to_string(), v.clone()))
            .collect();
        let mut locals = Vec::new();
        collect_locals(&fd.body, &mut locals);
        for l in locals {
            map.entry(l.clone()).or_insert_with(|| format!("{HIDDEN_PREFIX}v{id}_{l}"));
        }
        let mut stmts = Vec::new();
        let mut bound = vec![false; params.len()];
        let dry = call.is_none();
        let mut call_errors: Vec<(TextSize, String, String)> = Vec::new();
        let signature = params.iter().map(|(p, ..)| p.parameter.name.id.as_str()).collect::<Vec<_>>().join(", ");
        let at = call.as_ref().map(|c| c.2);
        if let Some((args, kws, at)) = call {
            let npos = params.iter().filter(|p| p.1).count();
            if args.len() > npos {
                call_errors.push((
                    at,
                    format!("`{name}()` takes {npos} positional argument(s) but {} were given", args.len()),
                    format!("call it as `{name}({signature})`"),
                ));
            }
            for (i, a) in args.into_iter().enumerate().take(npos) {
                stmts.push(assign(a.line, a.col, &pnames[i], a));
                bound[i] = true;
            }
            for (k, v, kat) in kws {
                match params.iter().position(|(p, _, kw_ok)| *kw_ok && p.parameter.name.id.as_str() == k) {
                    Some(i) if !bound[i] => {
                        stmts.push(assign(v.line, v.col, &pnames[i], v));
                        bound[i] = true;
                    }
                    Some(_) => call_errors.push((kat, format!("`{name}()` got two values for `{k}`"), format!("call it as `{name}({signature})`"))),
                    None => call_errors.push((kat, format!("`{name}()` has no parameter `{k}`"), format!("call it as `{name}({signature})`"))),
                }
            }
        }
        let brk = format!("{HIDDEN_PREFIX}rbrk{id}");
        let cont = format!("{HIDDEN_PREFIX}rcont{id}");
        let ret = format!("{HIDDEN_PREFIX}ret{id}");
        let plain_end = count_returns(&fd.body) == 1 && matches!(fd.body.last(), Some(Stmt::Return(_)));
        let later = self.later_defs.clone();
        let mut sub = Lowerer {
            source: &info.text,
            lines: LineIndex::new(&info.text),
            line_offset: info.line_offset,
            errors: Vec::new(),
            loops: vec![(brk.clone(), cont.clone(), false)],
            st: &mut *self.st,
            scopes: vec![(map, false)],
            holes: Vec::new(),
            later_defs: later,
            depth: 0,
            expr_depth: self.expr_depth,
            func: Some(FuncCtx { ret: ret.clone(), plain_end, dry }),
        };
        let (fl, fc) = sub.loc(fd.start());
        let mut missing = Vec::new();
        for (i, (p, ..)) in params.iter().enumerate() {
            if bound[i] {
                continue;
            }
            match &p.default {
                Some(d) => {
                    let v = sub.expr(d);
                    stmts.push(assign(fl, fc, &pnames[i], v));
                }
                None if dry => stmts.push(assign(fl, fc, &pnames[i], none(fl, fc))),
                None => missing.push(p.parameter.name.id.as_str().to_string()),
            }
        }
        sub.st.stack.push(name.to_string());
        let mut body = sub.suite(&fd.body);
        sub.st.stack.pop();
        if sub.loops[0].2 {
            let once = Target::Name { line: fl, col: fc, name: format!("{HIDDEN_PREFIX}once{id}") };
            let iter = ex(fl, fc, ExprKind::List(vec![ex(fl, fc, ExprKind::Int(0))]));
            body = vec![st(fl, fc, desugar_jumps(fl, fc, once, iter, body, &brk, &cont))];
        }
        let body = sub.place(body);
        let errs = std::mem::take(&mut sub.errors);
        drop(sub);
        self.errors.extend(errs);
        for (at, msg, hint) in call_errors {
            self.err_hint(at, msg, hint);
        }
        if let Some(at) = at {
            for p in missing {
                self.err_hint(at, format!("`{name}()` is missing argument `{p}`"), format!("call it as `{name}({signature})`"));
            }
        }
        stmts.push(assign(fl, fc, &ret, none(fl, fc)));
        stmts.extend(body);
        (stmts, ret)
    }

    /// `return` inside a helper: set the result, then the break flag of every
    /// enclosing loop, innermost last so the loop desugaring sees the jump.
    pub(super) fn lower_return(&mut self, stmt: &ast::StmtReturn, is_last: bool) -> Vec<WStmt> {
        let (line, col) = self.loc(stmt.start());
        let value = match &stmt.value {
            Some(v) => self.expr(v),
            None => none(line, col),
        };
        let f = self.func.as_ref().expect("return inside a helper");
        let plain = f.plain_end && self.depth == 1 && is_last;
        let mut out = vec![assign(line, col, &f.ret.clone(), value)];
        if !plain {
            for l in self.loops.iter_mut() {
                l.2 = true;
            }
            let flags: Vec<String> = self.loops.iter().map(|l| l.0.clone()).collect();
            for flag in flags {
                out.push(st(line, col, assign_flag(line, col, &flag, true)));
            }
        }
        out
    }

    /// Register a top-level `def` and check its body once.
    pub(super) fn define(&mut self, stmt: &Stmt, fd: &ast::StmtFunctionDef) -> StmtKind {
        if self.func.is_some() || self.depth > 0 {
            self.err_hint(
                stmt.start(),
                "`def` is only supported at the top level of a script",
                "move the helper to the top level, above its first call",
            );
            return StmtKind::Pass;
        }
        if fd.is_async {
            self.err_hint(stmt.start(), "`async def` is not supported", "use a plain `def`; Weave runs independent calls in parallel itself");
            return StmtKind::Pass;
        }
        if !fd.decorator_list.is_empty() {
            self.err_hint(stmt.start(), "decorators are not supported", "remove the decorator");
            return StmtKind::Pass;
        }
        let name = fd.name.id.as_str();
        if RESERVED.contains(&name) || crate::analysis::is_builtin(name) || name.starts_with(HIDDEN_PREFIX) {
            self.err_hint(stmt.start(), format!("`{name}` is a builtin name"), "give the helper another name");
            return StmtKind::Pass;
        }
        if self.st.defs.contains_key(name) {
            self.err_hint(stmt.start(), format!("helper `{name}` is defined twice"), "give each helper its own name");
            return StmtKind::Pass;
        }
        let p = &fd.parameters;
        if p.vararg.is_some() || p.kwarg.is_some() {
            self.err_hint(stmt.start(), "`*args` and `**kwargs` are not supported", "list each parameter by name");
            return StmtKind::Pass;
        }
        let mut bad_default = None;
        for (pd, ..) in params_of(p) {
            if let Some(d) = &pd.default {
                if !is_literal(d) && bad_default.is_none() {
                    bad_default = Some(d.start());
                }
            }
        }
        if let Some(at) = bad_default {
            self.err_hint(at, "helper defaults must be literals", "use `None` as the default and set the value inside the helper");
            return StmtKind::Pass;
        }
        let mut params: HashSet<String> = params_of(p).iter().map(|(x, ..)| x.parameter.name.id.as_str().to_string()).collect();
        let mut muts = Vec::new();
        param_mutations(&fd.body, &mut params, &mut muts);
        if let Some((at, pname)) = muts.into_iter().next() {
            self.err_hint(
                at,
                format!("helper `{name}` changes its argument `{pname}` in place"),
                format!("build a new value and return it, e.g. `{pname} = {pname} + [x]` then `return {pname}`"),
            );
            return StmtKind::Pass;
        }
        let text = self.source[stmt.range()].to_string();
        let (line, _) = self.loc(stmt.start());
        self.later_defs.remove(name);
        let info = DefInfo { text, line_offset: line - 1 };
        self.st.defs.insert(name.to_string(), info.clone());
        self.inline(name, &info, None);
        StmtKind::Pass
    }
}
