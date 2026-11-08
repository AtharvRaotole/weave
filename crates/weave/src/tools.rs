use std::collections::BTreeMap;
use std::sync::Arc;

use crate::effect::Effect;
use crate::value::Value;

pub type ToolFn = Arc<
    dyn Fn(Vec<Value>, BTreeMap<String, Value>) -> Result<Value, String> + Send + Sync,
>;

/// How a tool is registered. Omitted effect means irreversible. Omitted arity means any arity.
#[derive(Clone, Debug)]
pub struct ToolSpec {
    pub effect: Effect,
    pub arity: Option<usize>,
    pub accepts_kwargs: bool,
}

impl Default for ToolSpec {
    fn default() -> Self {
        Self {
            effect: Effect::Irreversible,
            arity: None,
            accepts_kwargs: false,
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
}

#[derive(Clone)]
pub struct Tool {
    pub spec: ToolSpec,
    pub func: ToolFn,
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
        let name = name.into();
        self.tools.insert(name, Tool { spec, func });
    }

    pub fn get(&self, name: &str) -> Option<&Tool> {
        self.tools.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tools.keys().map(String::as_str)
    }
}
