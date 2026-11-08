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

/// How the host function is invoked.
#[derive(Clone)]
pub enum ToolBody {
    /// Blocking function. A timeout abandons the wait. The thread is not signalled.
    Sync(ToolFn),
    /// Blocking function that receives a token and should return when it fires.
    Cooperative(CooperativeFn),
    /// Async function. Timeout and cancel drop the future.
    Async(AsyncToolFn),
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
}

impl Default for ToolSpec {
    fn default() -> Self {
        Self {
            effect: Effect::Irreversible,
            arity: None,
            accepts_kwargs: false,
            retries: 0,
            timeout: None,
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
