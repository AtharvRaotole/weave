# Browser playground

<https://atharvraotole.github.io/weave/>

The playground runs the real Weave crate in the browser: the parser, the
pre-run check (unknown tools, unknown or missing arguments, literal type and
enum checks, with "did you mean" hints) and the scheduler. It is built from
`crates/weave-wasm` and the static files in `playground/`, and is deployed by
`.github/workflows/pages.yml`.

## How it runs without threads

Native Weave uses a multi-threaded tokio runtime with real timers, and runs
blocking tools on threads. Browsers have neither, so wasm32 builds (and native
builds with the `virtual-time` feature) switch to `src/rt.rs`'s virtual mode:

- one current-thread tokio runtime, driven only while a run is finishing;
- a virtual clock. Every spawned task counts its polls. When a full pass of
  the scheduler makes no progress, the clock jumps to the earliest pending
  sleep and wakes it;
- tool latency, per-call timeouts and the wall-clock limit all use that clock;
- sync, cooperative and isolated tools run inline. Speculation is off.

The scheduling code itself is the same code native builds use. Only the
runtime, timers and thread helpers differ.

## The mock tools

`weather`, `search`, `gh_issues`, `db_get`, `db_put`, `summarize`,
`post_slack` and `send_email`. Each declares an effect, parameters (with small
JSON Schemas) and a fixed latency, and `db_get` / `db_put` declare footprints
(`db:{key}`). Nothing goes over the network. Latencies are made up for the
demo, so the times on the page show the shape of a schedule, not how fast any
real service is.

## Build and test locally

```bash
rustup target add wasm32-unknown-unknown
# a wasm-bindgen CLI matching the version in Cargo.lock
cargo test -p weave --features virtual-time --test virtual_time
cargo test -p weave-wasm --features virtual-time
WASM_BINDGEN=/path/to/wasm-bindgen scripts/build-playground.sh
(cd target/playground-site && python3 -m http.server 8765)
python playground/tests/smoke.py http://localhost:8765/
```

## Known limits

- Times are virtual. Real tools vary more, and there is no model in the loop.
- Sync tools can't be timed out in virtual mode (they run inline), and
  speculation is disabled. Only the mock tools are available.
- MCP and Python tools aren't available in the browser.
- The runtime is about 2 MB of WebAssembly (about 850 KB gzipped).
