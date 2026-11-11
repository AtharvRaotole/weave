/// Weave's own statement tree, lowered from the Ruff Python AST.
#[derive(Clone, Debug)]
pub struct Stmt {
    pub line: usize,
    pub col: usize,
    pub kind: StmtKind,
}

#[derive(Clone, Debug)]
pub enum StmtKind {
    Assign {
        targets: Vec<Target>,
        value: Expr,
    },
    AugAssign {
        target: Target,
        op: BinOp,
        value: Expr,
    },
    Expr(Expr),
    If {
        test: Expr,
        body: Vec<Stmt>,
        orelse: Vec<Stmt>,
    },
    For {
        target: Target,
        iter: Expr,
        body: Vec<Stmt>,
    },
    Try {
        body: Vec<Stmt>,
        handler: Handler,
    },
    Pass,
}

/// One `except` clause. Bare `except`, `except Exception` and
/// `except BaseException` catch every tool and runtime error. `except
/// ToolError` catches only host-tool failures. Builtin Python classes such as
/// `KeyError` or `(KeyError, IndexError)` catch the runtime errors of that
/// kind (see `exec::error_kind`).
#[derive(Clone, Debug)]
pub struct Handler {
    #[allow(dead_code)]
    pub line: usize,
    #[allow(dead_code)]
    pub col: usize,
    pub catches_all: bool,
    /// Exception class names when `catches_all` is false.
    pub classes: Vec<String>,
    pub name: Option<String>,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub enum Target {
    Name {
        line: usize,
        col: usize,
        name: String,
    },
    Subscript {
        line: usize,
        col: usize,
        base: Expr,
        index: Expr,
    },
    Unpack {
        line: usize,
        col: usize,
        targets: Vec<Target>,
    },
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub line: usize,
    pub col: usize,
    pub kind: ExprKind,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Name(String),
    List(Vec<Expr>),
    Tuple(Vec<Expr>),
    Dict(Vec<(Expr, Expr)>),
    BinOp {
        left: Box<Expr>,
        op: BinOp,
        right: Box<Expr>,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    BoolOp {
        op: BoolOp,
        values: Vec<Expr>,
    },
    Compare {
        left: Box<Expr>,
        ops: Vec<CmpOp>,
        comparators: Vec<Expr>,
    },
    Call {
        func: Box<Expr>,
        args: Vec<Expr>,
        keywords: Vec<Keyword>,
    },
    Attribute {
        value: Box<Expr>,
        attr: String,
    },
    Subscript {
        value: Box<Expr>,
        index: Box<Expr>,
    },
    Slice {
        lower: Option<Box<Expr>>,
        upper: Option<Box<Expr>>,
        step: Option<Box<Expr>>,
    },
    FString(Vec<FSeg>),
    IfExp {
        test: Box<Expr>,
        body: Box<Expr>,
        orelse: Box<Expr>,
    },
}

#[derive(Clone, Debug)]
pub struct Keyword {
    pub line: usize,
    pub name: Option<String>,
    pub value: Expr,
}

#[derive(Clone, Debug)]
pub enum FSeg {
    Lit(String),
    Interp {
        expr: Expr,
        conversion: Conversion,
        debug: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conversion {
    None,
    Str,
    Repr,
    Ascii,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    Pow,
    BitAnd,
    BitOr,
    BitXor,
    LShift,
    RShift,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Not,
    UAdd,
    USub,
    Invert,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoolOp {
    And,
    Or,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    NotEq,
    Lt,
    LtE,
    Gt,
    GtE,
    In,
    NotIn,
    Is,
    IsNot,
}

impl BinOp {
    #[allow(dead_code)]
    pub fn token(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::FloorDiv => "//",
            Self::Mod => "%",
            Self::Pow => "**",
            Self::BitAnd => "&",
            Self::BitOr => "|",
            Self::BitXor => "^",
            Self::LShift => "<<",
            Self::RShift => ">>",
        }
    }
}
