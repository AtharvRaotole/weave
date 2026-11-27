# Browser playground

<https://atharvraotole.github.io/weave/>

The playground runs the actual Weave crate in your browser. That means the
real parser, the real check (unknown tools, wrong or missing arguments, bad
literal types and enum values, wrong keys on declared results, all with "did
you mean" hints), the plan, and the scheduler. The Rust side lives in
`crates/weave-wasm`, the page lives in `playground/`, and
`.github/workflows/pages.yml` tests it and pushes it to the `gh-pages` branch,
which is what GitHub Pages serves.

## How it runs without threads

On your machine Weave uses a multi-threaded tokio runtime with real timers and
runs blocking tools on threads. Browsers give you neither, so wasm32 builds
(and native builds with the `virtual-time` feature) switch to a virtual mode
in `src/rt.rs`:

- One single-threaded tokio runtime, driven only while a run is finishing.
- A virtual clock. Every task counts its polls. When a full pass makes no
  progress, the clock jumps to the next pending sleep and wakes it.
- Tool delays, per-call timeouts and the overall time limit all use that clock.
- Sync, cooperative and isolated tools run inline. Speculation is off.

The scheduling code is the same code native builds run. Only the runtime,
timers and thread helpers are swapped.

## The fake tools

`weather`, `search`, `gh_issues`, `db_get`, `db_put`, `summarize`,
`post_slack` and `send_email`. Each one declares an effect, its parameters
(small JSON Schemas), what it returns, and a fixed delay. `db_get` and
`db_put` also say which key they touch (`db:{key}`). Nothing goes over the
network. The delays are made up for the demo, so the page shows the shape of a
schedule, not how fast any real service is.

## Build and test it yourself

```bash
rustup target add wasm32-unknown-unknown
# you also need a wasm-bindgen CLI that matches the version in Cargo.lock
cargo test -p weave --features virtual-time --test virtual_time
cargo test -p weave-wasm --features virtual-time
WASM_BINDGEN=/path/to/wasm-bindgen scripts/build-playground.sh
(cd target/playground-site && python3 -m http.server 8765)
python playground/tests/smoke.py http://localhost:8765/
```

The page uses Inter and JetBrains Mono, both under the SIL Open Font License.
The font files and their licenses are in `playground/fonts/` and ship with the
site, so it doesn't load anything from a CDN.

## What it can't do

- Times are virtual. Real tools are messier, and there's no model in the loop.
- Only the fake tools are there. No MCP or Python tools in the browser.
- Sync tools can't time out in virtual mode since they run inline, and
  speculation is off.
- The runtime is about 2 MB of WebAssembly (about 850 KB gzipped).
