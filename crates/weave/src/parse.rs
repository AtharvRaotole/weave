use ruff_python_ast as ast;
use ruff_python_ast::{CmpOp, Expr, Number, Operator, Stmt, UnaryOp};
use ruff_python_parser::parse_module;
use ruff_text_size::{Ranged, TextSize};

use crate::ast::{
    BinOp, BoolOp, Conversion, Expr as WExpr, ExprKind, FSeg, Keyword, Stmt as WStmt, StmtKind,
    Target, UnaryOp as WUnary,
};
use crate::error::{Diagnostic, WeaveError};

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
        line_offset,
        errors: Vec::new(),
    };
    let stmts = parsed.syntax().body.iter().map(|stmt| lower.stmt(stmt)).collect();
    Ok(ParsedProgram {
        stmts,
        errors: lower.errors,
    })
}

struct Lowerer<'a> {
    source: &'a str,
    line_offset: usize,
    errors: Vec<Diagnostic>,
}

impl<'a> Lowerer<'a> {
    fn loc(&self, offset: TextSize) -> (usize, usize) {
        let (line, col) = line_col(self.source, offset);
        (line + self.line_offset, col)
    }

    fn err(&mut self, offset: TextSize, message: impl Into<String>) {
        let (line, col) = self.loc(offset);
        self.errors.push(Diagnostic::static_err(line, col, message));
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
            Stmt::Expr(s) => StmtKind::Expr(self.expr(&s.value)),
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
                StmtKind::For {
                    target: self.target(&s.target),
                    iter: self.expr(&s.iter),
                    body: self.suite(&s.body),
                }
            }
            Stmt::Pass(_) => StmtKind::Pass,
            Stmt::FunctionDef(_) => {
                self.err(
                    stmt.start(),
                    "`def` is not supported; register a host tool instead of defining a function",
                );
                StmtKind::Pass
            }
            Stmt::ClassDef(_) => {
                self.err(stmt.start(), "`class` is not supported; use dicts for records");
                StmtKind::Pass
            }
            Stmt::Return(_) => {
                self.err(stmt.start(), "`return` is not supported; the script has no functions");
                StmtKind::Pass
            }
            Stmt::Delete(_) => {
                self.err(stmt.start(), "`del` is not supported; rebind the name instead");
                StmtKind::Pass
            }
            Stmt::TypeAlias(_) => {
                self.err(stmt.start(), "`type` aliases are not supported");
                StmtKind::Pass
            }
            Stmt::While(_) => {
                self.err(
                    stmt.start(),
                    "`while` is not supported; use `for` over a list",
                );
                StmtKind::Pass
            }
            Stmt::With(_) => {
                self.err(stmt.start(), "`with` is not supported; call the tool directly");
                StmtKind::Pass
            }
            Stmt::Match(_) => {
                self.err(stmt.start(), "`match` is not supported; use `if`");
                StmtKind::Pass
            }
            Stmt::Raise(_) => {
                self.err(stmt.start(), "`raise` is not supported");
                StmtKind::Pass
            }
            Stmt::Try(_) => {
                self.err(stmt.start(), "`try` is not supported; check values with `if`");
                StmtKind::Pass
            }
            Stmt::Assert(_) => {
                self.err(stmt.start(), "`assert` is not supported; use `if`");
                StmtKind::Pass
            }
            Stmt::Import(_) | Stmt::ImportFrom(_) => {
                self.err(
                    stmt.start(),
                    "`import` is not supported; inline values or fetch them with a tool",
                );
                StmtKind::Pass
            }
            Stmt::Global(_) | Stmt::Nonlocal(_) => {
                self.err(stmt.start(), "`global` and `nonlocal` are not supported");
                StmtKind::Pass
            }
            Stmt::Break(_) => {
                self.err(stmt.start(), "`break` is not supported; filter the list before the loop");
                StmtKind::Pass
            }
            Stmt::Continue(_) => {
                self.err(stmt.start(), "`continue` is not supported; guard the body with `if`");
                StmtKind::Pass
            }
            Stmt::IpyEscapeCommand(_) => {
                self.err(stmt.start(), "IPython escapes are not supported");
                StmtKind::Pass
            }
        };
        WStmt { line, col, kind }
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
