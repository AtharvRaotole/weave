use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::WeaveError;
use crate::value::Value;

pub const TRACE_VERSION: u32 = 2;

/// Deterministic record of host tool calls, in program order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Trace {
    pub version: u32,
    pub events: Vec<TraceEvent>,
    pub source: String,
    pub manifest: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TraceEvent {
    pub seq: u64,
    pub invocation: Vec<usize>,
    pub line: usize,
    pub tool: String,
    pub effect: String,
    pub args: Vec<Value>,
    pub kwargs: BTreeMap<String, Value>,
    pub result: Value,
    /// Set when the call failed. Replay raises the same tool error instead of returning `result`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Completion order of the call within the run (0 = finished first).
    /// Diagnostic only; replay uses the logical invocation path.
    #[serde(default)]
    pub done: u64,
}

impl Trace {
    pub fn empty() -> Self {
        Self {
            version: TRACE_VERSION,
            source: String::new(),
            manifest: BTreeMap::new(),
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
    next_done: u64,
    max_bytes: usize,
    used_bytes: usize,
    envelope_bytes: usize,
    exceeded: bool,
}

impl TraceBuilder {
    pub(crate) fn bounded(max_bytes: usize) -> Self {
        Self { max_bytes, ..Default::default() }
    }
    pub(crate) fn reserve_envelope(&mut self, bytes: usize) -> Result<(), WeaveError> {
        self.envelope_bytes = bytes;
        if bytes.saturating_add(self.used_bytes) > self.max_bytes {
            self.exceeded = true;
            return Err(WeaveError::budget("aggregate trace byte budget exceeded"));
        }
        Ok(())
    }

    /// Record a finished call. `done` overrides the completion index
    /// (replay copies it from the recorded event so traces compare equal).
    pub(crate) fn record(&mut self, mut event: TraceEvent, done: Option<u64>) -> Result<(), WeaveError> {
        event.done = done.unwrap_or(self.next_done);
        self.next_done += 1;
        let bytes = json_size(&event)?.saturating_add(usize::from(self.next_done > 1));
        if self.exceeded || bytes > self.max_bytes.saturating_sub(self.envelope_bytes).saturating_sub(self.used_bytes) {
            self.exceeded = true;
            return Err(WeaveError::budget("aggregate trace byte budget exceeded"));
        }
        self.used_bytes += bytes;
        let seq = event.seq as usize;
        if self.events.len() <= seq {
            self.events.resize(seq + 1, None);
        }
        self.events[seq] = Some(event);
        Ok(())
    }

    pub(crate) fn finish(&self) -> Result<Trace, WeaveError> {
        if self.exceeded { return Err(WeaveError::budget("aggregate trace byte budget exceeded")); }
        let mut events = Vec::with_capacity(self.events.len());
        // A scheduled call can be skipped by an earlier failure. Only actual
        // invocations belong in the trace; invocation identity is not seq.
        for event in self.events.iter().flatten() {
            events.push(event.clone());
        }
        Ok(Trace {
            version: TRACE_VERSION,
            source: String::new(),
            manifest: BTreeMap::new(),
            events,
        })
    }
}

/// Counts compact serialized bytes without creating another JSON allocation.
pub(crate) fn json_size(value: &impl Serialize) -> Result<usize, WeaveError> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len()); Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value).map_err(|_| WeaveError::runtime(0, 0, "trace encoding failed"))?;
    Ok(counter.0)
}
