use std::fmt;

/// One static or runtime diagnostic. Messages are a single line so an LLM can patch them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub line: usize,
    pub col: usize,
    pub message: String,
    pub class: ErrorClass,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorClass {
    Static,
    Runtime,
    /// A name placeholder that the taken path never assigned. Bindings ignore it.
    Unbound,
}

impl Diagnostic {
    pub fn static_err(line: usize, col: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            col,
            message: message.into(),
            class: ErrorClass::Static,
        }
    }

    pub fn runtime(line: usize, col: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            col,
            message: message.into(),
            class: ErrorClass::Runtime,
        }
    }

    pub fn unbound(line: usize, col: usize, name: &str) -> Self {
        Self {
            line,
            col,
            message: format!("name `{name}` is unbound on this path"),
            class: ErrorClass::Unbound,
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            write!(f, "{}", self.message)
        } else if self.col == 0 {
            write!(f, "L{}: {}", self.line, self.message)
        } else {
            write!(f, "L{}:C{}: {}", self.line, self.col, self.message)
        }
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

    pub fn static_err(line: usize, col: usize, message: impl Into<String>) -> Self {
        Self::one(Diagnostic::static_err(line, col, message))
    }

    pub fn unbound(name: &str) -> Self {
        Self::one(Diagnostic::unbound(0, 0, name))
    }

    pub fn is_unbound(&self) -> bool {
        self.diagnostics.len() == 1 && self.diagnostics[0].class == ErrorClass::Unbound
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
