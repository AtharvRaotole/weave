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

pub fn parse_program(source: &str, line_offset: usize) -> Result<Vec<WStmt>, WeaveError> {
    let parsed = parse_detailed(source, line_offset)?;
    if parsed.errors.is_empty() {
        Ok(parsed.stmts)
    } else {
        Err(WeaveError::new(parsed.errors))
    }
}

/// Lower the module even when individual statements are outside the subset.
/// A hard syntax failure is still an `Err`.
pub fn parse_detailed(source: &str, line_offset: usize) -> Result<ParsedProgram, WeaveError> {
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
    let mut lower = Lowerer {
        source,
        lines: LineIndex::new(source),
        line_offset,
        errors: Vec::new(),
        loops: Vec::new(),
    };
    let stmts: Vec<WStmt> = parsed.syntax().body.iter().map(|stmt| lower.stmt(stmt)).collect();
    for stmt in &stmts {
        if stmt_depth(stmt, 1) > MAX_DEPTH {
            return Err(WeaveError::static_err(
                stmt.line,
                stmt.col,
                format!("code is nested deeper than {MAX_DEPTH} levels; split it into smaller statements"),
            ));
        }
    }
    Ok(ParsedProgram {
        stmts,
        errors: lower.errors,
    })
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

struct Lowerer<'a> {
    source: &'a str,
    lines: LineIndex,
    line_offset: usize,
    errors: Vec<Diagnostic>,
    /// Enclosing loops, innermost last: (break flag, continue flag, used).
    loops: Vec<(String, String, bool)>,
}

impl<'a> Lowerer<'a> {
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
            Stmt::Assign(s) => {
                let targets = s.targets.iter().map(|t| self.target(t)).collect();
                StmtKind::Assign {
                    targets,
                    value: self.expr(&s.value),
                }
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
            Stmt::Expr(s) => desugar_list_mutation(self.expr(&s.value)),
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
            Stmt::FunctionDef(_) => {
                self.err_hint(
                    stmt.start(),
                    "`def` is not supported",
                    "register a host tool instead of defining a function",
                );
                StmtKind::Pass
            }
            Stmt::ClassDef(_) => {
                self.err_hint(stmt.start(), "`class` is not supported", "use a dict for the record");
                StmtKind::Pass
            }
            Stmt::Return(_) => {
                self.err_hint(stmt.start(), "`return` is not supported", "the script has no functions; assign the value instead");
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
            Stmt::Raise(_) => {
                self.err_hint(stmt.start(), "`raise` is not supported", "return a tool error by letting the tool fail, or use `if`");
                StmtKind::Pass
            }
            Stmt::Try(s) => self.lower_try(s),
            Stmt::Assert(_) => {
                self.err_hint(stmt.start(), "`assert` is not supported", "use `if`");
                StmtKind::Pass
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
                match self.loops.last_mut() {
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
                "use `except ToolError` or a bare `except`",
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
                "use `except ToolError:` or a bare `except:`",
            );
            return StmtKind::Pass;
        }
        let ast::ExceptHandler::ExceptHandler(handler) = &stmt.handlers[0];
        let catches_all = match handler.type_.as_deref() {
            None => true,
            Some(ast::Expr::Name(name)) if name.id.as_str() == "ToolError" => false,
            Some(_) => {
                self.err_hint(
                    handler.range.start(),
                    "only `except ToolError` or a bare `except` is supported",
                    "catch tool failures with `except ToolError`",
                );
                return StmtKind::Pass;
            }
        };
        let (line, col) = self.loc(handler.range.start());
        StmtKind::Try {
            body: self.suite(&stmt.body),
            handler: Handler {
                line,
                col,
                catches_all,
                name: handler.name.as_ref().map(|id| id.id.as_str().to_string()),
                body: self.suite(&handler.body),
            },
        }
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
        stmts.iter().map(|stmt| self.stmt(stmt)).collect()
    }

    fn target(&mut self, expr: &Expr) -> Target {
        let (line, col) = self.loc(expr.start());
        match expr {
            Expr::Name(name) => Target::Name {
                line,
                col,
                name: name.id.as_str().to_string(),
            },
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
            Expr::Name(name) => ExprKind::Name(name.id.as_str().to_string()),
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
            Expr::Set(_) => {
                self.err(expr.start(), "sets are not supported; use a list or a dict");
                ExprKind::None
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
            Expr::Lambda(_) => {
                self.err(expr.start(), "`lambda` is not supported; register a host tool");
                ExprKind::None
            }
            Expr::ListComp(_) | Expr::SetComp(_) | Expr::DictComp(_) | Expr::Generator(_) => {
                self.err(
                    expr.start(),
                    "comprehensions are not supported; use a for loop",
                );
                ExprKind::None
            }
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
                                if let Some(spec) = &piece.format_spec {
                                    if !spec.elements.is_empty() {
                                        self.err(
                                            piece.start(),
                                            "format specs are not supported; compute the value before the f-string",
                                        );
                                    }
                                }
                                let conversion = match piece.conversion {
                                    ast::ConversionFlag::None => Conversion::None,
                                    ast::ConversionFlag::Str => Conversion::Str,
                                    ast::ConversionFlag::Repr => Conversion::Repr,
                                    ast::ConversionFlag::Ascii => Conversion::Ascii,
                                };
                                segs.push(FSeg::Interp {
                                    expr: self.expr(&piece.expression),
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
            // A nested loop's own break/continue use its own flags.
            other => (other, false),
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
