//! Resource limits for untrusted scripts.
//!
//! Weave scripts come from a model, so the runtime treats them as untrusted.
//! The language has no `import`, `def`, `class`, `while`, attribute access,
//! file or network builtins; the only way out is a host tool the embedder
//! registered. These limits bound what a script can do *inside* the runtime:
//!
//! - **Steps.** Every executed statement and every loop iteration counts as
//!   one step. A run that exceeds [`Limits::max_steps`] fails.
//! - **Value size.** Strings count bytes, lists and dicts count elements
//!   (recursively). Building a value larger than [`Limits::max_value_size`]
//!   fails before the large allocation where the size is predictable
//!   (`*` repetition, `replace`, `join`) and right after it otherwise.
//! - **Nesting depth.** Source nesting (brackets, blocks, expression trees)
//!   and value nesting are capped at [`MAX_DEPTH`], so neither the parser,
//!   the scheduler, nor value printing can overflow the stack.

use crate::value::Value;

/// Maximum nesting of brackets, blocks, expressions, and values.
pub const MAX_DEPTH: usize = 100;

/// Maximum operator-like tokens in one logical line. Bounds parse recursion
/// before the parser runs. Literal-heavy lines (long lists of numbers or
/// strings) are not affected because names, literals, and commas don't count.
pub const MAX_OPS_PER_STATEMENT: usize = 1_000;

/// Hard cap applied by value operations regardless of engine settings, so a
/// single `"a" * n` can never try to allocate more than this.
pub const HARD_MAX_VALUE_SIZE: usize = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Statements plus loop iterations per run. Default 1,000,000.
    pub max_steps: u64,
    /// Largest value a script may build (bytes for strings, elements for
    /// containers). Default 16 MiB. Clamped to [`HARD_MAX_VALUE_SIZE`].
    pub max_value_size: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_steps: 1_000_000,
            max_value_size: 16 * 1024 * 1024,
        }
    }
}

impl Limits {
    pub fn with_max_steps(mut self, steps: u64) -> Self {
        self.max_steps = steps;
        self
    }

    pub fn with_max_value_size(mut self, size: usize) -> Self {
        self.max_value_size = size.min(HARD_MAX_VALUE_SIZE);
        self
    }
}

/// Check a value's total size and nesting without recursion. Stops early once
/// either limit is exceeded.
pub fn check_value(value: &Value, max_size: usize) -> Result<(), String> {
    let mut size = 0usize;
    let mut stack: Vec<(&Value, usize)> = vec![(value, 1)];
    while let Some((v, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            return Err(format!("value is nested deeper than {MAX_DEPTH} levels"));
        }
        size = size.saturating_add(match v {
            Value::Str(s) => s.len(),
            Value::List(xs) => xs.len(),
            Value::Dict(items) => items.len(),
            _ => 1,
        });
        if size > max_size {
            return Err(format!("value is larger than the {max_size} limit"));
        }
        match v {
            Value::List(xs) => stack.extend(xs.iter().map(|x| (x, depth + 1))),
            Value::Dict(items) => {
                for (k, x) in items {
                    stack.push((k, depth + 1));
                    stack.push((x, depth + 1));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Cheap lexical guard run before the parser. Rejects sources whose nesting
/// or per-statement operator count could exhaust the parser's stack.
pub fn check_source_shape(source: &str) -> Result<(), (usize, String)> {
    use ruff_python_ast::token::TokenKind;
    use ruff_python_parser::lexer::lex;
    use ruff_python_parser::Mode;

    let mut lexer = lex(source, Mode::Module);
    let mut brackets = 0usize;
    let mut indent = 0usize;
    let mut ops = 0usize;
    loop {
        let kind = lexer.next_token();
        let start = u32::from(lexer.current_range().start()) as usize;
        match kind {
            TokenKind::EndOfFile => return Ok(()),
            TokenKind::Lpar | TokenKind::Lsqb | TokenKind::Lbrace => {
                brackets += 1;
                ops += 1;
            }
            TokenKind::Rpar | TokenKind::Rsqb | TokenKind::Rbrace => brackets = brackets.saturating_sub(1),
            TokenKind::Indent => indent += 1,
            TokenKind::Dedent => indent = indent.saturating_sub(1),
            TokenKind::Newline => ops = 0,
            TokenKind::Identifier
            | TokenKind::Int
            | TokenKind::Float
            | TokenKind::Complex
            | TokenKind::String
            | TokenKind::Comma
            | TokenKind::Colon
            | TokenKind::Comment
            | TokenKind::NonLogicalNewline
            | TokenKind::FStringMiddle => {}
            _ => ops += 1,
        }
        if brackets > MAX_DEPTH || indent > MAX_DEPTH {
            return Err((start, format!("code is nested deeper than {MAX_DEPTH} levels")));
        }
        if ops > MAX_OPS_PER_STATEMENT {
            return Err((
                start,
                format!("statement has more than {MAX_OPS_PER_STATEMENT} operators; split it up"),
            ));
        }
    }
}
