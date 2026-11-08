//! Weave runs a Python-like script of tool calls, overlapping independent
//! pure and read calls, and deferring irreversible calls until the program
//! is complete.

#![allow(clippy::too_many_arguments)]

mod analysis;
mod ast;
mod cancel;
mod check;
mod effect;
mod error;
mod exec;
mod parse;
mod speculate;
mod stream;
mod tools;
mod trace;
mod value;

pub use cancel::CancelToken;
pub use effect::Effect;
pub use error::{Diagnostic, WeaveError};
pub use exec::{Engine, RunResult, Session};
pub use speculate::{PredictedCall, Predictor, SpecCache, SpecStats};
pub use tools::{IsolatedJob, ToolFn, ToolSpec};
pub use trace::{Trace, TraceEvent};
pub use value::Value;
