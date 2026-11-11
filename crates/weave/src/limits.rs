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
//! - **Wall time.** A run is cancelled once [`Limits::max_wall_time`] passes.
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
    /// Aggregate UTF-8 source bytes per session, before parsing.
    pub max_source_bytes: usize,
    /// Aggregate emitted stdout UTF-8 bytes per run.
    pub max_output_bytes: usize,
    /// Compact JSON trace bytes (events plus final source/manifest envelope).
    pub max_trace_bytes: usize,
    /// Largest value a script may build (bytes for strings, elements for
    /// containers). Default 16 MiB. Clamped to [`HARD_MAX_VALUE_SIZE`].
    pub max_value_size: usize,
    /// Wall-clock budget for a run, from the moment code reaches the
    /// scheduler. At the limit the run is cancelled (in-flight async and
    /// isolated tools are stopped) and fails. Default 30 s.
    pub max_wall_time: std::time::Duration,
    /// Start the read calls of an in-order loop (one with a loop-carried
    /// accumulator) for every iteration up front. See
    /// `analysis::prefetchable_calls`. Default true.
    pub loop_prefetch: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_steps: 1_000_000,
            max_source_bytes: 1024 * 1024,
            max_output_bytes: 1024 * 1024,
            max_trace_bytes: 4 * 1024 * 1024,
            max_value_size: 16 * 1024 * 1024,
            max_wall_time: std::time::Duration::from_secs(30),
            loop_prefetch: true,
        }
    }
}

impl Limits {
    pub fn with_max_steps(mut self, steps: u64) -> Self {
        self.max_steps = steps;
        self
    }

    pub fn with_max_wall_time(mut self, limit: std::time::Duration) -> Self {
        self.max_wall_time = limit;
        self
    }

    pub fn with_loop_prefetch(mut self, enabled: bool) -> Self {
        self.loop_prefetch = enabled;
        self
    }

    pub fn with_max_value_size(mut self, size: usize) -> Self {
        self.max_value_size = size.min(HARD_MAX_VALUE_SIZE);
        self
    }
}

/// Total size of `value` (same units as [`check_value`]), counting at most
/// `cap + 1` so a huge value is not walked to the end.
pub fn size_capped(value: &Value, cap: usize) -> usize {
    let mut size = 0usize;
    let mut stack = vec![value];
    while let Some(v) = stack.pop() {
        size = size.saturating_add(match v {
            Value::Str(s) => s.len(),
            Value::List(xs) => xs.len(),
            Value::Dict(items) => items.len(),
            _ => 1,
        });
        if size > cap {
            return size;
        }
        match v {
            Value::List(xs) => stack.extend(xs.iter()),
            Value::Dict(items) => {
                for (k, x) in items {
                    stack.push(k);
                    stack.push(x);
                }
            }
            _ => {}
        }
    }
    size
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
