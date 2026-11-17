# Contributing to Weave

Thanks for helping. Bug reports, failing scripts, and pull requests are all welcome.
Please follow the [code of conduct](CODE_OF_CONDUCT.md).

## Finding something to work on

Issues labeled [`good first issue`](https://github.com/AtharvRaotole/weave/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22)
are scoped for a newcomer: a missing language quirk, a docs table, a small adapter, or a test gap.
[`help wanted`](https://github.com/AtharvRaotole/weave/issues?q=is%3Aissue+is%3Aopen+label%3A%22help+wanted%22)
marks work we would particularly like a hand with. The [roadmap](ROADMAP.md) has the larger items.

Use the [bug](https://github.com/AtharvRaotole/weave/issues/new?template=bug_report.yml) or
[feature](https://github.com/AtharvRaotole/weave/issues/new?template=feature_request.yml) templates
for anything new. For a security problem, follow [SECURITY.md](SECURITY.md) instead of opening a public issue.

## Reporting a bug

Open an issue with the script, the tools it called (name, effect, arity), what it
printed, and what you expected. A trace (`result.trace_json()`) makes most bugs
reproducible without your tools.

## Building and testing

You need Rust (CI and `rust-toolchain.toml` pin 1.99.1; the crate MSRV in `Cargo.toml` is 1.97)
and Python 3.9 or newer.

```bash
# Rust unit, runtime, and differential tests
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings

# Python bindings (maturin develops an editable wheel into the venv)
python3 -m venv .venv
.venv/bin/pip install 'maturin>=1.8,<2'
.venv/bin/maturin develop --release
.venv/bin/python tests/python/test_agent_tool.py
.venv/bin/python tests/python/test_integrations.py
.venv/bin/python tests/audit/test_python_sugar.py
.venv/bin/python examples/speedup.py
```

CI (`.github/workflows/ci.yml`) also runs every script in `examples/`, the CLI,
and the MCP client against `weave-mcp`. Run the ones your change touches.

`WEAVE_DIFF_SEEDS=5000 cargo test -p weave --test differential` runs the
differential fuzzer longer. Scheduler changes should pass a long run.

## Pull requests

- Keep each pull request to one change, with a test that fails without it.
  Scheduler and language changes need a case in `crates/weave/tests/runtime.rs`
  and, where possible, a pattern the differential fuzzer can generate.
- Update `CHANGELOG.md` under the unreleased section.
- Language changes should also update `docs/language.html`.
- Benchmark numbers in docs must come from a file in `bench/results/`. Cite the
  file. Do not edit numbers by hand; rerun the benchmark instead.
  `python bench/make_charts.py` redraws the README chart from the results.
- New language features should fail with a one-line diagnostic and a hint when
  misused, because the main reader of those errors is a model.

## Benchmarks that call models

`bench/llm_eval.py` and `bench/real_tools_eval.py` call the OpenAI API and cost
money. Set `WEAVE_EVAL_BUDGET` to cap spend. Commit the result JSON along with
any claim it supports.

By contributing you agree that your contributions are licensed under the
[MIT License](LICENSE).
