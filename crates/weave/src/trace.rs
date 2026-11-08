use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::WeaveError;
use crate::value::Value;

pub const TRACE_VERSION: u32 = 1;

/// Deterministic record of host tool calls, in program order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Trace {
    pub version: u32,
    pub events: Vec<TraceEvent>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TraceEvent {
    pub seq: u64,
    pub line: usize,
    pub tool: String,
    pub effect: String,
    pub args: Vec<Value>,
    pub kwargs: BTreeMap<String, Value>,
    pub result: Value,
    /// Set when the call failed. Replay raises the same tool error instead of returning `result`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Trace {
    pub fn empty() -> Self {
        Self {
            version: TRACE_VERSION,
            events: Vec::new(),
        }
    }

    pub fn to_json(&self) -> Result<String, WeaveError> {
        serde_json::to_string_pretty(self)
            .map_err(|err| WeaveError::runtime(0, 0, format!("trace encode failed: {err}")))
    }

    pub fn from_json(text: &str) -> Result<Self, WeaveError> {
        let trace: Trace = serde_json::from_str(text)
            .map_err(|err| WeaveError::runtime(0, 0, format!("trace decode failed: {err}")))?;
        if trace.version != TRACE_VERSION {
            return Err(WeaveError::runtime(
                0,
                0,
                format!(
                    "trace version {} is not supported; expected {TRACE_VERSION}",
                    trace.version
                ),
            ));
        }
        Ok(trace)
    }
}

#[derive(Default)]
pub(crate) struct TraceBuilder {
    events: Vec<Option<TraceEvent>>,
}

impl TraceBuilder {
    pub(crate) fn record(&mut self, event: TraceEvent) {
        let seq = event.seq as usize;
        if self.events.len() <= seq {
            self.events.resize(seq + 1, None);
        }
        self.events[seq] = Some(event);
    }

    pub(crate) fn finish(&self) -> Result<Trace, WeaveError> {
        let mut events = Vec::with_capacity(self.events.len());
        for (i, slot) in self.events.iter().enumerate() {
            match slot {
                Some(event) => events.push(event.clone()),
                None => {
                    return Err(WeaveError::runtime(
                        0,
                        0,
                        format!("trace is missing call {i}"),
                    ));
                }
            }
        }
        Ok(Trace {
            version: TRACE_VERSION,
            events,
        })
    }
}
