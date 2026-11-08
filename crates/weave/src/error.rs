use std::fmt;

/// One static or runtime diagnostic. Messages are a single line so an LLM can patch them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub line: usize,
    pub col: usize,
    pub message: String,
    pub hint: Option<String>,
    pub class: ErrorClass,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorClass {
    Static,
    Runtime,
    /// A host tool failed, timed out, or exhausted its retries.
    Tool,
    /// The session was cancelled. `try` does not catch this.
    Cancelled,
    /// A name placeholder that the taken path never assigned. Bindings ignore it.
    Unbound,
}

impl Diagnostic {
    pub fn static_err(line: usize, col: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            col,
            message: message.into(),
            hint: None,
            class: ErrorClass::Static,
        }
    }

    pub fn runtime(line: usize, col: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            col,
            message: message.into(),
            hint: None,
            class: ErrorClass::Runtime,
        }
    }

    pub fn tool(line: usize, col: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            col,
            message: message.into(),
            hint: Some("catch it with `except ToolError` or fix the call".into()),
            class: ErrorClass::Tool,
        }
    }

    pub fn cancelled(line: usize, col: usize) -> Self {
        Self {
            line,
            col,
            message: "execution was cancelled".into(),
            hint: Some("call finish() only if the work should still run".into()),
            class: ErrorClass::Cancelled,
        }
    }

    pub fn unbound(line: usize, col: usize, name: &str) -> Self {
        Self {
            line,
            col,
            message: format!("name `{name}` is unbound on this path"),
            hint: Some(format!("assign `{name}` before this use")),
            class: ErrorClass::Unbound,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            write!(f, "{}", self.message)?;
        } else if self.col == 0 {
            write!(f, "L{}: {}", self.line, self.message)?;
        } else {
            write!(f, "L{}:C{}: {}", self.line, self.col, self.message)?;
        }
        if let Some(hint) = &self.hint {
            write!(f, " hint: {hint}")?;
        }
        Ok(())
    }
}

/// One or more diagnostics from a check or a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WeaveError {
    pub diagnostics: Vec<Diagnostic>,
}

impl WeaveError {
    pub fn new(diagnostics: Vec<Diagnostic>) -> Self {
        Self { diagnostics }
    }

    pub fn one(diagnostic: Diagnostic) -> Self {
        Self {
            diagnostics: vec![diagnostic],
        }
    }

    pub fn runtime(line: usize, col: usize, message: impl Into<String>) -> Self {
        Self::one(Diagnostic::runtime(line, col, message))
    }

    pub fn tool(line: usize, col: usize, message: impl Into<String>) -> Self {
        Self::one(Diagnostic::tool(line, col, message))
    }

    pub fn cancelled(line: usize, col: usize) -> Self {
        Self::one(Diagnostic::cancelled(line, col))
    }

    pub fn static_err(line: usize, col: usize, message: impl Into<String>) -> Self {
        Self::one(Diagnostic::static_err(line, col, message))
    }

    pub fn unbound(name: &str) -> Self {
        Self::one(Diagnostic::unbound(0, 0, name))
    }

    pub fn is_unbound(&self) -> bool {
        self.diagnostics.len() == 1 && self.diagnostics[0].class == ErrorClass::Unbound
    }

    pub fn is_cancelled(&self) -> bool {
        self.diagnostics.iter().any(|d| d.class == ErrorClass::Cancelled)
    }

    /// Tool failures and ordinary runtime errors can be caught by `try`.
    pub fn is_catchable(&self) -> bool {
        self.diagnostics.iter().any(|d| {
            matches!(d.class, ErrorClass::Tool | ErrorClass::Runtime)
        }) && !self.is_cancelled()
    }

    pub fn is_tool(&self) -> bool {
        self.diagnostics.iter().any(|d| d.class == ErrorClass::Tool) && !self.is_cancelled()
    }

    pub fn summary(&self) -> String {
        self.diagnostics
            .first()
            .map(|d| d.message.clone())
            .unwrap_or_else(|| "error".into())
    }
}

impl fmt::Display for WeaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, diag) in self.diagnostics.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "{diag}")?;
        }
        Ok(())
    }
}

impl std::error::Error for WeaveError {}
