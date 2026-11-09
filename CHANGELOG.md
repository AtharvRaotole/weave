# Changelog

## Unreleased process isolation
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

## Unreleased finite protocol-v3 adapter stage
- Connect shipped WeaveTool/MCP execution and native Responses wire format to
  synthetic loopback HTTP and real disposable filesystem/git/memory MCP fixtures.
- Pin actual artifacts, enforce equal backend concurrency/call controls, and retain
  append-only durable trial/attempt/HTTP accounting with explicit missing usage.
- Keep remote spending disabled by default with dual explicit opt-in; no live runs,
  unseen evaluation, releases or model-performance claims.

## Unreleased benchmark protocol remediation
- Add deterministic offline controlled-comparison tests for endpoints, full-context
  parity, native parallel reads/equal backend caps, real state/write validation,
  configuration pins and retry-inclusive failure/usage accounting.
- Separate public development/candidate-held-out tasks without independence claims;
  retire the legacy v2 real-tools live CLI. Corrected live runs remain pending.

## Unreleased safety follow-up
- Responsive MCP request-ID cancellation with best-effort upstream forwarding,
  EOF cleanup, bounded admission and duplicate active-ID rejection.
- Reversible/irreversible retries require explicit host idempotency opt-in;
  cancellations report uncertainty rather than successful completion.
- Omit replay argument values and wholesale upstream script errors from diagnostics;
  document still-open host/debug/direct-envelope/stderr privacy gaps.

All notable changes to Weave. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Aggregate budget follow-up
- Add configurable aggregate UTF-8 source/stdout and compact-JSON trace budgets.
- Bound proxy and upstream MCP frames and headers before unbounded buffering.
- Breaches are fatal, not silently truncated; these are not process memory caps.

### Audit follow-up
- W13: preserve completed bindings through failing nested branches, loops and
  handlers; add independent CPython differential exception/write-log coverage.

### Audit remediation (experimental source only)
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

### Documentation
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
