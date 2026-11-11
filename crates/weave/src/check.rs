use std::collections::HashSet;

use crate::analysis::is_builtin;
use crate::ast::{Expr, ExprKind, Stmt, StmtKind, Target};
use crate::error::Diagnostic;
use crate::tools::ToolRegistry;

pub fn check_program(stmts: &[Stmt], tools: &ToolRegistry) -> Vec<Diagnostic> {
    let mut defined = HashSet::new();
    check_block(stmts, tools, &mut defined)
}

/// Check `stmts` against names already assigned. Updates `defined` with names
/// assigned on every path.
pub fn check_block(stmts: &[Stmt], tools: &ToolRegistry, defined: &mut HashSet<String>) -> Vec<Diagnostic> {
    let mut diags = check_block_inner(stmts, tools, defined);
    for d in &mut diags {
        d.message = crate::parse::unhide(&d.message);
    }
    diags
}

fn check_block_inner(stmts: &[Stmt], tools: &ToolRegistry, defined: &mut HashSet<String>) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Assign { targets, value } => {
                diags.extend(check_expr(value, tools, defined));
                for target in targets {
                    diags.extend(check_target(target, tools, defined, false));
                    bind_target(target, defined);
                }
            }
            StmtKind::AugAssign { target, op, value } => {
                diags.extend(check_target(target, tools, defined, true));
                diags.extend(check_expr(value, tools, defined));
                bind_target(target, defined);
                let _ = op;
            }
            StmtKind::Expr(expr) => diags.extend(check_expr(expr, tools, defined)),
            StmtKind::If { test, body, orelse } => {
                diags.extend(check_expr(test, tools, defined));
                let mut then_defined = defined.clone();
                let mut else_defined = defined.clone();
                diags.extend(check_block_inner(body, tools, &mut then_defined));
                diags.extend(check_block_inner(orelse, tools, &mut else_defined));
                // `if True:` (how loops with break/continue are desugared)
                // always takes its body.
                // A branch that ends in `break`/`continue`/`return`/`raise`
                // never reaches the code after the `if` (the lowering guards
                // that code with the jump flags), so only the other branch's
                // assignments count, as in Python.
                *defined = if matches!(test.kind, ExprKind::Bool(true)) {
                    then_defined
                } else {
                    match (diverges(body), diverges(orelse)) {
                        (true, false) => else_defined,
                        (false, true) => then_defined,
                        (true, true) => then_defined.union(&else_defined).cloned().collect(),
                        (false, false) => then_defined.intersection(&else_defined).cloned().collect(),
                    }
                };
            }
            StmtKind::For { target, iter, body } => {
                diags.extend(check_expr(iter, tools, defined));
                let mut inner = defined.clone();
                diags.extend(check_target(target, tools, &inner, false));
                bind_target(target, &mut inner);
                diags.extend(check_block_inner(body, tools, &mut inner));
                // As in Python, a name assigned in the loop (the target, or
                // `r = ...` in the body) may be used after it; it is unbound
                // only if no iteration assigned it, which is a runtime error.
                // Scripts commonly read the last item after `break`.
                defined.extend(inner);
                defined.extend(crate::analysis::assigned_names(body));
            }
            StmtKind::Try { body, handler } => {
                let mut try_defined = defined.clone();
                diags.extend(check_block_inner(body, tools, &mut try_defined));
                let mut exc_defined = defined.clone();
                if let Some(name) = &handler.name {
                    exc_defined.insert(name.clone());
                }
                diags.extend(check_block_inner(&handler.body, tools, &mut exc_defined));
                *defined = if diverges(&handler.body) {
                    try_defined
                } else {
                    try_defined.intersection(&exc_defined).cloned().collect()
                };
            }
            StmtKind::Pass => {}
        }
    }
    diags
}

/// Whether every path through `stmts` ends in a jump: a lowered
/// `break`/`continue`/helper `return` (an assignment of `True` to a hidden
/// jump flag) or a `raise`.
fn diverges(stmts: &[Stmt]) -> bool {
    let Some(last) = stmts.last() else { return false };
    match &last.kind {
        StmtKind::Assign { targets, value } => {
            matches!(value.kind, ExprKind::Bool(true))
                && matches!(targets.as_slice(), [Target::Name { name, .. }]
                    if name.starts_with(crate::parse::HIDDEN_PREFIX) && (name.contains("brk") || name.contains("cont")))
        }
        StmtKind::Expr(e) => matches!(&e.kind, ExprKind::Call { func, .. }
            if matches!(&func.kind, ExprKind::Name(n) if n == "__weave_raise")),
        StmtKind::If { test, body, orelse } => {
            if matches!(test.kind, ExprKind::Bool(true)) {
                diverges(body)
            } else {
                diverges(body) && diverges(orelse)
            }
        }
        _ => false,
    }
}

fn check_target(target: &Target, tools: &ToolRegistry, defined: &HashSet<String>, aug: bool) -> Vec<Diagnostic> {
    match target {
        Target::Name { line, col, name } => {
            if aug && !defined.contains(name) {
                vec![Diagnostic::static_err(
                    *line,
                    *col,
                    format!("name `{name}` is used before assignment"),
                )]
            } else {
                Vec::new()
            }
        }
        Target::Subscript { line, col, base, index } => {
            let mut diags = check_expr(base, tools, defined);
            diags.extend(check_expr(index, tools, defined));
            if !matches!(base.kind, ExprKind::Name(_)) {
                diags.push(Diagnostic::static_err(
                    *line,
                    *col,
                    "subscript assignment only supports a name, like `items[i] = value`",
                ));
            }
            diags
        }
        Target::Unpack { targets, .. } => {
            let mut diags = Vec::new();
            for target in targets {
                diags.extend(check_target(target, tools, defined, aug));
            }
            diags
        }
    }
}

fn bind_target(target: &Target, defined: &mut HashSet<String>) {
    match target {
        Target::Name { name, .. } => {
            defined.insert(name.clone());
        }
        Target::Subscript { base, .. } => {
            if let ExprKind::Name(name) = &base.kind {
                defined.insert(name.clone());
            }
        }
        Target::Unpack { targets, .. } => {
            for target in targets {
                bind_target(target, defined);
            }
        }
    }
}

fn check_expr(expr: &Expr, tools: &ToolRegistry, defined: &HashSet<String>) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    match &expr.kind {
        ExprKind::Name(name) => {
                if !defined.contains(name) && !is_builtin(name) {
                diags.push(Diagnostic::static_err(
                    expr.line,
                    expr.col,
                    format!("name `{name}` is used before assignment"),
                ).with_hint(format!("assign `{name}` on an earlier line, on every branch that reaches here")));
            }
        }
        ExprKind::Call { func, args, keywords } => {
            diags.extend(check_call(expr.line, expr.col, func, args, keywords, tools, defined));
        }
        ExprKind::Attribute { value, .. } => diags.extend(check_expr(value, tools, defined)),
        ExprKind::List(xs) | ExprKind::Tuple(xs) => {
            for item in xs {
                diags.extend(check_expr(item, tools, defined));
            }
        }
        ExprKind::Dict(items) => {
            for (key, value) in items {
                diags.extend(check_expr(key, tools, defined));
                diags.extend(check_expr(value, tools, defined));
            }
        }
        ExprKind::BinOp { left, right, .. } => {
            diags.extend(check_expr(left, tools, defined));
            diags.extend(check_expr(right, tools, defined));
        }
        ExprKind::Unary { operand, .. } => diags.extend(check_expr(operand, tools, defined)),
        ExprKind::BoolOp { values, .. } => {
            for value in values {
                diags.extend(check_expr(value, tools, defined));
            }
        }
        ExprKind::Compare { left, comparators, .. } => {
            diags.extend(check_expr(left, tools, defined));
            for item in comparators {
                diags.extend(check_expr(item, tools, defined));
            }
        }
        ExprKind::Subscript { value, index } => {
            diags.extend(check_expr(value, tools, defined));
            diags.extend(check_expr(index, tools, defined));
        }
        ExprKind::Slice { lower, upper, step } => {
            for part in [lower, upper, step].into_iter().flatten() {
                diags.extend(check_expr(part, tools, defined));
            }
        }
        ExprKind::FString(segs) => {
            for seg in segs {
                if let crate::ast::FSeg::Interp { expr, .. } = seg {
                    diags.extend(check_expr(expr, tools, defined));
                }
            }
        }
        ExprKind::IfExp { test, body, orelse } => {
            diags.extend(check_expr(test, tools, defined));
            diags.extend(check_expr(body, tools, defined));
            diags.extend(check_expr(orelse, tools, defined));
        }
        ExprKind::None | ExprKind::Bool(_) | ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) => {}
    }
    diags
}

fn check_call(
    line: usize,
    col: usize,
    func: &Expr,
    args: &[Expr],
    keywords: &[crate::ast::Keyword],
    tools: &ToolRegistry,
    defined: &HashSet<String>,
) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    for arg in args {
        diags.extend(check_expr(arg, tools, defined));
    }
    for kw in keywords {
        diags.extend(check_expr(&kw.value, tools, defined));
        if kw.name.is_none() {
            diags.push(Diagnostic::static_err(
                kw.line,
                1,
                "keyword splatting is not supported; pass keywords by name",
            ));
        }
    }
    match &func.kind {
        ExprKind::Name(name) => {
            if is_builtin(name) {
                diags.extend(check_builtin(line, col, name, args.len(), keywords));
            } else if let Some(tool) = tools.get(name) {
                if let Some(arity) = tool.spec.arity {
                    let count = args.len() + if tool.spec.accepts_kwargs { keywords.len() } else { 0 };
                    if count != arity {
                        diags.push(Diagnostic::static_err(
                            line,
                            col,
                            format!("tool `{name}` takes {arity} argument(s), got {count}"),
                        ).with_hint(format!("pass {arity} positional argument(s) to `{name}`")));
                    }
                }
                if !keywords.is_empty() && !tool.spec.accepts_kwargs {
                    diags.push(Diagnostic::static_err(
                        line,
                        col,
                        format!("tool `{name}` does not take keyword arguments"),
                    ));
                }
            } else if defined.contains(name) {
                diags.push(Diagnostic::static_err(
                    line,
                    col,
                    format!("`{name}` is a value, not a tool; call a registered tool by its name"),
                ));
            } else {
                let hint = match suggest(name, tools) {
                    _ if name == "type" => "compare types with `isinstance(x, dict)` (or `int`, `str`, `list`, ...)".into(),
                    Some(suggestion) => format!("did you mean `{suggestion}`?"),
                    None => "register the tool on the engine before running the script".into(),
                };
                diags.push(
                    Diagnostic::static_err(line, col, format!("unknown tool `{name}`")).with_hint(hint),
                );
            }
        }
        ExprKind::Attribute { value, attr } => match &value.kind {
            // `functions.get_order(...)`: models trained on JSON tool calling often prefix a
            // namespace. Point at the bare name instead of reporting an unassigned `functions`.
            ExprKind::Name(ns) if !defined.contains(ns) && !is_builtin(ns) && tools.get(attr).is_some() => {
                diags.push(
                    Diagnostic::static_err(line, col, format!("`{ns}.{attr}` is not defined; tools are plain names"))
                        .with_hint(format!("call `{attr}(...)` without the `{ns}.` prefix")),
                );
            }
            _ => diags.extend(check_expr(value, tools, defined)),
        },
        _ => {
            diags.push(Diagnostic::static_err(
                line,
                col,
                "only tool names, builtins, and methods can be called",
            ));
        }
    }
    diags
}

fn check_builtin(line: usize, col: usize, name: &str, argc: usize, keywords: &[crate::ast::Keyword]) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let ok = match name {
        "print" => true,
        "len" | "str" | "int" | "float" | "bool" | "abs" | "sum" | "sorted" | "reversed"
        | "enumerate" | "any" | "all" | "list" | "parse_json" => argc == 1,
        "min" | "max" => argc == 1 || argc == 2,
        "zip" => argc == 2,
        "range" => (1..=3).contains(&argc),
        "round" => argc == 1 || argc == 2,
        _ => true,
    };
    if !ok {
        diags.push(Diagnostic::static_err(
            line,
            col,
            format!("builtin `{name}` got {argc} argument(s)"),
        ).with_hint(format!("check the argument count for `{name}`")));
    }
    if name != "print" && !keywords.is_empty() {
        let err = Diagnostic::static_err(line, col, format!("builtin `{name}` does not take keyword arguments"));
        let by_key = keywords.iter().any(|kw| kw.name.as_deref() == Some("key"));
        diags.push(match name {
            "max" | "min" if by_key => err.with_hint(format!(
                "`key=` is not supported; loop and keep the best item, e.g. `if best is None or v {} best_v: best, best_v = x, v`",
                if name == "max" { ">" } else { "<" }
            )),
            "sorted" if by_key => err.with_hint(
                "`key=` is not supported; build a list of [key, item] pairs in a loop and sort that",
            ),
            _ => err,
        });
    }
    diags
}

fn suggest(name: &str, tools: &ToolRegistry) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for cand in tools.names() {
        let dist = edit_distance(name, cand);
        if dist > 0 && dist <= 2 && best.as_ref().map(|(d, _)| dist < *d).unwrap_or(true) {
            best = Some((dist, cand.to_string()));
        }
    }
    best.map(|(_, name)| name)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len() > 32 || b.len() > 32 {
        return usize::MAX;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}
