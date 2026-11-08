//! Speculative pre-execution of pure and read tools.
//!
//! This is the tool-cache half of Nichols et al., "Optimizing Agentic Language
//! Model Inference via Speculative Tool Calls" (arXiv:2512.15834). A predictor
//! proposes calls. Weave starts the pure and read ones immediately and stores
//! each result under a key of tool name plus canonical arguments. When the
//! script reaches the same call, it waits on that result instead of calling
//! the tool again.
//!
//! Only [`Effect::Pure`] and [`Effect::Read`] run ahead. Reversible and
//! irreversible calls are rejected. The cache does not reorder observation:
//! the script still waits for its usual effect barrier, then reads the value
//! that was computed early.
//!
//! Writes invalidate. When the script starts a reversible or irreversible
//! call, every cached read whose [`Footprint`] overlaps the write's footprint
//! is dropped before the write runs. A later read of the same call misses and
//! runs live, so it observes the write. Tools default to [`Footprint::All`],
//! so with no annotations any write drops every cached read. Pure entries
//! are never dropped. The speculative call itself is not rolled back.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use tokio::runtime::Handle;
use tokio::sync::watch;

use crate::effect::Effect;
use crate::tools::{Footprint, ToolBody, ToolRegistry};
use crate::value::Value;

/// One predicted host call. Arguments are the cache key, so they must be the
/// values the script will actually pass.
#[derive(Clone, Debug)]
pub struct PredictedCall {
    pub tool: String,
    pub args: Vec<Value>,
    pub kwargs: BTreeMap<String, Value>,
}

impl PredictedCall {
    pub fn new(tool: impl Into<String>, args: Vec<Value>) -> Self {
        Self {
            tool: tool.into(),
            args,
            kwargs: BTreeMap::new(),
        }
    }

    pub fn with_kwargs(mut self, kwargs: BTreeMap<String, Value>) -> Self {
        self.kwargs = kwargs;
        self
    }
}

/// Proposes tool calls before the script that will use them is ready.
///
/// `hint` is caller-defined. The mock benchmark passes the task name. A live
/// agent can pass the user request or the script prefix received so far.
pub trait Predictor: Send + Sync {
    fn predict(&self, hint: &str) -> Vec<PredictedCall>;
}

impl<F> Predictor for F
where
    F: Fn(&str) -> Vec<PredictedCall> + Send + Sync,
{
    fn predict(&self, hint: &str) -> Vec<PredictedCall> {
        self(hint)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpecStats {
    /// Predictions that were launched.
    pub started: u64,
    /// Program calls served from the cache.
    pub hits: u64,
    /// Pure or read program calls with no cache entry.
    pub misses: u64,
    /// Launched predictions the program never used.
    pub wasted: u64,
    /// Predictions that were not started (missing tool, wrong effect, bad arity, or duplicate).
    pub rejected: u64,
    /// Cached reads dropped because the script wrote an overlapping resource first.
    pub invalidated: u64,
}

impl SpecStats {
    /// `hits / (hits + misses)` for the program's pure and read calls.
    /// A run with no such calls reports `0.0`.
    pub fn hit_rate(self) -> f64 {
        let denom = self.hits + self.misses;
        if denom == 0 {
            0.0
        } else {
            self.hits as f64 / denom as f64
        }
    }
}

struct Entry {
    current: Mutex<Option<Result<Value, String>>>,
    cv: Condvar,
    consumed: AtomicBool,
    /// `None` for pure tools, which no write can invalidate.
    reads: Option<Footprint>,
}

impl Entry {
    fn wait(&self) -> Result<Value, String> {
        let mut guard = self.current.lock().expect("spec entry");
        while guard.is_none() {
            guard = self.cv.wait(guard).expect("spec entry");
        }
        guard.clone().expect("spec entry filled")
    }
}

struct Inner {
    entries: Mutex<HashMap<String, Arc<Entry>>>,
    started: AtomicU64,
    hits: AtomicU64,
    misses: AtomicU64,
    rejected: AtomicU64,
    invalidated: AtomicU64,
}

/// Cache of in-flight and finished speculative calls.
///
/// Clone shares the same entries. Dropping every clone does not cancel work
/// that has already started; those threads finish and their results are discarded.
#[derive(Clone)]
pub struct SpecCache {
    inner: Arc<Inner>,
}

impl SpecCache {
    pub(crate) fn start(tools: &ToolRegistry, calls: &[PredictedCall]) -> Self {
        let cache = Self {
            inner: Arc::new(Inner {
                entries: Mutex::new(HashMap::new()),
                started: AtomicU64::new(0),
                hits: AtomicU64::new(0),
                misses: AtomicU64::new(0),
                rejected: AtomicU64::new(0),
                invalidated: AtomicU64::new(0),
            }),
        };
        for call in calls {
            cache.launch(tools, call);
        }
        cache
    }

    pub fn stats(&self) -> SpecStats {
        let wasted = self
            .inner
            .entries
            .lock()
            .expect("spec entries")
            .values()
            .filter(|entry| !entry.consumed.load(Ordering::SeqCst))
            .count() as u64;
        SpecStats {
            started: self.inner.started.load(Ordering::SeqCst),
            hits: self.inner.hits.load(Ordering::SeqCst),
            misses: self.inner.misses.load(Ordering::SeqCst),
            wasted,
            rejected: self.inner.rejected.load(Ordering::SeqCst),
            invalidated: self.inner.invalidated.load(Ordering::SeqCst),
        }
    }

    /// Drop every cached read whose footprint overlaps `writes`. Called before
    /// a reversible or irreversible call runs. Unconsumed dropped entries are
    /// counted in [`SpecStats::invalidated`]; already-consumed ones are just
    /// removed so a repeat of the read runs live.
    pub(crate) fn invalidate(&self, writes: &Footprint) {
        let mut entries = self.inner.entries.lock().expect("spec entries");
        let mut dropped = 0u64;
        entries.retain(|_, entry| {
            let hit = entry.reads.as_ref().is_some_and(|reads| reads.overlaps(writes));
            if hit && !entry.consumed.load(Ordering::SeqCst) {
                dropped += 1;
            }
            !hit
        });
        self.inner.invalidated.fetch_add(dropped, Ordering::SeqCst);
    }

    pub(crate) fn miss(&self) {
        self.inner.misses.fetch_add(1, Ordering::SeqCst);
    }

    /// Wait for a speculative result. `None` means this call was not predicted.
    pub(crate) async fn take(
        &self,
        tool: &str,
        args: &[Value],
        kwargs: &BTreeMap<String, Value>,
    ) -> Option<Result<Value, String>> {
        let key = call_key(tool, args, kwargs);
        let entry = {
            let entries = self.inner.entries.lock().expect("spec entries");
            entries.get(&key)?.clone()
        };
        entry.consumed.store(true, Ordering::SeqCst);
        self.inner.hits.fetch_add(1, Ordering::SeqCst);
        Some(await_entry(entry).await)
    }

    fn launch(&self, tools: &ToolRegistry, call: &PredictedCall) {
        let Some(tool) = tools.get(&call.tool) else {
            self.inner.rejected.fetch_add(1, Ordering::SeqCst);
            return;
        };
        if !matches!(tool.spec.effect, Effect::Pure | Effect::Read) {
            self.inner.rejected.fetch_add(1, Ordering::SeqCst);
            return;
        }
        if let Some(arity) = tool.spec.arity {
            if call.args.len() != arity {
                self.inner.rejected.fetch_add(1, Ordering::SeqCst);
                return;
            }
        }
        let key = call_key(&call.tool, &call.args, &call.kwargs);
        let entry = Arc::new(Entry {
            current: Mutex::new(None),
            cv: Condvar::new(),
            consumed: AtomicBool::new(false),
            reads: match tool.spec.effect {
                Effect::Pure => None,
                _ => Some(tool.spec.reads.clone()),
            },
        });
        {
            let mut entries = self.inner.entries.lock().expect("spec entries");
            if entries.contains_key(&key) {
                self.inner.rejected.fetch_add(1, Ordering::SeqCst);
                return;
            }
            entries.insert(key, entry.clone());
        }
        self.inner.started.fetch_add(1, Ordering::SeqCst);
        let body = tool.body.clone();
        let args = call.args.clone();
        let kwargs = call.kwargs.clone();
        let timeout = tool.spec.timeout;
        let retries = tool.spec.retries;
        let name = call.tool.clone();
        thread::spawn(move || {
            let result = block_on_tool(&name, &body, &args, &kwargs, retries, timeout);
            {
                let mut slot = entry.current.lock().expect("spec entry");
                *slot = Some(result);
            }
            entry.cv.notify_all();
        });
    }
}

async fn await_entry(entry: Arc<Entry>) -> Result<Value, String> {
    tokio::task::spawn_blocking(move || entry.wait())
        .await
        .unwrap_or_else(|_| Err("speculation worker stopped".to_string()))
}

fn block_on_tool(
    tool_name: &str,
    body: &ToolBody,
    args: &[Value],
    kwargs: &BTreeMap<String, Value>,
    retries: u32,
    timeout: Option<Duration>,
) -> Result<Value, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("speculation runtime");
    let (_tx, cancel) = watch::channel(false);
    runtime.block_on(async {
        let handle = Handle::current();
        match crate::exec::run_tool_attempts(
            0,
            0,
            tool_name,
            &handle,
            body,
            args,
            kwargs,
            retries,
            timeout,
            &cancel,
        )
        .await
        {
            Ok(value) => Ok(value),
            Err(err) => Err(err.summary()),
        }
    })
}

pub fn call_key(tool: &str, args: &[Value], kwargs: &BTreeMap<String, Value>) -> String {
    let payload = serde_json::json!({
        "tool": tool,
        "args": args.iter().map(Value::to_json).collect::<Vec<_>>(),
        "kwargs": kwargs
            .iter()
            .map(|(key, value)| (key.clone(), value.to_json()))
            .collect::<Vec<_>>(),
    });
    payload.to_string()
}
