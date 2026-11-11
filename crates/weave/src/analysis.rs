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
            | "__weave_by_key"
            | "__weave_unique"
            | "__weave_dict"
            | "round"
            | "__weave_isinstance"
            | "__weave_format"
            | "__weave_json_dumps"
            | "__weave_merge"
            | "__weave_without"
            | "__weave_raise"
            | "__weave_dict_from"
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

