mod sugar;

use std::collections::{HashMap, HashSet};

use ruff_python_ast as ast;
use ruff_python_ast::{CmpOp, Expr, Number, Operator, Stmt, UnaryOp};
use ruff_python_parser::parse_module;
use ruff_text_size::{Ranged, TextSize};

use crate::ast::{
    BinOp, BoolOp, Conversion, Expr as WExpr, ExprKind, FSeg, Handler, Keyword, Stmt as WStmt,
    StmtKind, Target, UnaryOp as WUnary,
};
use crate::error::{Diagnostic, WeaveError};
use crate::limits::MAX_DEPTH;

pub struct ParsedProgram {
    pub stmts: Vec<WStmt>,
    pub errors: Vec<Diagnostic>,
}

/// Lowering state that outlives one chunk of a streamed script: helpers
/// defined so far and the counter for compiler-introduced names.
#[derive(Clone, Debug, Default)]
pub struct LowerState {
    defs: HashMap<String, DefInfo>,
    fresh: usize,
    expansions: usize,
    stack: Vec<String>,
}

/// A top-level `def`, kept as source so each call site re-lowers it.
#[derive(Clone, Debug)]
struct DefInfo {
    text: String,
    line_offset: usize,
}

struct FuncCtx {
    ret: String,
    plain_end: bool,
    dry: bool,
}

pub fn parse_program_with(source: &str, line_offset: usize, state: &mut LowerState) -> Result<Vec<WStmt>, WeaveError> {
    let parsed = parse_detailed_with(source, line_offset, state)?;
    if parsed.errors.is_empty() {
        Ok(parsed.stmts)
    } else {
        Err(WeaveError::new(parsed.errors))
    }
}

/// Lower the module even when individual statements are outside the subset.
/// A hard syntax failure is still an `Err`.
pub fn parse_detailed(source: &str, line_offset: usize) -> Result<ParsedProgram, WeaveError> {
    parse_detailed_with(source, line_offset, &mut LowerState::default())
}

pub fn parse_detailed_with(source: &str, line_offset: usize, state: &mut LowerState) -> Result<ParsedProgram, WeaveError> {
    if let Err((offset, message)) = crate::limits::check_source_shape(source) {
        let (line, col) = line_col(source, TextSize::new(offset as u32));
        return Err(WeaveError::static_err(line + line_offset, col, message));
    }
    let parsed = match parse_module(source) {
        Ok(parsed) => parsed,
        Err(err) => {
            let (line, col) = line_col(source, err.location.start());
            return Err(WeaveError::static_err(
                line + line_offset,
                col,
                format!("syntax error: {}", err.error),
            ));
        }
    };
    if let Some(err) = parsed.errors().first() {
        let (line, col) = line_col(source, err.location.start());
        return Err(WeaveError::static_err(
            line + line_offset,
            col,
            format!("syntax error: {}", err.error),
        ));
    }
    let later_defs = parsed
        .syntax()
        .body
        .iter()
        .filter_map(|stmt| match stmt {
            Stmt::FunctionDef(f) => Some(f.name.id.as_str().to_string()),
            _ => None,
        })
        .collect();
    let mut lower = Lowerer {
        source,
        lines: LineIndex::new(source),
        line_offset,
        errors: Vec::new(),
        loops: Vec::new(),
        st: state,
        scopes: Vec::new(),
        holes: Vec::new(),
        later_defs,
        depth: 0,
        expr_depth: 0,
        func: None,
    };
    let stmts: Vec<WStmt> = parsed.syntax().body.iter().map(|stmt| lower.stmt(stmt)).collect();
    let stmts = lower.place(stmts);
    let mut seen = HashSet::new();
    let mut errors = std::mem::take(&mut lower.errors);
    errors.retain(|d| seen.insert((d.line, d.col, d.message.clone())));
    for d in &mut errors {
        d.message = unhide(&d.message);
    }
    for stmt in &stmts {
        if stmt_depth(stmt, 1) > MAX_DEPTH {
            return Err(WeaveError::static_err(
                stmt.line,
                stmt.col,
                format!("code is nested deeper than {MAX_DEPTH} levels; split it into smaller statements"),
            ));
        }
    }
    Ok(ParsedProgram { stmts, errors })
}

/// Depth of a statement tree. Stops descending once past [`MAX_DEPTH`], so the
/// walk itself is bounded.
fn stmt_depth(stmt: &WStmt, depth: usize) -> usize {
    if depth > MAX_DEPTH {
        return depth;
    }
    let block = |stmts: &[WStmt]| stmts.iter().map(|s| stmt_depth(s, depth + 1)).max().unwrap_or(depth);
    match &stmt.kind {
        StmtKind::Assign { value, .. } | StmtKind::AugAssign { value, .. } | StmtKind::Expr(value) => {
            expr_depth(value, depth + 1)
        }
        StmtKind::If { test, body, orelse } => expr_depth(test, depth + 1).max(block(body)).max(block(orelse)),
        StmtKind::For { iter, body, .. } => expr_depth(iter, depth + 1).max(block(body)),
        StmtKind::Try { body, handler } => block(body).max(block(&handler.body)),
        StmtKind::Pass => depth,
    }
}

fn expr_depth(expr: &WExpr, depth: usize) -> usize {
    if depth > MAX_DEPTH {
        return depth;
    }
    let d = depth + 1;
    let many = |xs: &[WExpr]| xs.iter().map(|x| expr_depth(x, d)).max().unwrap_or(depth);
    match &expr.kind {
        ExprKind::None
        | ExprKind::Bool(_)
        | ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Name(_) => depth,
        ExprKind::List(xs) | ExprKind::Tuple(xs) => many(xs),
        ExprKind::Dict(items) => items
            .iter()
            .map(|(k, v)| expr_depth(k, d).max(expr_depth(v, d)))
            .max()
            .unwrap_or(depth),
        ExprKind::BinOp { left, right, .. } => expr_depth(left, d).max(expr_depth(right, d)),
        ExprKind::Unary { operand, .. } => expr_depth(operand, d),
        ExprKind::BoolOp { values, .. } => many(values),
        ExprKind::Compare { left, comparators, .. } => expr_depth(left, d).max(many(comparators)),
        ExprKind::Call { func, args, keywords } => expr_depth(func, d)
            .max(many(args))
            .max(keywords.iter().map(|k| expr_depth(&k.value, d)).max().unwrap_or(depth)),
        ExprKind::Attribute { value, .. } => expr_depth(value, d),
        ExprKind::Subscript { value, index } => expr_depth(value, d).max(expr_depth(index, d)),
        ExprKind::Slice { lower, upper, step } => [lower, upper, step]
            .iter()
            .filter_map(|x| x.as_ref())
            .map(|x| expr_depth(x, d))
            .max()
            .unwrap_or(depth),
        ExprKind::FString(segs) => segs
            .iter()
            .map(|seg| match seg {
                FSeg::Lit(_) => depth,
                FSeg::Interp { expr, .. } => expr_depth(expr, d),
            })
            .max()
            .unwrap_or(depth),
        ExprKind::IfExp { test, body, orelse } => {
            expr_depth(test, d).max(expr_depth(body, d)).max(expr_depth(orelse, d))
        }
    }
}

/// `__weave_v12_total` -> `total` in messages shown to the model.
pub fn unhide(message: &str) -> String {
    let marker = format!("{HIDDEN_PREFIX}v");
    let mut out = String::with_capacity(message.len());
    let mut rest = message;
    while let Some(i) = rest.find(&marker) {
        out.push_str(&rest[..i]);
        let after = &rest[i + marker.len()..];
        let digits = after.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 && after[digits..].starts_with('_') {
            rest = &after[digits + 1..];
        } else {
            out.push_str(&marker);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

struct Lowerer<'a, 's> {
    source: &'a str,
    lines: LineIndex,
    line_offset: usize,
    errors: Vec<Diagnostic>,
    /// Enclosing loops, innermost last: (break flag, continue flag, used).
    /// Inside a helper the first entry is the helper's own pseudo-loop.
    loops: Vec<(String, String, bool)>,
    st: &'s mut LowerState,
    /// Renamed variables, innermost last; `false` stops lookup (helper bodies
    /// see only their own locals and the globals).
    scopes: Vec<(HashMap<String, String>, bool)>,
    holes: crate::anf::Holes,
    /// Top-level helpers defined later in this chunk.
    later_defs: HashSet<String>,
    depth: usize,
    expr_depth: usize,
    func: Option<FuncCtx>,
}

impl<'a> Lowerer<'a, '_> {
    fn loc(&self, offset: TextSize) -> (usize, usize) {
        let (line, col) = self.lines.line_col(self.source, offset.to_usize());
        (line + self.line_offset, col)
    }

    fn err(&mut self, offset: TextSize, message: impl Into<String>) {
        let (line, col) = self.loc(offset);
        self.errors.push(Diagnostic::static_err(line, col, message));
    }

    fn err_hint(&mut self, offset: TextSize, message: impl Into<String>, hint: impl Into<String>) {
        let (line, col) = self.loc(offset);
        self.errors.push(Diagnostic::static_err(line, col, message).with_hint(hint));
    }

    fn stmt(&mut self, stmt: &Stmt) -> WStmt {
        let (line, col) = self.loc(stmt.start());
        let kind = match stmt {
            Stmt::Assign(s) if matches!(s.value.as_ref(), Expr::Lambda(_)) => {
                self.err_hint(
                    stmt.start(),
                    "assigning a `lambda` is not supported",
                    "write a helper instead: `def name(x): return ...`",
                );
                StmtKind::Pass
            }
            Stmt::Assign(s) => {
                let value = self.expr(&s.value);
                let targets = s.targets.iter().map(|t| self.target(t)).collect();
                StmtKind::Assign { targets, value }
            }
            Stmt::AugAssign(s) => match self.bin_op(s.op) {
                Some(op) => StmtKind::AugAssign {
                    target: self.target(&s.target),
                    op,
                    value: self.expr(&s.value),
                },
                None => {
                    self.err(
                        stmt.start(),
                        format!("operator `{}` is not supported", s.op),
                    );
                    StmtKind::Pass
                }
            },
            Stmt::AnnAssign(s) => match &s.value {
                Some(value) => StmtKind::Assign {
                    targets: vec![self.target(&s.target)],
                    value: self.expr(value),
                },
                None => {
                    self.err(
                        stmt.start(),
                        "annotated assignment needs a value; write `name: T = value` or drop the annotation",
                    );
                    StmtKind::Pass
                }
            },
            Stmt::Expr(s) => match self.sugar_method_stmt(&s.value) {
                Some(kind) => kind,
                None => desugar_list_mutation(self.expr(&s.value)),
            },
            Stmt::If(s) => self.lower_if(s),
            Stmt::For(s) => {
                if s.is_async {
                    self.err(stmt.start(), "`async for` is not supported; use a plain for loop");
                }
                if !s.orelse.is_empty() {
                    self.err(
                        stmt.start(),
                        "`for/else` is not supported; put the follow-up code after the loop",
                    );
                }
                let target = self.target(&s.target);
                let iter = self.expr(&s.iter);
                let brk = format!("{HIDDEN_PREFIX}brk_{line}_{col}");
                let cont = format!("{HIDDEN_PREFIX}cont_{line}_{col}");
                self.loops.push((brk.clone(), cont.clone(), false));
                let body = self.suite(&s.body);
                let (_, _, used) = self.loops.pop().expect("loop stack");
                if used {
                    desugar_jumps(line, col, target, iter, body, &brk, &cont)
                } else {
                    StmtKind::For { target, iter, body }
                }
            }
            Stmt::Pass(_) => StmtKind::Pass,
            Stmt::FunctionDef(fd) => self.define(stmt, fd),
            Stmt::ClassDef(_) => {
                self.err_hint(stmt.start(), "`class` is not supported", "use a dict for the record");
                StmtKind::Pass
            }
            Stmt::Return(_) => {
                self.err_hint(stmt.start(), "`return` outside a `def` helper", "assign the value instead, or print it");
                StmtKind::Pass
            }
            Stmt::Delete(_) => {
                self.err_hint(stmt.start(), "`del` is not supported", "rebind the name instead");
                StmtKind::Pass
            }
            Stmt::TypeAlias(_) => {
                self.err_hint(stmt.start(), "`type` aliases are not supported", "drop the alias and use the value directly");
                StmtKind::Pass
            }
            Stmt::While(_) => {
                self.err_hint(stmt.start(), "`while` is not supported", "use `for` over a list; for a loop of unknown length write `for _ in range(100):` and `break` when done");
                StmtKind::Pass
            }
            Stmt::With(_) => {
                self.err_hint(stmt.start(), "`with` is not supported", "call the tool directly");
                StmtKind::Pass
            }
            Stmt::Match(_) => {
                self.err_hint(stmt.start(), "`match` is not supported", "use `if`");
                StmtKind::Pass
            }
            Stmt::Raise(r) => self.lower_raise(stmt, r),
            Stmt::Try(s) => self.lower_try(s),
            Stmt::Assert(a) => {
                // `assert c, msg` is `if not c: raise AssertionError(msg)`.
                let (line, col) = self.loc(stmt.start());
                let test = self.expr(&a.test);
                let msg = match &a.msg {
                    Some(m) => self.expr(m),
                    None => WExpr { line, col, kind: ExprKind::Str(String::new()) },
                };
                let class = WExpr { line, col, kind: ExprKind::Str("AssertionError".into()) };
                let raise = WStmt { line, col, kind: StmtKind::Expr(call_expr(line, col, "__weave_raise", vec![class, msg])) };
                let not = WExpr { line, col, kind: ExprKind::Unary { op: crate::ast::UnaryOp::Not, operand: Box::new(test) } };
                StmtKind::If { test: not, body: vec![raise], orelse: Vec::new() }
            }
            Stmt::Import(_) | Stmt::ImportFrom(_) => {
                self.err_hint(
                    stmt.start(),
                    "`import` is not supported",
                    "inline values or fetch them with a tool",
                );
                StmtKind::Pass
            }
            Stmt::Global(_) | Stmt::Nonlocal(_) => {
                self.err(stmt.start(), "`global` and `nonlocal` are not supported");
                StmtKind::Pass
            }
            Stmt::Break(_) | Stmt::Continue(_) => {
                let is_break = matches!(stmt, Stmt::Break(_));
                let base = usize::from(self.func.is_some());
                let in_loop = self.loops.len() > base;
                match self.loops.last_mut().filter(|_| in_loop) {
                    Some((brk, cont, used)) => {
                        *used = true;
                        let flag = if is_break { brk.clone() } else { cont.clone() };
                        assign_flag(line, col, &flag, true)
                    }
                    None => {
                        let word = if is_break { "break" } else { "continue" };
                        self.err(stmt.start(), format!("`{word}` outside a loop"));
                        StmtKind::Pass
                    }
                }
            }
            Stmt::IpyEscapeCommand(_) => {
                self.err(stmt.start(), "IPython escapes are not supported");
                StmtKind::Pass
            }
        };
        WStmt { line, col, kind }
    }

    fn lower_try(&mut self, stmt: &ast::StmtTry) -> StmtKind {
        if stmt.is_star {
            self.err_hint(
                stmt.start(),
                "`except*` is not supported",
                EXCEPT_HINT,
            );
            return StmtKind::Pass;
        }
        if !stmt.orelse.is_empty() {
            self.err_hint(
                stmt.start(),
                "`try/else` is not supported",
                "put that code after the try",
            );
        }
        if !stmt.finalbody.is_empty() {
            self.err_hint(
                stmt.start(),
                "`finally` is not supported",
                "put cleanup after the try",
            );
        }
        if stmt.handlers.len() != 1 {
            self.err_hint(
                stmt.start(),
                "try needs exactly one except clause",
                "catch several kinds in one clause, e.g. `except (KeyError, ToolError):` or `except Exception:`",
            );
            return StmtKind::Pass;
        }
        let ast::ExceptHandler::ExceptHandler(handler) = &stmt.handlers[0];
        let mut names: Vec<(String, TextSize)> = Vec::new();
        match handler.type_.as_deref() {
            None => {}
            Some(ast::Expr::Name(name)) => names.push((name.id.as_str().to_string(), name.range.start())),
            Some(ast::Expr::Tuple(t)) => {
                for e in &t.elts {
                    match e {
                        ast::Expr::Name(name) => names.push((name.id.as_str().to_string(), name.range.start())),
                        other => {
                            self.err_hint(other.start(), "only exception class names can be caught", EXCEPT_HINT);
                            return StmtKind::Pass;
                        }
                    }
                }
            }
            Some(other) => {
                self.err_hint(other.start(), "only exception class names can be caught", EXCEPT_HINT);
                return StmtKind::Pass;
            }
        }
        for (name, at) in &names {
            if !CATCHABLE.contains(&name.as_str()) {
                self.err_hint(*at, format!("`except {name}` is not supported"), EXCEPT_HINT);
                return StmtKind::Pass;
            }
        }
        let catches_all = handler.type_.is_none()
            || names.iter().any(|(n, _)| n == "Exception" || n == "BaseException");
        let classes: Vec<String> = names.into_iter().map(|(n, _)| n).collect();
        let (line, col) = self.loc(handler.range.start());
        StmtKind::Try {
            body: self.suite(&stmt.body),
            handler: Handler {
                line,
                col,
                catches_all,
                classes,
                name: handler.name.as_ref().map(|id| self.resolve(id.id.as_str())),
                body: self.suite(&handler.body),
            },
        }
    }

    /// `raise ValueError("msg")` stops the script with a runtime error that
    /// `except ValueError` (or `Exception`) catches. A bare `raise` is not
    /// supported.
    fn lower_raise(&mut self, stmt: &Stmt, r: &ast::StmtRaise) -> StmtKind {
        let (line, col) = self.loc(stmt.start());
        let hint = "raise a builtin class with a message, e.g. `raise ValueError(\"order not found\")`";
        if r.cause.is_some() {
            self.err_hint(stmt.start(), "`raise ... from ...` is not supported", hint);
            return StmtKind::Pass;
        }
        let Some(exc) = r.exc.as_deref() else {
            self.err_hint(stmt.start(), "a bare `raise` is not supported", hint);
            return StmtKind::Pass;
        };
        let (class, msg) = match exc {
            Expr::Name(n) => (n.id.as_str().to_string(), None),
            Expr::Call(c) if matches!(c.func.as_ref(), Expr::Name(_)) && c.arguments.keywords.is_empty() && c.arguments.args.len() <= 1 => {
                let Expr::Name(n) = c.func.as_ref() else { unreachable!() };
                (n.id.as_str().to_string(), c.arguments.args.first())
            }
            _ => {
                self.err_hint(stmt.start(), "only `raise SomeError(message)` is supported", hint);
                return StmtKind::Pass;
            }
        };
        if !CATCHABLE.contains(&class.as_str()) && class != "AssertionError" {
            self.err_hint(stmt.start(), format!("`raise {class}` is not supported"), hint);
            return StmtKind::Pass;
        }
        let msg = match msg {
            Some(m) => self.expr(m),
            None => WExpr { line, col, kind: ExprKind::Str(String::new()) },
        };
        let class = WExpr { line, col, kind: ExprKind::Str(class) };
        StmtKind::Expr(call_expr(line, col, "__weave_raise", vec![class, msg]))
    }

    fn lower_if(&mut self, stmt: &ast::StmtIf) -> StmtKind {
        let mut orelse = Vec::new();
        for clause in stmt.elif_else_clauses.iter().rev() {
            let body = self.suite(&clause.body);
            if let Some(test) = &clause.test {
                let (line, col) = self.loc(clause.start());
                orelse = vec![WStmt {
                    line,
                    col,
                    kind: StmtKind::If {
                        test: self.expr(test),
                        body,
                        orelse,
                    },
                }];
            } else {
                orelse = body;
            }
        }
        StmtKind::If {
            test: self.expr(&stmt.test),
            body: self.suite(&stmt.body),
            orelse,
        }
    }

    fn suite(&mut self, stmts: &[Stmt]) -> Vec<WStmt> {
        self.depth += 1;
        let mut out = Vec::new();
        for (i, stmt) in stmts.iter().enumerate() {
            match stmt {
                Stmt::Return(r) if self.func.is_some() => out.extend(self.lower_return(r, i + 1 == stmts.len())),
                _ => out.push(self.stmt(stmt)),
            }
        }
        self.depth -= 1;
        out
    }

    fn target(&mut self, expr: &Expr) -> Target {
        let (line, col) = self.loc(expr.start());
        match expr {
            Expr::Name(name) => {
                let id = name.id.as_str();
                if self.is_helper(id) {
                    self.err_hint(
                        expr.start(),
                        format!("`{id}` is a helper defined with `def`"),
                        "use another name for this variable",
                    );
                }
                Target::Name { line, col, name: self.resolve(id) }
            }
            Expr::Subscript(sub) => {
                if matches!(sub.slice.as_ref(), Expr::Slice(_)) {
                    self.err(expr.start(), "slice assignment is not supported; build a new list");
                }
                Target::Subscript {
                    line,
                    col,
                    base: self.expr(&sub.value),
                    index: self.expr(&sub.slice),
                }
            }
            Expr::Tuple(t) => Target::Unpack {
                line,
                col,
                targets: t.elts.iter().map(|e| self.target(e)).collect(),
            },
            Expr::List(t) => Target::Unpack {
                line,
                col,
                targets: t.elts.iter().map(|e| self.target(e)).collect(),
            },
            Expr::Starred(_) => {
                self.err(expr.start(), "starred assignment is not supported");
                Target::Name {
                    line,
                    col,
                    name: "_".into(),
                }
            }
            _ => {
                self.err(expr.start(), "cannot assign to this expression; assign to a name");
                Target::Name {
                    line,
                    col,
                    name: "_".into(),
                }
            }
        }
    }

    fn expr(&mut self, expr: &Expr) -> WExpr {
        // Bound the lowering recursion itself; `stmt_depth` re-checks the result.
        if self.expr_depth > MAX_DEPTH {
            let (line, col) = self.loc(expr.start());
            if !self.errors.iter().any(|d| d.message.contains("nested deeper than")) {
                self.err(expr.start(), format!("code is nested deeper than {MAX_DEPTH} levels; split it into smaller statements"));
            }
            return WExpr { line, col, kind: ExprKind::None };
        }
        self.expr_depth += 1;
        let lowered = self.expr_inner(expr);
        self.expr_depth -= 1;
        lowered
    }

    fn expr_inner(&mut self, expr: &Expr) -> WExpr {
        if let Some(lowered) = self.sugar(expr) {
            return lowered;
        }
        let (line, col) = self.loc(expr.start());
        let kind = match expr {
            Expr::NoneLiteral(_) => ExprKind::None,
            Expr::BooleanLiteral(b) => ExprKind::Bool(b.value),
            Expr::NumberLiteral(n) => match &n.value {
                Number::Int(i) => match i.as_i64() {
                    Some(v) => ExprKind::Int(v),
                    None => {
                        self.err(expr.start(), "integer literal does not fit in i64; use a smaller literal");
                        ExprKind::None
                    }
                },
                Number::Float(f) => ExprKind::Float(*f),
                Number::Complex { .. } => {
                    self.err(expr.start(), "complex numbers are not supported");
                    ExprKind::None
                }
            },
            Expr::StringLiteral(s) => ExprKind::Str(s.value.to_str().to_string()),
            Expr::BytesLiteral(_) => {
                self.err(expr.start(), "bytes literals are not supported; use a string");
                ExprKind::None
            }
            Expr::EllipsisLiteral(_) => {
                self.err(expr.start(), "`...` is not supported");
                ExprKind::None
            }
            Expr::Name(name) => {
                let id = name.id.as_str();
                if self.is_helper(id) {
                    self.err_hint(
                        expr.start(),
                        format!("helper `{id}` can only be called"),
                        format!("call it, e.g. `{id}(x)`, or pass it as `key={id}`"),
                    );
                }
                ExprKind::Name(self.resolve(id))
            }
            Expr::List(list) => {
                if list.elts.iter().any(|e| matches!(e, Expr::Starred(_))) {
                    self.err(expr.start(), "starred list items are not supported");
                }
                ExprKind::List(list.elts.iter().map(|e| self.expr(e)).collect())
            }
            Expr::Tuple(tuple) => {
                if tuple.elts.iter().any(|e| matches!(e, Expr::Starred(_))) {
                    self.err(expr.start(), "starred tuple items are not supported");
                }
                ExprKind::Tuple(tuple.elts.iter().map(|e| self.expr(e)).collect())
            }
            Expr::Dict(dict) => {
                let mut items = Vec::new();
                for item in &dict.items {
                    let Some(key) = &item.key else {
                        self.err(expr.start(), "dict unpacking is not supported; write the keys out");
                        continue;
                    };
                    items.push((self.expr(key), self.expr(&item.value)));
                }
                ExprKind::Dict(items)
            }
            Expr::BinOp(op) => match self.bin_op(op.op) {
                Some(oper) => ExprKind::BinOp {
                    left: Box::new(self.expr(&op.left)),
                    op: oper,
                    right: Box::new(self.expr(&op.right)),
                },
                None => {
                    self.err(expr.start(), format!("operator `{}` is not supported", op.op));
                    ExprKind::None
                }
            },
            Expr::UnaryOp(op) => {
                let oper = match op.op {
                    UnaryOp::Not => WUnary::Not,
                    UnaryOp::UAdd => WUnary::UAdd,
                    UnaryOp::USub => WUnary::USub,
                    UnaryOp::Invert => WUnary::Invert,
                };
                ExprKind::Unary {
                    op: oper,
                    operand: Box::new(self.expr(&op.operand)),
                }
            }
            Expr::BoolOp(op) => {
                let oper = match op.op {
                    ast::BoolOp::And => BoolOp::And,
                    ast::BoolOp::Or => BoolOp::Or,
                };
                ExprKind::BoolOp {
                    op: oper,
                    values: op.values.iter().map(|e| self.expr(e)).collect(),
                }
            }
            Expr::Compare(cmp) => {
                if cmp.operands.is_empty() {
                    self.err(expr.start(), "comparison is missing an operand");
                    ExprKind::None
                } else {
                    let mut ops = Vec::new();
                    for op in cmp.ops.iter() {
                        match self.cmp_op(*op) {
                            Some(op) => ops.push(op),
                            None => self.err(expr.start(), "this comparison is not supported"),
                        }
                    }
                    let mut operands = cmp.operands.iter();
                    let left = self.expr(operands.next().unwrap());
                    let comparators: Vec<WExpr> = operands.map(|e| self.expr(e)).collect();
                    ExprKind::Compare {
                        left: Box::new(left),
                        ops,
                        comparators,
                    }
                }
            }
            Expr::Call(call) => {
                let mut args = Vec::new();
                for arg in &call.arguments.args {
                    if matches!(arg, Expr::Starred(_)) {
                        self.err(arg.start(), "starred arguments are not supported; pass the list explicitly");
                    }
                    args.push(self.expr(arg));
                }
                let mut keywords = Vec::new();
                for kw in &call.arguments.keywords {
                    let (kline, _) = self.loc(kw.start());
                    if kw.arg.is_none() {
                        self.err(kw.start(), "keyword splatting is not supported; pass keywords by name");
                    }
                    keywords.push(Keyword {
                        line: kline,
                        name: kw.arg.as_ref().map(|id| id.id.as_str().to_string()),
                        value: self.expr(&kw.value),
                    });
                }
                ExprKind::Call {
                    func: Box::new(self.expr(&call.func)),
                    args,
                    keywords,
                }
            }
            Expr::Attribute(attr) => ExprKind::Attribute {
                value: Box::new(self.expr(&attr.value)),
                attr: attr.attr.id.as_str().to_string(),
            },
            Expr::Subscript(sub) => ExprKind::Subscript {
                value: Box::new(self.expr(&sub.value)),
                index: Box::new(self.expr(&sub.slice)),
            },
            Expr::Slice(slice) => ExprKind::Slice {
                lower: slice.lower.as_ref().map(|e| Box::new(self.expr(e))),
                upper: slice.upper.as_ref().map(|e| Box::new(self.expr(e))),
                step: slice.step.as_ref().map(|e| Box::new(self.expr(e))),
            },
            Expr::FString(f) => ExprKind::FString(self.fstring(f)),
            Expr::TString(_) => {
                self.err(expr.start(), "t-strings are not supported; use an f-string");
                ExprKind::None
            }
            Expr::If(ife) => ExprKind::IfExp {
                test: Box::new(self.expr(&ife.test)),
                body: Box::new(self.expr(&ife.body)),
                orelse: Box::new(self.expr(&ife.orelse)),
            },
            Expr::Named(_) => {
                self.err(
                    expr.start(),
                    "assignment expressions are not supported; assign on its own line",
                );
                ExprKind::None
            }
            Expr::Set(_)
            | Expr::Lambda(_)
            | Expr::ListComp(_)
            | Expr::SetComp(_)
            | Expr::DictComp(_)
            | Expr::Generator(_) => unreachable!("lowered by `sugar`"),
            Expr::Await(_) => {
                self.err(expr.start(), "`await` is not supported; Weave schedules tool calls itself");
                ExprKind::None
            }
            Expr::Yield(_) | Expr::YieldFrom(_) => {
                self.err(expr.start(), "`yield` is not supported");
                ExprKind::None
            }
            Expr::Starred(_) => {
                self.err(expr.start(), "starred expressions are not supported");
                ExprKind::None
            }
            Expr::IpyEscapeCommand(_) => {
                self.err(expr.start(), "IPython escapes are not supported");
                ExprKind::None
            }
        };
        WExpr { line, col, kind }
    }

    fn fstring(&mut self, expr: &ast::ExprFString) -> Vec<FSeg> {
        let mut segs = Vec::new();
        for part in expr.value.iter() {
            match part {
                ast::FStringPartRef::Literal(lit) => segs.push(FSeg::Lit(lit.as_str().to_string())),
                ast::FStringPartRef::FString(fs) => {
                    for element in fs.elements.iter() {
                        match element {
                            ast::InterpolatedStringElement::Literal(lit) => {
                                segs.push(FSeg::Lit(lit.value.to_string()));
                            }
                            ast::InterpolatedStringElement::Interpolation(piece) => {
                                let mut spec_text: Option<String> = None;
                                if let Some(spec) = &piece.format_spec {
                                    let mut text = String::new();
                                    for el in spec.elements.iter() {
                                        match el {
                                            ast::InterpolatedStringElement::Literal(lit) => text.push_str(&lit.value),
                                            ast::InterpolatedStringElement::Interpolation(_) => {
                                                self.err_hint(
                                                    piece.start(),
                                                    "nested fields in a format spec are not supported",
                                                    "write the spec literally, e.g. `{x:.2f}`",
                                                );
                                            }
                                        }
                                    }
                                    if !text.is_empty() {
                                        spec_text = Some(text);
                                    }
                                }
                                let mut conversion = match piece.conversion {
                                    ast::ConversionFlag::None => Conversion::None,
                                    ast::ConversionFlag::Str => Conversion::Str,
                                    ast::ConversionFlag::Repr => Conversion::Repr,
                                    ast::ConversionFlag::Ascii => Conversion::Ascii,
                                };
                                let mut value = self.expr(&piece.expression);
                                if let Some(spec) = spec_text {
                                    let (l, c) = (value.line, value.col);
                                    let conv = match conversion {
                                        Conversion::None => None,
                                        Conversion::Str => Some("str"),
                                        _ => {
                                            self.err_hint(
                                                piece.start(),
                                                "`!r`/`!a` with a format spec is not supported",
                                                "drop the conversion or the spec",
                                            );
                                            None
                                        }
                                    };
                                    if let Some(f) = conv {
                                        value = call_expr(l, c, f, vec![value]);
                                    }
                                    let spec_lit = WExpr { line: l, col: c, kind: ExprKind::Str(spec) };
                                    value = call_expr(l, c, "__weave_format", vec![value, spec_lit]);
                                    // Already a str; `Str` keeps `{x=:.2f}` debug output unquoted.
                                    conversion = Conversion::Str;
                                }
                                segs.push(FSeg::Interp {
                                    expr: value,
                                    conversion,
                                    debug: piece.debug_text.as_ref().map(|d| d.as_str().to_string()),
                                });
                            }
                        }
                    }
                }
            }
        }
        segs
    }

    fn bin_op(&self, op: Operator) -> Option<BinOp> {
        Some(match op {
            Operator::Add => BinOp::Add,
            Operator::Sub => BinOp::Sub,
            Operator::Mult => BinOp::Mul,
            Operator::Div => BinOp::Div,
            Operator::FloorDiv => BinOp::FloorDiv,
            Operator::Mod => BinOp::Mod,
            Operator::Pow => BinOp::Pow,
            Operator::BitAnd => BinOp::BitAnd,
            Operator::BitOr => BinOp::BitOr,
            Operator::BitXor => BinOp::BitXor,
            Operator::LShift => BinOp::LShift,
            Operator::RShift => BinOp::RShift,
            Operator::MatMult => return None,
        })
    }

    fn cmp_op(&self, op: CmpOp) -> Option<crate::ast::CmpOp> {
        Some(match op {
            CmpOp::Eq => crate::ast::CmpOp::Eq,
            CmpOp::NotEq => crate::ast::CmpOp::NotEq,
            CmpOp::Lt => crate::ast::CmpOp::Lt,
            CmpOp::LtE => crate::ast::CmpOp::LtE,
            CmpOp::Gt => crate::ast::CmpOp::Gt,
            CmpOp::GtE => crate::ast::CmpOp::GtE,
            CmpOp::In => crate::ast::CmpOp::In,
            CmpOp::NotIn => crate::ast::CmpOp::NotIn,
            CmpOp::Is => crate::ast::CmpOp::Is,
            CmpOp::IsNot => crate::ast::CmpOp::IsNot,
        })
    }
}

/// Line starts plus an ASCII flag per line, so locating a node is a binary
/// search instead of a scan from the top of the file.
struct LineIndex {
    starts: Vec<usize>,
    ascii: Vec<bool>,
}

impl LineIndex {
    fn new(source: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(source.match_indices('\n').map(|(i, _)| i + 1));
        let ascii = starts
            .iter()
            .enumerate()
            .map(|(n, &start)| {
                let end = starts.get(n + 1).copied().unwrap_or(source.len());
                source.as_bytes()[start..end].is_ascii()
            })
            .collect();
        Self { starts, ascii }
    }

    fn line_col(&self, source: &str, offset: usize) -> (usize, usize) {
        let offset = offset.min(source.len());
        let line = self.starts.partition_point(|&start| start <= offset) - 1;
        let start = self.starts[line];
        let col = if self.ascii[line] {
            offset - start + 1
        } else {
            source[start..offset].chars().count() + 1
        };
        (line + 1, col)
    }
}

pub fn line_col(source: &str, offset: TextSize) -> (usize, usize) {
    let offset = offset.to_usize();
    let mut line = 1usize;
    let mut col = 1usize;
    for (i, ch) in source.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn call_expr(line: usize, col: usize, name: &str, args: Vec<WExpr>) -> WExpr {
    WExpr {
        line,
        col,
        kind: ExprKind::Call {
            func: Box::new(WExpr { line, col, kind: ExprKind::Name(name.to_string()) }),
            args,
            keywords: Vec::new(),
        },
    }
}

/// Exception classes a script may name in `except`.
pub(crate) const CATCHABLE: &[&str] = &[
    "Exception", "BaseException", "ToolError", "KeyError", "IndexError", "LookupError", "TypeError", "ValueError",
    "AttributeError", "ZeroDivisionError", "ArithmeticError", "RuntimeError", "AssertionError",
];
const EXCEPT_HINT: &str =
    "use `except Exception:` (any error), `except ToolError:` (tool failures) or builtin classes such as `except (KeyError, IndexError):`";

/// `xs.append(v)` and `xs.extend(ys)` on a plain name become `xs += [v]` and
/// `xs += ys`. Weave values are immutable, so this is the only way these calls
/// can mean what the model intended, and the loop analysis already treats
/// `+=` as a loop-carried dependency.
fn desugar_list_mutation(expr: crate::ast::Expr) -> StmtKind {
    if let crate::ast::ExprKind::Call { func, args, keywords } = &expr.kind {
        if let crate::ast::ExprKind::Attribute { value, attr } = &func.kind {
            if let (crate::ast::ExprKind::Name(name), true, 1) = (&value.kind, keywords.is_empty(), args.len()) {
                let item = args[0].clone();
                let value = match attr.as_str() {
                    "append" => Some(crate::ast::Expr {
                        line: item.line,
                        col: item.col,
                        kind: crate::ast::ExprKind::List(vec![item]),
                    }),
                    "extend" => Some(item),
                    _ => None,
                };
                if let Some(value) = value {
                    return StmtKind::AugAssign {
                        target: crate::ast::Target::Name { line: expr.line, col: expr.col, name: name.clone() },
                        op: crate::ast::BinOp::Add,
                        value,
                    };
                }
            }
        }
    }
    StmtKind::Expr(expr)
}

/// Names the compiler introduces. They never appear in run results.
pub const HIDDEN_PREFIX: &str = "__weave_";

fn name_expr(line: usize, col: usize, name: &str) -> crate::ast::Expr {
    crate::ast::Expr { line, col, kind: crate::ast::ExprKind::Name(name.to_string()) }
}

fn assign_flag(line: usize, col: usize, flag: &str, value: bool) -> StmtKind {
    StmtKind::Assign {
        targets: vec![Target::Name { line, col, name: flag.to_string() }],
        value: crate::ast::Expr { line, col, kind: crate::ast::ExprKind::Bool(value) },
    }
}

fn is_jump(stmt: &WStmt, brk: &str, cont: &str) -> bool {
    match &stmt.kind {
        StmtKind::Assign { targets, value } => {
            matches!(value.kind, crate::ast::ExprKind::Bool(true))
                && matches!(targets.as_slice(), [Target::Name { name, .. }] if name == brk || name == cont)
        }
        _ => false,
    }
}

fn sets_flag(stmts: &[WStmt], brk: &str, cont: &str) -> bool {
    stmts.iter().any(|s| {
        is_jump(s, brk, cont)
            || match &s.kind {
                StmtKind::If { body, orelse, .. } => sets_flag(body, brk, cont) || sets_flag(orelse, brk, cont),
                StmtKind::For { body, .. } => sets_flag(body, brk, cont),
                StmtKind::Try { body, handler } => sets_flag(body, brk, cont) || sets_flag(&handler.body, brk, cont),
                _ => false,
            }
    })
}

/// `not a and not b ...` over flag names.
fn none_set(line: usize, col: usize, flags: &[&str]) -> crate::ast::Expr {
    let values = flags
        .iter()
        .map(|flag| crate::ast::Expr {
            line,
            col,
            kind: crate::ast::ExprKind::Unary {
                op: crate::ast::UnaryOp::Not,
                operand: Box::new(name_expr(line, col, flag)),
            },
        })
        .collect::<Vec<_>>();
    if values.len() == 1 {
        return values.into_iter().next().expect("one flag");
    }
    crate::ast::Expr { line, col, kind: crate::ast::ExprKind::BoolOp { op: crate::ast::BoolOp::And, values } }
}

/// Drop code after a jump and guard code after a block that may jump.
/// Returns the rewritten statements and whether they may jump.
fn guard_jumps(stmts: Vec<WStmt>, brk: &str, cont: &str) -> (Vec<WStmt>, bool) {
    let mut out = Vec::new();
    let mut rest = stmts.into_iter();
    while let Some(stmt) = rest.next() {
        if is_jump(&stmt, brk, cont) {
            out.push(stmt);
            return (out, true); // anything after is unreachable
        }
        let (line, col) = (stmt.line, stmt.col);
        let (kind, jumps) = match stmt.kind {
            StmtKind::If { test, body, orelse } => {
                let (body, a) = guard_jumps(body, brk, cont);
                let (orelse, b) = guard_jumps(orelse, brk, cont);
                (StmtKind::If { test, body, orelse }, a || b)
            }
            StmtKind::Try { body, handler } => {
                let (body, a) = guard_jumps(body, brk, cont);
                let (handler_body, b) = guard_jumps(handler.body, brk, cont);
                (StmtKind::Try { body, handler: crate::ast::Handler { body: handler_body, ..handler } }, a || b)
            }
            // A nested loop's own break/continue use its own flags; only a
            // helper's `return` sets an outer loop's flag from inside it.
            other => {
                let jumps = matches!(&other, StmtKind::For { body, .. } if sets_flag(body, brk, cont));
                (other, jumps)
            }
        };
        out.push(WStmt { line, col, kind });
        if jumps {
            let (tail, _) = guard_jumps(rest.collect(), brk, cont);
            if !tail.is_empty() {
                out.push(WStmt {
                    line,
                    col,
                    kind: StmtKind::If { test: none_set(line, col, &[brk, cont]), body: tail, orelse: Vec::new() },
                });
            }
            return (out, true);
        }
    }
    (out, false)
}

/// Rewrite a loop that uses `break`/`continue` into flag-guarded code:
///
/// ```text
/// if True:
///     brk = False
///     for it in iter:
///         if not brk:
///             cont = False
///             target = it
///             <body, with code after a jump guarded by `not brk and not cont`>
/// ```
///
/// The guard reads `brk` before the body assigns it, so the scheduler sees a
/// loop-carried dependency and runs iterations in order. Calls inside one
/// iteration still run in parallel. The loop target is bound only for
/// iterations that run, so after `break` it holds the item that broke.
fn desugar_jumps(line: usize, col: usize, target: Target, iter: crate::ast::Expr, body: Vec<WStmt>, brk: &str, cont: &str) -> StmtKind {
    let item = format!("{brk}_item");
    let (guarded, _) = guard_jumps(body, brk, cont);
    let mut inner = vec![
        WStmt { line, col, kind: assign_flag(line, col, cont, false) },
        WStmt {
            line,
            col,
            kind: StmtKind::Assign { targets: vec![target], value: name_expr(line, col, &item) },
        },
    ];
    inner.extend(guarded);
    let loop_body = vec![WStmt {
        line,
        col,
        kind: StmtKind::If { test: none_set(line, col, &[brk]), body: inner, orelse: Vec::new() },
    }];
    StmtKind::If {
        test: crate::ast::Expr { line, col, kind: crate::ast::ExprKind::Bool(true) },
        body: vec![
            WStmt { line, col, kind: assign_flag(line, col, brk, false) },
            WStmt {
                line,
                col,
                kind: StmtKind::For { target: Target::Name { line, col, name: item }, iter, body: loop_body },
            },
        ],
        orelse: Vec::new(),
    }
}
