use std::cmp;
use std::collections::HashSet;

use crate::ast::{Expr, ExprKind, Stmt, StmtKind, Target};
use crate::effect::Effect;
use crate::tools::ToolRegistry;

#[derive(Clone, Copy, Debug)]
pub struct Effects {
    pub max: Effect,
    pub has_print: bool,
}

impl Effects {
    fn none() -> Self {
        Self {
            max: Effect::Pure,
            has_print: false,
        }
    }

    pub(crate) fn union(self, other: Self) -> Self {
        Self {
            max: cmp::max(self.max, other.max),
            has_print: self.has_print || other.has_print,
        }
    }
}

pub fn block_effects(stmts: &[Stmt], tools: &ToolRegistry) -> Effects {
    stmts.iter().fold(Effects::none(), |acc, stmt| acc.union(stmt_effects(stmt, tools)))
}

fn stmt_effects(stmt: &Stmt, tools: &ToolRegistry) -> Effects {
    match &stmt.kind {
        StmtKind::Assign { value, .. } => expr_effects(value, tools),
        StmtKind::AugAssign { value, .. } => expr_effects(value, tools),
        StmtKind::Expr(expr) => expr_effects(expr, tools),
        StmtKind::If { test, body, orelse } => expr_effects(test, tools)
            .union(block_effects(body, tools))
            .union(block_effects(orelse, tools)),
        StmtKind::For { iter, body, .. } => expr_effects(iter, tools).union(block_effects(body, tools)),
        StmtKind::Try { body, handler } => block_effects(body, tools).union(block_effects(&handler.body, tools)),
        StmtKind::Pass => Effects::none(),
    }
}

fn expr_effects(expr: &Expr, tools: &ToolRegistry) -> Effects {
    match &expr.kind {
        ExprKind::Call { func, args, keywords } => {
            let mut effects = call_effect(func, tools);
            effects = args.iter().fold(effects, |acc, arg| acc.union(expr_effects(arg, tools)));
            keywords.iter().fold(effects, |acc, kw| acc.union(expr_effects(&kw.value, tools)))
        }
        ExprKind::Attribute { value, .. } => expr_effects(value, tools),
        ExprKind::Name(_)
        | ExprKind::None
        | ExprKind::Bool(_)
        | ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_) => Effects::none(),
        ExprKind::List(xs) | ExprKind::Tuple(xs) => xs.iter().fold(Effects::none(), |acc, e| acc.union(expr_effects(e, tools))),
        ExprKind::Dict(items) => items.iter().fold(Effects::none(), |acc, (k, v)| {
            acc.union(expr_effects(k, tools)).union(expr_effects(v, tools))
        }),
        ExprKind::BinOp { left, right, .. } => expr_effects(left, tools).union(expr_effects(right, tools)),
        ExprKind::Unary { operand, .. } => expr_effects(operand, tools),
        ExprKind::BoolOp { values, .. } => values.iter().fold(Effects::none(), |acc, e| acc.union(expr_effects(e, tools))),
        ExprKind::Compare { left, comparators, .. } => comparators.iter().fold(expr_effects(left, tools), |acc, e| {
            acc.union(expr_effects(e, tools))
        }),
        ExprKind::Subscript { value, index } => expr_effects(value, tools).union(expr_effects(index, tools)),
        ExprKind::Slice { lower, upper, step } => [lower, upper, step].into_iter().flatten().fold(Effects::none(), |acc, e| {
            acc.union(expr_effects(e, tools))
        }),
        ExprKind::FString(segs) => segs.iter().fold(Effects::none(), |acc, seg| match seg {
            crate::ast::FSeg::Lit(_) => acc,
            crate::ast::FSeg::Interp { expr, .. } => acc.union(expr_effects(expr, tools)),
        }),
        ExprKind::IfExp { test, body, orelse } => expr_effects(test, tools)
            .union(expr_effects(body, tools))
            .union(expr_effects(orelse, tools)),
    }
}

fn call_effect(func: &Expr, tools: &ToolRegistry) -> Effects {
    match &func.kind {
        ExprKind::Name(name) if name == "print" => Effects {
            max: Effect::Pure,
            has_print: true,
        },
        ExprKind::Name(name) if is_builtin(name) => Effects::none(),
        ExprKind::Name(name) => Effects {
            max: tools.get(name).map(|tool| tool.spec.effect).unwrap_or(Effect::Irreversible),
            has_print: false,
        },
        ExprKind::Attribute { value, .. } => expr_effects(value, tools),
        _ => Effects {
            max: Effect::Irreversible,
            has_print: false,
        },
    }
}

pub fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "print"
            | "len"
            | "range"
            | "str"
            | "int"
            | "float"
            | "bool"
            | "abs"
            | "sum"
            | "min"
            | "max"
            | "sorted"
            | "reversed"
            | "enumerate"
            | "zip"
            | "any"
            | "all"
            | "list"
            | "parse_json"
    )
}

pub fn assigned_names(stmts: &[Stmt]) -> HashSet<String> {
    let mut names = HashSet::new();
    collect_assigned(stmts, &mut names);
    names
}

fn collect_assigned(stmts: &[Stmt], names: &mut HashSet<String>) {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Assign { targets, .. } => {
                for target in targets {
                    collect_target(target, names);
                }
            }
            StmtKind::AugAssign { target, .. } => collect_target(target, names),
            StmtKind::If { body, orelse, .. } => {
                collect_assigned(body, names);
                collect_assigned(orelse, names);
            }
            StmtKind::For { target, body, .. } => {
                collect_target(target, names);
                collect_assigned(body, names);
            }
            StmtKind::Try { body, handler } => {
                collect_assigned(body, names);
                if let Some(name) = &handler.name {
                    names.insert(name.clone());
                }
                collect_assigned(&handler.body, names);
            }
            StmtKind::Expr(_) | StmtKind::Pass => {}
        }
    }
}

fn collect_target(target: &Target, names: &mut HashSet<String>) {
    match target {
        Target::Name { name, .. } => {
            names.insert(name.clone());
        }
        Target::Subscript { base, .. } => {
            if let ExprKind::Name(name) = &base.kind {
                names.insert(name.clone());
            }
        }
        Target::Unpack { targets, .. } => {
            for target in targets {
                collect_target(target, names);
            }
        }
    }
}

/// True when a later iteration can observe a name written by an earlier one.
pub fn has_carried_dep(body: &[Stmt]) -> bool {
    let writes = assigned_names(body);
    let mut assigned = HashSet::new();
    reads_before_assign(body, &writes, &mut assigned)
}

fn reads_before_assign(stmts: &[Stmt], writes: &HashSet<String>, assigned: &mut HashSet<String>) -> bool {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Assign { targets, value } => {
                // `d[k] = v` reads `d` (it rebuilds the container), so a
                // container filled across iterations is loop-carried.
                if expr_reads_carried(value, writes, assigned)
                    || targets.iter().any(|t| target_reads_carried(t, writes, assigned))
                {
                    return true;
                }
                for target in targets {
                    mark_assigned(target, assigned);
                }
            }
            StmtKind::AugAssign { target, value, .. } => {
                // `n += x` reads `n` before writing it.
                let reads_self = match target {
                    Target::Name { name, .. } => writes.contains(name) && !assigned.contains(name),
                    _ => false,
                };
                if reads_self
                    || target_reads_carried(target, writes, assigned)
                    || expr_reads_carried(value, writes, assigned)
                {
                    return true;
                }
                mark_assigned(target, assigned);
            }
            StmtKind::Expr(expr) => {
                if expr_reads_carried(expr, writes, assigned) {
                    return true;
                }
            }
            StmtKind::If { test, body, orelse } => {
                if expr_reads_carried(test, writes, assigned) {
                    return true;
                }
                let mut then_assigned = assigned.clone();
                let mut else_assigned = assigned.clone();
                if reads_before_assign(body, writes, &mut then_assigned)
                    || reads_before_assign(orelse, writes, &mut else_assigned)
                {
                    return true;
                }
                *assigned = then_assigned
                    .intersection(&else_assigned)
                    .cloned()
                    .collect();
            }
            StmtKind::For { target, iter, body } => {
                if expr_reads_carried(iter, writes, assigned) {
                    return true;
                }
                let mut inner = assigned.clone();
                mark_assigned(target, &mut inner);
                if reads_before_assign(body, writes, &mut inner) {
                    return true;
                }
            }
            StmtKind::Try { body, handler } => {
                let mut try_assigned = assigned.clone();
                if reads_before_assign(body, writes, &mut try_assigned) {
                    return true;
                }
                let mut exc_assigned = assigned.clone();
                if let Some(name) = &handler.name {
                    exc_assigned.insert(name.clone());
                }
                if reads_before_assign(&handler.body, writes, &mut exc_assigned) {
                    return true;
                }
                *assigned = try_assigned.intersection(&exc_assigned).cloned().collect();
            }
            StmtKind::Pass => {}
        }
    }
    false
}

fn expr_reads_carried(expr: &Expr, writes: &HashSet<String>, assigned: &HashSet<String>) -> bool {
    match &expr.kind {
        ExprKind::Name(name) => writes.contains(name) && !assigned.contains(name),
        ExprKind::Call { func, args, keywords } => {
            expr_reads_carried(func, writes, assigned)
                || args.iter().any(|e| expr_reads_carried(e, writes, assigned))
                || keywords.iter().any(|kw| expr_reads_carried(&kw.value, writes, assigned))
        }
        ExprKind::Attribute { value, .. } | ExprKind::Unary { operand: value, .. } => {
            expr_reads_carried(value, writes, assigned)
        }
        ExprKind::List(xs) | ExprKind::Tuple(xs) | ExprKind::BoolOp { values: xs, .. } => {
            xs.iter().any(|e| expr_reads_carried(e, writes, assigned))
        }
        ExprKind::Dict(items) => items.iter().any(|(k, v)| {
            expr_reads_carried(k, writes, assigned) || expr_reads_carried(v, writes, assigned)
        }),
        ExprKind::BinOp { left, right, .. } => {
            expr_reads_carried(left, writes, assigned) || expr_reads_carried(right, writes, assigned)
        }
        ExprKind::Compare { left, comparators, .. } => {
            expr_reads_carried(left, writes, assigned)
                || comparators.iter().any(|e| expr_reads_carried(e, writes, assigned))
        }
        ExprKind::Subscript { value, index } => {
            expr_reads_carried(value, writes, assigned) || expr_reads_carried(index, writes, assigned)
        }
        ExprKind::Slice { lower, upper, step } => [lower, upper, step]
            .into_iter()
            .flatten()
            .any(|e| expr_reads_carried(e, writes, assigned)),
        ExprKind::FString(segs) => segs.iter().any(|seg| match seg {
            crate::ast::FSeg::Lit(_) => false,
            crate::ast::FSeg::Interp { expr, .. } => expr_reads_carried(expr, writes, assigned),
        }),
        ExprKind::IfExp { test, body, orelse } => {
            expr_reads_carried(test, writes, assigned)
                || expr_reads_carried(body, writes, assigned)
                || expr_reads_carried(orelse, writes, assigned)
        }
        ExprKind::None | ExprKind::Bool(_) | ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) => false,
    }
}

fn target_reads_carried(target: &Target, writes: &HashSet<String>, assigned: &HashSet<String>) -> bool {
    match target {
        Target::Name { .. } => false,
        Target::Subscript { base, index, .. } => {
            expr_reads_carried(base, writes, assigned) || expr_reads_carried(index, writes, assigned)
        }
        Target::Unpack { targets, .. } => targets.iter().any(|t| target_reads_carried(t, writes, assigned)),
    }
}

fn mark_assigned(target: &Target, assigned: &mut HashSet<String>) {
    match target {
        Target::Name { name, .. } => {
            assigned.insert(name.clone());
        }
        Target::Subscript { .. } => {}
        Target::Unpack { targets, .. } => {
            for target in targets {
                mark_assigned(target, assigned);
            }
        }
    }
}

/// Read and pure tool calls in a loop body that can be issued for every
/// iteration before the (sequential) loop starts.
///
/// A loop whose iterations depend on each other (an accumulator such as
/// `total += read(f)` or `rows.append(get(id))`) must run in order, but the
/// tool calls inside it often do not depend on that order: their arguments
/// use only the loop variable and values fixed before the loop. Those calls
/// can all start at once and each iteration then waits on its own result.
///
/// A call qualifies only when
/// - the body has no reversible or irreversible call (no write can change
///   what a read returns),
/// - the tool's effect is pure or read,
/// - it is evaluated unconditionally in every iteration: it sits in a
///   top-level statement of the body (or of a `try` body), not under `if`,
///   a nested `for`, an `except` handler, a conditional expression, or the
///   right side of `and`/`or`,
/// - its arguments contain no tool call and read no name the body assigns
///   (so they are identical whether computed before the loop or in order).
///
/// Loops with `break`/`continue` are desugared into guarded `if` bodies, so
/// none of their calls qualify and early exit never issues extra calls. The
/// only extra calls possible are reads after a statement that raises inside
/// the same `try` body or an iteration that fails the script.
pub fn prefetchable_calls<'a>(body: &'a [Stmt], tools: &ToolRegistry) -> Vec<&'a Expr> {
    if block_effects(body, tools).max > Effect::Read {
        return Vec::new();
    }
    let written = assigned_names(body);
    let mut out = Vec::new();
    collect_prefetch_stmts(body, tools, &written, &mut out);
    out
}

fn collect_prefetch_stmts<'a>(stmts: &'a [Stmt], tools: &ToolRegistry, written: &HashSet<String>, out: &mut Vec<&'a Expr>) {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Assign { value, .. } | StmtKind::AugAssign { value, .. } | StmtKind::Expr(value) => {
                collect_prefetch_expr(value, tools, written, out)
            }
            StmtKind::Try { body, .. } => collect_prefetch_stmts(body, tools, written, out),
            StmtKind::If { .. } | StmtKind::For { .. } | StmtKind::Pass => {}
        }
    }
}

fn is_prefetch_tool(func: &Expr, tools: &ToolRegistry) -> bool {
    match &func.kind {
        ExprKind::Name(name) if name != "print" && !is_builtin(name) => {
            tools.get(name).is_some_and(|tool| tool.spec.effect <= Effect::Read)
        }
        _ => false,
    }
}

fn collect_prefetch_expr<'a>(expr: &'a Expr, tools: &ToolRegistry, written: &HashSet<String>, out: &mut Vec<&'a Expr>) {
    match &expr.kind {
        ExprKind::Call { func, args, keywords } => {
            if is_prefetch_tool(func, tools) {
                if args.iter().all(|a| loop_invariant(a, written))
                    && keywords.iter().all(|k| k.name.is_some() && loop_invariant(&k.value, written))
                {
                    out.push(expr);
                }
                return;
            }
            collect_prefetch_expr(func, tools, written, out);
            for arg in args {
                collect_prefetch_expr(arg, tools, written, out);
            }
            for kw in keywords {
                collect_prefetch_expr(&kw.value, tools, written, out);
            }
        }
        ExprKind::Attribute { value, .. } | ExprKind::Unary { operand: value, .. } => {
            collect_prefetch_expr(value, tools, written, out)
        }
        ExprKind::List(xs) | ExprKind::Tuple(xs) => {
            for x in xs {
                collect_prefetch_expr(x, tools, written, out);
            }
        }
        // Only the first operand of `and`/`or` always runs.
        ExprKind::BoolOp { values, .. } => {
            if let Some(first) = values.first() {
                collect_prefetch_expr(first, tools, written, out);
            }
        }
        ExprKind::Dict(items) => {
            for (k, v) in items {
                collect_prefetch_expr(k, tools, written, out);
                collect_prefetch_expr(v, tools, written, out);
            }
        }
        ExprKind::BinOp { left, right, .. } => {
            collect_prefetch_expr(left, tools, written, out);
            collect_prefetch_expr(right, tools, written, out);
        }
        // Chained comparisons short-circuit after the first pair.
        ExprKind::Compare { left, comparators, .. } => {
            collect_prefetch_expr(left, tools, written, out);
            if let Some(first) = comparators.first() {
                collect_prefetch_expr(first, tools, written, out);
            }
        }
        ExprKind::Subscript { value, index } => {
            collect_prefetch_expr(value, tools, written, out);
            collect_prefetch_expr(index, tools, written, out);
        }
        ExprKind::FString(segs) => {
            for seg in segs {
                if let crate::ast::FSeg::Interp { expr, .. } = seg {
                    collect_prefetch_expr(expr, tools, written, out);
                }
            }
        }
        // Only the test of a conditional expression always runs.
        ExprKind::IfExp { test, .. } => collect_prefetch_expr(test, tools, written, out),
        ExprKind::Slice { .. }
        | ExprKind::Name(_)
        | ExprKind::None
        | ExprKind::Bool(_)
        | ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_) => {}
    }
}

/// True when `expr` makes no tool call and reads no name in `written`.
fn loop_invariant(expr: &Expr, written: &HashSet<String>) -> bool {
    let all = |xs: &[Expr]| xs.iter().all(|x| loop_invariant(x, written));
    match &expr.kind {
        ExprKind::Name(name) => !written.contains(name),
        ExprKind::Call { func, args, keywords } => {
            let callable = match &func.kind {
                ExprKind::Name(name) => is_builtin(name) && name != "print",
                ExprKind::Attribute { value, .. } => loop_invariant(value, written),
                _ => false,
            };
            callable && all(args) && keywords.iter().all(|k| k.name.is_some() && loop_invariant(&k.value, written))
        }
        ExprKind::Attribute { value, .. } | ExprKind::Unary { operand: value, .. } => loop_invariant(value, written),
        ExprKind::List(xs) | ExprKind::Tuple(xs) | ExprKind::BoolOp { values: xs, .. } => all(xs),
        ExprKind::Dict(items) => items.iter().all(|(k, v)| loop_invariant(k, written) && loop_invariant(v, written)),
        ExprKind::BinOp { left, right, .. } => loop_invariant(left, written) && loop_invariant(right, written),
        ExprKind::Compare { left, comparators, .. } => loop_invariant(left, written) && all(comparators),
        ExprKind::Subscript { value, index } => loop_invariant(value, written) && loop_invariant(index, written),
        ExprKind::Slice { lower, upper, step } => [lower, upper, step]
            .into_iter()
            .flatten()
            .all(|e| loop_invariant(e, written)),
        ExprKind::FString(segs) => segs.iter().all(|seg| match seg {
            crate::ast::FSeg::Lit(_) => true,
            crate::ast::FSeg::Interp { expr, .. } => loop_invariant(expr, written),
        }),
        ExprKind::IfExp { test, body, orelse } => {
            loop_invariant(test, written) && loop_invariant(body, written) && loop_invariant(orelse, written)
        }
        ExprKind::None | ExprKind::Bool(_) | ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) => true,
    }
}
