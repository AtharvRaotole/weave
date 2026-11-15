# Changelog

## [Unreleased]

### Adoption (round 8)
- `WeaveTool.langchain_tool()` (and `weave.integrations.langchain_tool`): a LangChain `StructuredTool` for LangChain
  and LangGraph (`ToolNode`, `bind_tools`, `create_react_agent`); async calls cancel the script session.
- `weave mcp-config [--client claude|cursor] [--config FILE] [--binary PATH] [--name NAME] [--write]`: prints or
  merges the `weave-mcp` entry into Claude Desktop's or Cursor's MCP config (absolute paths, `.bak` kept).
- `scripts/install.sh`: one-command install from a GitHub release's prebuilt wheels (`--mcp` also builds weave-mcp).
- Examples: `examples/langgraph_agent.py`, `examples/agents_sdk_agent.py` (offline scripted models, `--live`), demo
  apps `examples/apps/dep_audit.py` (PyPI + OSV.dev; offline synthetic fixtures) and `examples/apps/repo_scan.py`
  (many git repos in one script). CI runs all of them offline, plus `tests/python/test_integrations.py`.

### Benchmarks (round 7)
- tau-bench runner: the in-flight budget guard now waits for running trials instead of skipping a trial that does
  not fit (it skips only when nothing is running) and lists skipped trials in `run.json`.
- tau tables report the input/cached/output token split, list-price dollars (with a Weave/native ratio CI) and host
  time per arm.
- Round 7 results (`retail_test12r7`, Addendum 7): `bench/results/v10/`, `assets/benchmark-v10-tau.*`.

### Agent tool and benchmarks (round 6)
- `WeaveTool(conversational=True)` adds rules for hosts where a person replies between turns: fetch everything the
  next reply needs in one script, and change no state before the person confirms that exact change.
- `WeaveTool(persist_variables=True)`: later scripts see the variables earlier scripts set (snapshots, passed as
  literal assignments; error line numbers still refer to the model's script).
- `weave.agent_tool.shape_of_samples`: one return shape from several real results. Id-keyed dicts become
  `dict[str, V]`, free-form string maps become `dict[str, str]`, and fields missing from some samples are marked `?`.
- The static check no longer rejects a name assigned in one branch (or a `try`) when the other branch or handler ends
  in `continue`, `break`, `return` or `raise`.
- tau-bench adapter: return shapes sampled with `Tool.invoke` on a copy of the data (never `env.step`) and cached. In
  the round-6 evidence run they were not cached, which cost about 2.2 s per Weave trial.
- Round 6 tau-bench retail results (`retail_test10r6`, Addendum 6): `bench/results/v9/`, `assets/benchmark-v9-tau.*`.

### Language (round 6)
- Iterating a dict yields its keys (`for k in d`, `sorted(d)`, `min(d)`, `enumerate(d)`, comprehensions); strings
  iterate as characters in those builtins too.
- `except Exception`, `except BaseException`, builtin classes (`KeyError`, `IndexError`, `LookupError`, `TypeError`,
  `ValueError`, `AttributeError`, `ZeroDivisionError`, `ArithmeticError`, `RuntimeError`, `AssertionError`) and tuples
  of them. Each catches only its kind; `except Exception` also catches tool failures.
- `raise SomeError(msg)` and `assert cond, msg`; an irreversible call after a raise is never issued.
- `isinstance(x, T)` over builtin types, `round` (half to even), `dict(...)`, `tuple(...)`, `json.dumps`/`json.loads`
  without an import.
- f-string format specs (`{x:.2f}`, `{n:,}`, `{s:>8}`, `{p:.1%}`), `str.format` with positional fields, printf-style
  `%` formatting.
- `d.update(...)`, `d.pop(k[, default])` and `xs.pop([i])` as statements.
- Indexing a str or list with a key names the value it actually got (it was `string index must be int`).
- Fixes found by the new CPython differential suite (`tests/audit/test_python_idioms.py`, 63 fixtures + 600
  generated programs, batch/stream/replay): `//` and `%` now follow Python's sign rules for negative operands (they
  used Euclidean division), and floats print in scientific notation below 1e-4 and from 1e16, as `repr` does.

### Benchmarks (round 5)
- `bench/taubench/`: a Weave arm for tau-bench (sierra-research/tau-bench, MIT, pinned `59a200c`) next to its own
  `ToolCallingAgent`, with a metering wrapper that applies the same request settings to both arms.
- `weave_ptc` and `native_noptc` arms for a symmetric `parallel_tool_calls` ablation; `confirm5` suite.
- Both runners reserve tokens for every trial in flight before starting one (`--reserve-per-trial`); round 5 overspent
  without it.

### Agent tool
- `WeaveTool.request_options` returns `{"parallel_tool_calls": False}` for hosts to pass with the tool, and the tool
  rules say to call it once per turn (scripts do not share variables). GPT-4.1 otherwise split tasks across two
  parallel scripts and sometimes ran away to `max_output_tokens` (`bench/results/v7/diag-turn1/NOTES.md`).
- `examples/demo.py`: the offline README demo (13 tool calls in 4 overlapping waves, measured); run in CI.

### Benchmark harness
- An `incomplete` Responses result (billed in full) now adds its provider-reported usage to the trial
  (`billed_on_error`) with an output excerpt (`partial_output`); every attempt logs its usage. Previously 0.
- `confirm4` suite (Addendum 4) and the dev-only `real_dev_git` suite.

### Language
- List, dict and set comprehensions and generator expressions (nested `for`/`if`), lowered to ordinary pipelined
  loops: `[get(k) for k in keys]` issues its calls in parallel. `any`/`all` over a generator stop at the first
  decisive item, as in Python.
- `key=` (a one-argument `lambda`, a tool, builtin, helper, `str.lower`, or a bound method like `counts.get`),
  `reverse=` and `default=` on `sorted`, `min`, `max` and `xs.sort()`; stable, first extreme wins, as in CPython.
  `map(f, xs)`, `filter(f, xs)`, `xs.reverse()`.
- Top-level `def` helpers with positional, keyword and literal-default parameters and `return` anywhere. Each call
  is inlined with renamed locals, so effects, scheduling, replay and limits see the inlined code. Recursion, nested
  `def`, `*args`, non-literal defaults, use before `def`, and changing an argument in place are rejected with hints.
- Sets as duplicate-free lists: `{a, b}`, `set(xs)`, set comprehensions, `s.add(v)`.
- `tests/audit/test_python_sugar.py`: generated programs vs CPython (batch, streamed, replayed); the Rust
  differential fuzzer generates comprehensions, a helper with an early return and a write, `key=` lambdas and
  short-circuit `any`.

### Runtime
- Footprint-aware ordering: a read waits only for earlier writes whose declared footprint overlaps its own
  (`declare(reads=[...], writes=[...])`, templates such as `"ticket:{ticket_id}"`, positional or keyword). `if`/`for`
  blocks that may write carry a footprint computed from their arguments, or from what they actually wrote. Undeclared
  tools keep the old order.
- Loops whose bodies write are pipelined too. Calls behind a condition or `break` are still never issued early.
- Footprint templates accept `{index|name}` alternatives, so declarations from Python resolve for positional and
  keyword calls.
- Read-only `for` loops are pipelined (windows of 64 iterations), including accumulators, dict fills and
  appends; `break` and guarded calls are never issued early; replay stays exact. `Limits::with_pipeline_loops`.
- `total += tool()` polls both sides concurrently.
- Fixed: `d[k] = v` inside a parallel loop dropped every key.
- Names assigned in a loop are defined after it; an unbound name is a runtime error, not empty output.
- Lists compare lexicographically.
- Errors a model can repair from: value descriptions ("dict with keys 'value'"), missing-key lists, `bad call:
  ...; call it as f(p=...)`, `functions.f` → `f`, `while`, `lambda` and `key=` hints.
- `Engine.script_effect(source)`.

### Agent tool
- Listings show return types (`name(params) -> type, effect E`), from annotations or `__weave_returns__`.
- `WeaveTool.handle_many` / `run_many`: the read-only scripts of one turn run concurrently.
- A script's final bare expression is printed.
- Rules ask for few scripts and printing only what is needed.

### Benchmarks
- Benchmark v4 (`bench/v4`, `make bench-mock`, `make bench-live`), pre-registered held-out results on three
  OpenAI models, scaling, ablation, confirm suite, partial Gemini run, and a protocol-v3 rerun. Paper in
  `docs/paper/`.
- Round 2: `confirm2` and real-services (`real`: SQLite, git, filesystem, local HTTP, PyPI) suites, pre-registered in
  Addendum 2; `weave_nofp` arm; `--min-interval-s` request pacing. Results in `bench/results/v5/`.

### Benchmarks
- First live protocol-v3 comparison (2026-11-09): `gpt-5.5-2026-04-23`,
  `gpt-5.4-mini-2026-03-17` and `gpt-4.1-2025-04-14`, ten frozen
  file/git/memory tasks over the real `weave-mcp` binary, 60 trials per model.
  Success is on par with native tool calling; no speedup is claimed. Results,
  ledgers and charts: `docs/benchmarks.md`, `bench/results/protocol-v3-live/`,
  `assets/benchmark-v3.png`.
- `bench/protocol_v3/live.py` (opt-in live runner), `live_tasks.py` (frozen
  task set), `report.py` (tables) and `bench/make_v3_charts.py` (figures).
- Responses adapter: several `run_weave` calls in one turn run in order with
  their own outputs; unsent generation fields; configurable request timeout.

### Documentation and examples
- `docs/getting-started.md`: where Weave runs, install paths that work today,
  and recipes for scripts, model tools, MCP clients, cancellation and the CLI.
- `examples/quickstart.py` and `examples/cancel_run.py`, run in CI.
- README: benchmark section replaced with the live protocol-v3 results; the
  older unequal-endpoint numbers moved to the history section of
  `docs/benchmarks.md`.

## [1.0.0] - 2026-11-09

First release built with the independent-audit fixes and production hardening.
Guarantees, non-guarantees and the threat model: `docs/production.md`.
v0.1.0 stays published unchanged as a historic experimental release.

### Breaking changes from 0.1.0
- Python distribution renamed to `weave-lang` (import name stays `weave`);
  `weave.__version__` reads the `weave-lang` metadata.
- Host exception text no longer reaches scripts: raise `weave.ToolError` for a
  model-visible message, or use `debug=True`.
- `weave-mcp` upstreams start with an allowlisted environment and discarded
  stderr (`env`, `inherit_env`, `stderr` to opt in); upstream error payloads
  are withheld unless the upstream sets `debug`.
- `weave_run` / `weave_replay` return stdout/status only unless `debug: true`.
- Trace v1 is rejected; exact replay needs trace v2 with identical source and
  tool declarations.
- Write retries are off unless a tool is declared `retry_safe=True`.

### Release, packaging and documentation
- `docs/production.md`: supported production posture, threat model, each
  guarantee with the test that holds it, and inherent non-guarantees.
- CI `python-cross` job runs the isolation, privacy and bounded-phase suites on
  macOS and Windows; release builds run the isolation suite on each OS.
- README, SECURITY.md (supported versions) and the audit ledger updated.

### Cancellation propagation
- `Session.cancel_handle()` returns a thread-safe `SessionCancel`.
- `WeaveTool.run_async()` and the Agents SDK tool cancel the script session when
  the awaiting task is cancelled, so isolated workers are killed instead of
  running on after the agent gave up.
- Fixed quadratic run and replay cost in call-heavy loops: the write-gating
  dependency list is folded logarithmically and replay matches calls through
  an index instead of rescanning the trace (6000 tool calls in one loop: run
  ~6.3 s -> ~0.3 s, replay ~5.1 s -> ~0.1 s, release build, one Linux box).
  `test_bounded_phases.py` fails if growth stops being linear.
- `tests/python/test_bounded_phases.py` bounds the phases cancellation cannot
  interrupt (parse, check, replay) by the input budgets.

### MCP schema forwarding and reference-server compatibility
- `weave-mcp` forwards upstream `outputSchema`, `title` and `annotations`
  (with `readOnlyHint` set from the effect Weave enforces); the `weave_run`
  listing shows the declared return type. `serverInfo.version` is the crate version.
- CI job `reference-mcp` runs `weave-mcp` against the official filesystem,
  memory (npm, lockfile) and git (PyPI, hash-pinned) reference servers: real
  file writes, a real commit and a real memory graph through one `weave_run`,
  checked on disk; forwarded output schemas and `structuredContent` verified.
- The benchmark fixture now checks discovered output schemas against its pins.
- Structured results are not validated against `outputSchema` by Weave.

### Privacy hardening
- **Behavior change:** Python host exceptions (in-process, cooperative and
  worker tools) reach scripts as the exception type only. Raise
  `weave.ToolError("...")` for model-visible messages, or use `debug=True`.
- **Behavior change:** `weave-mcp` discards upstream stderr by default
  (`"stderr": "inherit" | {"file": path}` to keep it) and passes upstreams an
  allowlisted environment plus explicit `"env"` (`"inherit_env": true` restores
  the old behavior).
- Upstream `isError` text, JSON-RPC error payloads and failed example samples
  are withheld unless the upstream sets `"debug": true`.
- Planted-secret tests for every channel; see docs/privacy.md.

### Process isolation
- `Engine(worker_pool=N, isolate_all=True)` and `WeaveTool(..., worker_pool=N)`
  run every host tool in a killable worker; tools opt out only with an explicit
  `hard_deadline=False`. `engine.isolation()` reports what is enforced.
- POSIX workers lead their own process group; timeouts, run deadlines and
  cancellation `SIGKILL` the group, including subprocesses a tool started.
- `weave.WorkerLimits` (memory/CPU/open files/file size via `setrlimit`) fail
  closed where the OS cannot enforce them; worker results are capped at 16 MiB.
- Worker exceptions cross as the exception type only unless they are
  `weave.ToolError` or `debug=True` (**behavior change**).
- Unpickleable tools are rejected at registration when isolation is requested.
- See docs/isolation.md for exact guarantees and non-guarantees.

### Finite protocol-v3 adapter stage
- Connect shipped WeaveTool/MCP execution and native Responses wire format to
  synthetic loopback HTTP and real disposable filesystem/git/memory MCP fixtures.
- Pin actual artifacts, enforce equal backend concurrency/call controls, and retain
  append-only durable trial/attempt/HTTP accounting with explicit missing usage.
- Keep remote spending disabled by default with dual explicit opt-in; no live runs,
  unseen evaluation, releases or model-performance claims.

### Benchmark protocol remediation
- Add deterministic offline controlled-comparison tests for endpoints, full-context
  parity, native parallel reads/equal backend caps, real state/write validation,
  configuration pins and retry-inclusive failure/usage accounting.
- Separate public development/candidate-held-out tasks without independence claims;
  retire the legacy v2 real-tools live CLI. Corrected live runs remain pending.

### Safety follow-up
- Responsive MCP request-ID cancellation with best-effort upstream forwarding,
  EOF cleanup, bounded admission and duplicate active-ID rejection.
- Reversible/irreversible retries require explicit host idempotency opt-in;
  cancellations report uncertainty rather than successful completion.
- Omit replay argument values and wholesale upstream script errors from diagnostics;
  document still-open host/debug/direct-envelope/stderr privacy gaps.

All notable changes to Weave. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

### Audit remediation and safety follow-ups

#### Aggregate budget follow-up
- Add configurable aggregate UTF-8 source/stdout and compact-JSON trace budgets.
- Bound proxy and upstream MCP frames and headers before unbounded buffering.
- Breaches are fatal, not silently truncated; these are not process memory caps.

#### Audit follow-up
- W13: preserve completed bindings through failing nested branches, loops and
  handlers; add independent CPython differential exception/write-log coverage.

#### Audit remediation (experimental source only)
- Preserve PR10 README improvements; remove unequal-endpoint speedup framing.
- Gate writes on prior computation success; execute exception regions in order.
- Trace v2 exact identity/source/declaration replay, with v1 rejection.
- Check literal, tool, speculative and replay value ingress; cap Python conversion depth.
- Make process-worker queues cancellation-aware; preserve slots after pickle failures.
- Reject MCP final-name collisions; paginate discovery; alias invalid script names;
  preserve structured results and compact successful model output by default.
- Repair Python keyword support, malformed source errors, SDK event-loop blocking,
  CLI typo exits and Unicode find indexes.
- Benchmark v2 uses final-model-answer endpoints for both modes and bounded native
  parallel reads; no new live API measurements. Legacy mock LLM harness is labelled unequal.
- See `docs/audit-status.md` for all 17 findings and remaining/deferred limitations,
  especially partial nested exception binding state, aggregate resource budgets,
  script cancellation forwarding, and idempotency-aware write retry policy.
- No rebuilt release downloads or PyPI publication.

#### Documentation
- Reorganize the README around installation, a runnable workflow, agent and MCP
  integration, and execution boundaries. Clarify package naming, release wheels,
  benchmark completion boundaries, and known v0.1.0 limitations.

## [0.1.0] - 2026-11-08

First public version, released on GitHub with prebuilt wheels. It is not on
PyPI. `v0.1.0-rc1` and `v0.1.0-rc2` were release-candidate tags used to check
the wheel builds.

### Language
- A Python subset parsed with `ruff_python_parser`: assignment (including
  unpacking and augmented assignment), calls with keyword arguments,
  `if`/`elif`/`else`, conditional expressions, `for` with `break` and
  `continue`, `try`/`except ToolError` and a bare `except`, lists, dicts,
  f-strings, comparisons, boolean operators and arithmetic.
- String, list and dict methods, including `split`/`rsplit` with `sep` and
  `maxsplit` (positional or keyword), `splitlines`, `strip`/`lstrip`/`rstrip`
  with an optional character set, and the `is*` predicates (`isdigit`,
  `isalpha`, `isalnum`, `isspace`, `isupper`, `islower`).
- Builtins: `len`, `range`, `sum`, `min`, `max`, `sorted`, `reversed`,
  `enumerate`, `zip`, `any`, `all`, `list`, `str`, `int`, and `parse_json`.
- Unsupported constructs (`import`, `def`, `class`, `while`, `with`, `lambda`,
  comprehensions, `await`) get a one-line static error with a fix hint.

### Runtime
- Dataflow scheduler. Independent `pure` and `read` calls run together;
  `reversible` calls wait for conflicting reads and writes; `irreversible`
  calls wait until the script is complete.
- Loops with no carried assignment and no writes run their iterations in
  parallel. A loop that uses `break` or `continue` runs in order.
- Per-tool timeouts and retries, cancellation, sandbox limits (steps, value
  size, nesting depth), and streaming sessions that run each completed
  statement while the rest of the script arrives.
- JSON traces with tagged values. Replay reproduces prints and bindings
  without calling tools and reports divergence.
- Speculative pre-execution of reads, with invalidation when a write could
  change their result.

### Bindings and tools
- Python package (PyO3, built with maturin): `Engine`, `Session`, `Run`,
  `CancelToken`, `Speculation`, `declare`, and `WeaveTool`, a single
  function-calling tool for Chat Completions, Responses, and the Agents SDK.
  `weave.__version__`.
- `weave` CLI: `check`, `run`, `replay`, `trace`, and `--version`.
- `weave-mcp`, an MCP server with `weave_check`, `weave_run`, and
  `weave_replay`. It reads newline-delimited JSON and `Content-Length`
  framing, and it accepts `--version`.
  - It proxies upstream MCP servers (`upstreams`), with per-tool `effects`,
    `results` shapes (`lines`, `json`), and startup `examples` that show a
    sample output in the tool listing.
  - Structured parameters appear in the listing as compact types.

### Fixed before release
- A parallel `for` loop dropped a conditional assignment when a later
  iteration skipped it; the value now comes from the last iteration that
  assigned it, as in Python. The differential fuzzer now generates this case.
- An error inside an `if` or loop nested in `try` could rarely escape the
  handler (2 of 5000 runs of one script), because nested statements were
  also tracked as top-level roots. They no longer are.

### Testing and measurement
- A differential fuzzer compares an independent oracle with sequential,
  jittered parallel, streamed, speculative, and replayed runs.
- `bench/real_tools_eval.py`: 16 tasks against the real filesystem, fetch,
  git, and memory MCP servers, with write tasks checked on real state, run in
  Weave and native function-calling modes. Every measured run is in
  `bench/results/` and in `docs/benchmarks.md`.
- Release workflow: wheels for Linux x86_64/aarch64, macOS arm64/x86_64, and
  Windows x64 on CPython 3.9–3.13, plus an sdist. An opt-in PyPI
  trusted-publishing job stays off unless `PYPI_PUBLISH` is set.

### Documentation
- README rewritten around a measured benchmark chart and a how-it-works
  diagram, with the long eval write-up in `docs/benchmarks.md`.
- `CONTRIBUTING.md`, `SECURITY.md`, and `docs/releasing.md`.
