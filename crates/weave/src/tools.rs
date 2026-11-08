use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::cancel::CancelToken;
use crate::effect::Effect;
use crate::value::Value;

pub type ToolFn = Arc<
    dyn Fn(Vec<Value>, BTreeMap<String, Value>) -> Result<Value, String> + Send + Sync,
>;

pub type CooperativeFn = Arc<
    dyn Fn(Vec<Value>, BTreeMap<String, Value>, CancelToken) -> Result<Value, String> + Send + Sync,
>;

pub type AsyncToolFn = Arc<
    dyn Fn(Vec<Value>, BTreeMap<String, Value>, CancelToken) -> AsyncToolFut + Send + Sync,
>;

pub type AsyncToolFut = Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>;

pub type IsolatedStart = Arc<
    dyn Fn(Vec<Value>, BTreeMap<String, Value>) -> Result<IsolatedJob, String> + Send + Sync,
>;

/// A sync tool call running where the host can destroy it.
///
/// `wait` blocks until the call returns. `kill` must make `wait` unblock and
/// must stop the underlying work. The Python worker pool implements `kill`
/// with `SIGKILL`. A plain thread cannot do this safely, so Rust sync tools
/// stay on [`ToolBody::Sync`] unless the host supplies this pair.
pub struct IsolatedJob {
    wait: Arc<dyn Fn() -> Result<Value, String> + Send + Sync>,
    kill: Arc<dyn Fn() + Send + Sync>,
}

impl IsolatedJob {
    pub fn new(
        wait: impl Fn() -> Result<Value, String> + Send + Sync + 'static,
        kill: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self {
            wait: Arc::new(wait),
            kill: Arc::new(kill),
        }
    }

    pub(crate) fn waiter(&self) -> Arc<dyn Fn() -> Result<Value, String> + Send + Sync> {
        self.wait.clone()
    }

    pub(crate) fn kill(&self) {
        (self.kill)()
    }
}

/// How the host function is invoked.
#[derive(Clone)]
pub enum ToolBody {
    /// Blocking function. A timeout abandons the wait. The thread is not signalled.
    Sync(ToolFn),
    /// Blocking function that receives a token and should return when it fires.
    Cooperative(CooperativeFn),
    /// Async function. Timeout and cancel drop the future.
    Async(AsyncToolFn),
    /// Blocking work with a host-provided kill. Timeout and cancel call `kill`.
    Isolated(IsolatedStart),
}

/// How a tool is registered. Omitted effect means irreversible. Omitted arity means any arity.
#[derive(Clone, Debug)]
pub struct ToolSpec {
    pub effect: Effect,
    pub arity: Option<usize>,
    pub accepts_kwargs: bool,
    /// Extra attempts after a failure or timeout. Zero means one try.
    pub retries: u32,
    pub timeout: Option<Duration>,
    /// Resources this tool reads. Used to decide which speculated results a
    /// write invalidates. Default: every resource.
    pub reads: Footprint,
    /// Resources this tool writes (only meaningful for reversible and
    /// irreversible tools). Default: every resource, so any write
    /// invalidates every cached speculative read.
    pub writes: Footprint,
}

/// A set of named resources a tool touches.
///
/// `All` overlaps with everything. `Only(names)` overlaps with another
/// footprint when they share a name. An empty `Only` touches nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Footprint {
    #[default]
    All,
    Only(Vec<String>),
}

impl Footprint {
    pub fn only<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::Only(names.into_iter().map(Into::into).collect())
    }

    /// Parse a comma-separated list. `*` means [`Footprint::All`].
    pub fn parse(text: &str) -> Self {
        let names: Vec<String> = text
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect();
        if names.iter().any(|name| name == "*") {
            Self::All
        } else {
            Self::Only(names)
        }
    }

    pub fn overlaps(&self, other: &Footprint) -> bool {
        match (self, other) {
            (Self::All, _) | (_, Self::All) => true,
            (Self::Only(a), Self::Only(b)) => a.iter().any(|name| b.contains(name)),
        }
    }
}

impl Default for ToolSpec {
    fn default() -> Self {
        Self {
            effect: Effect::Irreversible,
            arity: None,
            accepts_kwargs: false,
            retries: 0,
            timeout: None,
            reads: Footprint::All,
            writes: Footprint::All,
        }
    }
}

impl ToolSpec {
    pub fn effect(effect: Effect) -> Self {
        Self {
            effect,
            ..Self::default()
        }
    }

    pub fn with_arity(mut self, arity: usize) -> Self {
        self.arity = Some(arity);
        self
    }

    pub fn with_retries(mut self, retries: u32) -> Self {
        self.retries = retries;
        self
    }

    /// Declare the resources this tool reads.
    pub fn with_reads(mut self, reads: Footprint) -> Self {
        self.reads = reads;
        self
    }

    /// Declare the resources this tool writes.
    pub fn with_writes(mut self, writes: Footprint) -> Self {
        self.writes = writes;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

#[derive(Clone)]
pub struct Tool {
    pub spec: ToolSpec,
    pub body: ToolBody,
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Tool>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, name: impl Into<String>, spec: ToolSpec, func: ToolFn) {
        self.insert(name, spec, ToolBody::Sync(func));
    }

    pub fn register_cooperative(&mut self, name: impl Into<String>, spec: ToolSpec, func: CooperativeFn) {
        self.insert(name, spec, ToolBody::Cooperative(func));
    }

    pub fn register_async(&mut self, name: impl Into<String>, spec: ToolSpec, func: AsyncToolFn) {
        self.insert(name, spec, ToolBody::Async(func));
    }

    pub fn register_isolated(&mut self, name: impl Into<String>, spec: ToolSpec, start: IsolatedStart) {
        self.insert(name, spec, ToolBody::Isolated(start));
    }

    fn insert(&mut self, name: impl Into<String>, spec: ToolSpec, body: ToolBody) {
        self.tools.insert(name.into(), Tool { spec, body });
    }

    pub fn get(&self, name: &str) -> Option<&Tool> {
        self.tools.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tools.keys().map(String::as_str)
    }
}
