//! Placement of lowered Python sugar.
//!
//! The lowerer turns a comprehension, a `key=` call or a helper call into a
//! *hole*: a placeholder expression plus the statements that compute it. This
//! pass moves those statements in front of the statement that holds the hole,
//! in Python evaluation order: siblings evaluated before the hole are first
//! saved in temporaries, so `f(a(), [b(x) for x in xs])` still calls `a`
//! before any `b`. Holes in a branch that Python may skip (`x if c else
//! [..]`, `a and any(..)`) become an `if` statement, so the work runs only
//! when Python would run it.
//!
//! The output is ordinary Weave AST, so effect analysis, dependency
//! scheduling, replay and limits treat it like code the model wrote.

use crate::ast::{Expr, ExprKind, FSeg, Handler, Stmt, StmtKind, Target, UnaryOp};
use crate::error::Diagnostic;
use crate::parse::HIDDEN_PREFIX;

pub(crate) const HOLE: &str = "__weave_hole_";

pub(crate) type Holes = Vec<Option<(Vec<Stmt>, Expr)>>;

pub(crate) struct Anf<'a> {
    pub holes: &'a mut Holes,
    pub fresh: &'a mut usize,
    pub errors: &'a mut Vec<Diagnostic>,
}

pub(crate) fn hole_expr(line: usize, col: usize, id: usize) -> Expr {
    Expr {
        line,
        col,
        kind: ExprKind::Call {
            func: Box::new(Expr { line, col, kind: ExprKind::Name(format!("{HOLE}{id}")) }),
            args: Vec::new(),
            keywords: Vec::new(),
        },
    }
}

fn hole_id(e: &Expr) -> Option<usize> {
    if let ExprKind::Call { func, args, keywords } = &e.kind {
        if args.is_empty() && keywords.is_empty() {
            if let ExprKind::Name(n) = &func.kind {
                return n.strip_prefix(HOLE)?.parse().ok();
            }
        }
    }
    None
}

pub(crate) fn has_hole(e: &Expr) -> bool {
    if hole_id(e).is_some() {
        return true;
    }
    match &e.kind {
        ExprKind::None
        | ExprKind::Bool(_)
        | ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Name(_) => false,
        ExprKind::List(xs) | ExprKind::Tuple(xs) => xs.iter().any(has_hole),
        ExprKind::Dict(items) => items.iter().any(|(k, v)| has_hole(k) || has_hole(v)),
        ExprKind::BinOp { left, right, .. } => has_hole(left) || has_hole(right),
        ExprKind::Unary { operand, .. } => has_hole(operand),
        ExprKind::BoolOp { values, .. } => values.iter().any(has_hole),
        ExprKind::Compare { left, comparators, .. } => has_hole(left) || comparators.iter().any(has_hole),
        ExprKind::Call { func, args, keywords } => {
            has_hole(func) || args.iter().any(has_hole) || keywords.iter().any(|k| has_hole(&k.value))
        }
        ExprKind::Attribute { value, .. } => has_hole(value),
        ExprKind::Subscript { value, index } => has_hole(value) || has_hole(index),
        ExprKind::Slice { lower, upper, step } => [lower, upper, step]
            .iter()
            .any(|x| x.as_ref().map(|x| has_hole(x)).unwrap_or(false)),
        ExprKind::FString(segs) => segs.iter().any(|s| matches!(s, FSeg::Interp { expr, .. } if has_hole(expr))),
        ExprKind::IfExp { test, body, orelse } => has_hole(test) || has_hole(body) || has_hole(orelse),
    }
}

fn target_has_hole(t: &Target) -> bool {
    match t {
        Target::Name { .. } => false,
        Target::Subscript { base, index, .. } => has_hole(base) || has_hole(index),
        Target::Unpack { targets, .. } => targets.iter().any(target_has_hole),
    }
}

fn assign(line: usize, col: usize, name: &str, value: Expr) -> Stmt {
    Stmt {
        line,
        col,
        kind: StmtKind::Assign { targets: vec![Target::Name { line, col, name: name.to_string() }], value },
    }
}

fn name(line: usize, col: usize, n: &str) -> Expr {
    Expr { line, col, kind: ExprKind::Name(n.to_string()) }
}

/// Literals and names are left in place; anything else evaluated before a
/// hole is saved first. Names stay put so a helper that appends to a global
/// list is seen the way Python sees it.
fn stable(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::None | ExprKind::Bool(_) | ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Name(_)
    )
}

impl Anf<'_> {
    fn tmp(&mut self) -> String {
        *self.fresh += 1;
        format!("{HIDDEN_PREFIX}t{}", self.fresh)
    }

    pub fn block(&mut self, stmts: Vec<Stmt>) -> Vec<Stmt> {
        let mut out = Vec::new();
        for s in stmts {
            self.stmt(s, &mut out);
        }
        out
    }

    fn stmt(&mut self, s: Stmt, out: &mut Vec<Stmt>) {
        let (line, col) = (s.line, s.col);
        let kind = match s.kind {
            StmtKind::Assign { targets, value } => {
                if targets.iter().any(target_has_hole) {
                    self.target_err(line, col);
                }
                StmtKind::Assign { targets, value: self.expr(value, out) }
            }
            StmtKind::AugAssign { target, op, value } => {
                if target_has_hole(&target) {
                    self.target_err(line, col);
                }
                StmtKind::AugAssign { target, op, value: self.expr(value, out) }
            }
            StmtKind::Expr(e) => StmtKind::Expr(self.expr(e, out)),
            StmtKind::If { test, body, orelse } => {
                let test = self.expr(test, out);
                StmtKind::If { test, body: self.block(body), orelse: self.block(orelse) }
            }
            StmtKind::For { target, iter, body } => {
                if target_has_hole(&target) {
                    self.target_err(line, col);
                }
                let iter = self.expr(iter, out);
                StmtKind::For { target, iter, body: self.block(body) }
            }
            StmtKind::Try { body, handler } => {
                let body = self.block(body);
                let hbody = self.block(handler.body);
                StmtKind::Try { body, handler: Handler { body: hbody, ..handler } }
            }
            StmtKind::Pass => StmtKind::Pass,
        };
        out.push(Stmt { line, col, kind });
    }

    fn target_err(&mut self, line: usize, col: usize) {
        self.errors.push(
            Diagnostic::static_err(line, col, "a comprehension, `key=` call or helper call cannot appear inside an assignment target")
                .with_hint("compute it on its own line first, e.g. `k = max(xs, key=f)` then `d[k] = v`"),
        );
    }

    /// Evaluate `xs` left to right, hoisting what comes before the last hole.
    fn seq(&mut self, xs: Vec<Expr>, pre: &mut Vec<Stmt>) -> Vec<Expr> {
        let Some(last) = xs.iter().rposition(has_hole) else {
            return xs;
        };
        let mut out = Vec::with_capacity(xs.len());
        for (i, x) in xs.into_iter().enumerate() {
            if i < last {
                let x = self.expr(x, pre);
                if stable(&x) {
                    out.push(x);
                } else {
                    let t = self.tmp();
                    let (l, c) = (x.line, x.col);
                    pre.push(assign(l, c, &t, x));
                    out.push(name(l, c, &t));
                }
            } else if i == last {
                out.push(self.expr(x, pre));
            } else {
                out.push(x);
            }
        }
        out
    }

    pub fn expr(&mut self, e: Expr, pre: &mut Vec<Stmt>) -> Expr {
        if !has_hole(&e) {
            return e;
        }
        if let Some(id) = hole_id(&e) {
            let (stmts, result) = self.holes[id].take().expect("each hole is placed once");
            let placed = self.block(stmts);
            pre.extend(placed);
            return self.expr(result, pre);
        }
        let (line, col) = (e.line, e.col);
        let kind = match e.kind {
            ExprKind::List(xs) => ExprKind::List(self.seq(xs, pre)),
            ExprKind::Tuple(xs) => ExprKind::Tuple(self.seq(xs, pre)),
            ExprKind::Dict(items) => {
                let flat: Vec<Expr> = items.into_iter().flat_map(|(k, v)| [k, v]).collect();
                let mut it = self.seq(flat, pre).into_iter();
                let mut items = Vec::new();
                while let (Some(k), Some(v)) = (it.next(), it.next()) {
                    items.push((k, v));
                }
                ExprKind::Dict(items)
            }
            ExprKind::BinOp { left, op, right } => {
                let mut v = self.seq(vec![*left, *right], pre).into_iter();
                ExprKind::BinOp { left: Box::new(v.next().unwrap()), op, right: Box::new(v.next().unwrap()) }
            }
            ExprKind::Unary { op, operand } => ExprKind::Unary { op, operand: Box::new(self.expr(*operand, pre)) },
            ExprKind::Compare { left, ops, comparators } => {
                if comparators.iter().skip(1).any(has_hole) {
                    self.errors.push(
                        Diagnostic::static_err(line, col, "a comprehension or helper call in a chained comparison is not supported")
                            .with_hint("split it: `a < b and b < c`, or compute the value on its own line"),
                    );
                }
                let mut all = vec![*left];
                let n = comparators.len();
                let mut rest = comparators.into_iter();
                if let Some(first) = rest.next() {
                    all.push(first);
                }
                let mut v = self.seq(all, pre).into_iter();
                let left = v.next().unwrap();
                let mut comparators: Vec<Expr> = v.collect();
                comparators.extend(rest);
                debug_assert_eq!(comparators.len(), n);
                ExprKind::Compare { left: Box::new(left), ops, comparators }
            }
            ExprKind::Call { func, args, keywords } => {
                // The receiver of a method call is the evaluated child; the
                // method name stays attached to it.
                let mut items = Vec::new();
                let method = match func.kind {
                    ExprKind::Attribute { value, attr } => {
                        items.push(*value);
                        Some((attr, func.line, func.col))
                    }
                    kind => {
                        if has_hole(&Expr { line: func.line, col: func.col, kind: kind.clone() }) {
                            self.errors.push(Diagnostic::static_err(line, col, "only tool names, builtins, and methods can be called"));
                        }
                        items.push(Expr { line: func.line, col: func.col, kind });
                        None
                    }
                };
                let nargs = args.len();
                items.extend(args);
                let kw_meta: Vec<(usize, Option<String>)> = keywords.iter().map(|k| (k.line, k.name.clone())).collect();
                items.extend(keywords.into_iter().map(|k| k.value));
                let mut v = self.seq(items, pre).into_iter();
                let head = v.next().unwrap();
                let func = match method {
                    Some((attr, fl, fc)) => Expr { line: fl, col: fc, kind: ExprKind::Attribute { value: Box::new(head), attr } },
                    None => head,
                };
                let args: Vec<Expr> = v.by_ref().take(nargs).collect();
                let keywords = kw_meta
                    .into_iter()
                    .zip(v)
                    .map(|((kline, name), value)| crate::ast::Keyword { line: kline, name, value })
                    .collect();
                ExprKind::Call { func: Box::new(func), args, keywords }
            }
            ExprKind::Attribute { value, attr } => ExprKind::Attribute { value: Box::new(self.expr(*value, pre)), attr },
            ExprKind::Subscript { value, index } => {
                let mut v = self.seq(vec![*value, *index], pre).into_iter();
                ExprKind::Subscript { value: Box::new(v.next().unwrap()), index: Box::new(v.next().unwrap()) }
            }
            ExprKind::Slice { lower, upper, step } => {
                let present = [lower.is_some(), upper.is_some(), step.is_some()];
                let xs: Vec<Expr> = [lower, upper, step].into_iter().flatten().map(|b| *b).collect();
                let mut v = self.seq(xs, pre).into_iter();
                let mut take = |p: bool| if p { v.next().map(Box::new) } else { None };
                let (lower, upper, step) = (take(present[0]), take(present[1]), take(present[2]));
                ExprKind::Slice { lower, upper, step }
            }
            ExprKind::FString(segs) => {
                let exprs: Vec<Expr> = segs
                    .iter()
                    .filter_map(|s| match s {
                        FSeg::Interp { expr, .. } => Some(expr.clone()),
                        FSeg::Lit(_) => None,
                    })
                    .collect();
                let mut v = self.seq(exprs, pre).into_iter();
                ExprKind::FString(
                    segs.into_iter()
                        .map(|s| match s {
                            FSeg::Interp { conversion, debug, .. } => FSeg::Interp { expr: v.next().unwrap(), conversion, debug },
                            lit => lit,
                        })
                        .collect(),
                )
            }
            ExprKind::IfExp { test, body, orelse } => {
                if !has_hole(&body) && !has_hole(&orelse) {
                    ExprKind::IfExp { test: Box::new(self.expr(*test, pre)), body, orelse }
                } else {
                    let test = self.expr(*test, pre);
                    let v = self.tmp();
                    let mut bpre = Vec::new();
                    let b = self.expr(*body, &mut bpre);
                    bpre.push(assign(line, col, &v, b));
                    let mut opre = Vec::new();
                    let o = self.expr(*orelse, &mut opre);
                    opre.push(assign(line, col, &v, o));
                    pre.push(Stmt { line, col, kind: StmtKind::If { test, body: bpre, orelse: opre } });
                    ExprKind::Name(v)
                }
            }
            ExprKind::BoolOp { op, values } => {
                if !values.iter().skip(1).any(has_hole) {
                    let mut values = values;
                    let first = values.remove(0);
                    values.insert(0, self.expr(first, pre));
                    ExprKind::BoolOp { op, values }
                } else {
                    let v = self.tmp();
                    let mut it = values.into_iter();
                    let first = self.expr(it.next().unwrap(), pre);
                    pre.push(assign(line, col, &v, first));
                    for val in it {
                        let mut inner = Vec::new();
                        let x = self.expr(val, &mut inner);
                        inner.push(assign(line, col, &v, x));
                        let test = match op {
                            crate::ast::BoolOp::And => name(line, col, &v),
                            crate::ast::BoolOp::Or => Expr {
                                line,
                                col,
                                kind: ExprKind::Unary { op: UnaryOp::Not, operand: Box::new(name(line, col, &v)) },
                            },
                        };
                        pre.push(Stmt { line, col, kind: StmtKind::If { test, body: inner, orelse: Vec::new() } });
                    }
                    ExprKind::Name(v)
                }
            }
            leaf => leaf,
        };
        Expr { line, col, kind }
    }
}
