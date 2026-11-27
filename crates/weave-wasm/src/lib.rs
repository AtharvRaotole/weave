//! Weave in the browser: the real parser, pre-run check and scheduler, with
//! a handful of mock tools that sleep on a virtual clock.
//!
//! Nothing here talks to a network. Tool latencies are fixed per tool (and
//! per argument for `weather`), so every run of the same script gives the
//! same schedule.
#![cfg(any(target_arch = "wasm32", feature = "virtual-time"))]

mod mock;

pub use mock::{check_json, run_json, tools_json};

#[cfg(target_arch = "wasm32")]
mod bindings {
    use wasm_bindgen::prelude::*;

    /// Run the pre-run check and, when it passes, the script. Returns JSON.
    #[wasm_bindgen]
    pub fn run(source: &str) -> String {
        super::run_json(source)
    }

    /// Only the pre-run check. Returns JSON.
    #[wasm_bindgen]
    pub fn check(source: &str) -> String {
        super::check_json(source)
    }

    /// The mock tools: name, effect, parameters, latency. Returns JSON.
    #[wasm_bindgen]
    pub fn tools() -> String {
        super::tools_json()
    }

    #[wasm_bindgen]
    pub fn version() -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }
}
