# Running Weave in production

This page states what Weave 1.0 guarantees, what it does not, and what it
assumes about the host. Every guarantee below names the test that checks it;
those tests run in CI on every push (Linux) and, for isolation and privacy, on
macOS and Windows runners too. Anything not listed here is not promised.

## Supported production posture

Weave is supported for production use when **all** of these hold:

1. **The host owns the tools.** Tools are your own functions or stdio MCP
   servers you chose, declared with honest effects (`read` only for real reads).
2. **Tools whose runtime matters are isolated.** Construct the engine with a
   worker pool and `isolate_all=True` (or `WeaveTool(..., worker_pool=N)`), set
   `max_wall_time_ms`, and set `WorkerLimits` where the platform supports them.
   Without isolation, deadlines are soft (see below).
3. **Model-visible output is treated as model-visible.** Tools do not return
   secrets you would not show the model; hosts raise `weave.ToolError` for error
   text the model should see and leave `debug` off.
4. **Writes are reconciled, not retried blindly.** A failed or timed-out write
   may have happened. Only tools that are truly idempotent are marked
   `retry_safe=True`.
5. **The script author is not more trusted than the model.** Scripts are
   untrusted input; the host process, its tools and its credentials are trusted.

## Threat model

| Actor | Trusted? | What Weave does about it |
| --- | --- | --- |
| Script / model writing it | **Untrusted** | Restricted language with no imports, no `eval`, no filesystem/network/process access except registered tools; step, value-size, nesting, source, stdout and trace byte budgets; effect ordering for writes; irreversible calls wait for the full script. |
| Host process and registered tools | Trusted | Weave cannot stop a tool from doing what its code does. Isolation bounds its time and (POSIX) CPU/memory/files, but it runs as the host user. |
| Upstream MCP servers | Trusted to behave as declared; output treated as data | Environment allowlist, stderr discarded, error payloads withheld by default; annotations are declarations, not verified permissions. |
| Trace files | Sensitive host artifacts | Replay never calls live tools and rejects traces whose source or tool manifest differ. Traces contain arguments and results. |
| MCP client | Trusted to send well-formed requests | Frame/header byte caps, 16-request admission, duplicate-ID rejection. |

## Guarantees and the tests that hold them

| Guarantee | Scope | Test |
| --- | --- | --- |
| A runaway isolated tool (sleep, CPU spin, child processes) is killed at its `timeout_ms` or the run deadline, and the pool recovers | isolated tools; process group on POSIX, worker only on Windows | `tests/python/test_isolation.py` (Linux in CI; Linux/macOS/Windows in the cross-platform CI job and every release build) |
| Cancelling a session, dropping it, closing the engine, or cancelling the asyncio task awaiting `WeaveTool` kills in-flight isolated work | isolated tools | `test_isolation.py` |
| `WorkerLimits` are enforced by the kernel or rejected at construction, never silently ignored | memory: Linux; CPU/files/file size: POSIX; none on Windows | `test_isolation.py` |
| Host exception text, tracebacks, upstream stderr, upstream error payloads and the host environment do not reach scripts or MCP clients by default | Python tools, workers, `weave-mcp` | `tests/python/test_privacy.py`, `crates/weave-mcp/tests/privacy.rs` (planted secrets) |
| A failed expression stops every later write in the script | all tools | Rust suite, `tests/audit/test_adapters.py`, 504 CPython differential programs in `tests/audit/test_exceptions.py` |
| Replay makes no live calls and reproduces recorded results by call identity, not completion order | all tools | `tests/audit/test_replay.py`, Rust differential suite |
| Script-controlled sizes are bounded (source 1 MiB, stdout 1 MiB, trace 4 MiB, values, nesting, steps) | runtime | `tests/audit/test_budgets.py`, `test_replay.py` |
| Parse, check and replay (not preemptible) finish within bounds derived from those budgets and scale linearly with calls | runtime | `tests/python/test_bounded_phases.py` |
| `weave-mcp` works against the official filesystem, git and memory reference servers, including real writes and `outputSchema`/`structuredContent` | stdio MCP | `tests/compat/test_reference_servers.py` (pinned by lockfiles) |

Timings in tests are regression bounds on shared CI runners, not real-time
guarantees.

## Non-guarantees (inherent, not bugs)

- **No rollback or exactly-once.** Killing a worker cannot recall an HTTP
  request already sent. A timed-out write is reported as uncertain.
- **In-process tools keep soft deadlines.** Plain threads cannot be killed;
  in-process tools, Rust `register` tools and MCP upstream calls stop being
  awaited, but may continue. Upstream cancellation is a best-effort
  `notifications/cancelled`.
- **Not a security sandbox for tool code.** Workers share the host user,
  network, filesystem and credentials. Use containers/VMs/separate users when
  the tool code itself is untrusted.
- **The host process has no memory cap.** Budgets are byte accounting for
  script-controlled data, not an RLIMIT on the host. Parser, checker,
  scheduler and replay run in the host.
- **Tool results are model-visible.** Redaction is by channel, not content;
  no secret scanning. A tool that prints or returns a secret leaks it.
- **Effects are declarations.** Weave orders calls by the effect you declare;
  it does not verify that a `read` tool does not write.
- **Windows:** no process-group kill (grandchildren may survive) and no
  `WorkerLimits`. **macOS:** no `memory_bytes`.
- **Escapes from the process group** (`setsid`, daemonizing) are not tracked.

## Not covered by 1.0

- Remote (HTTP/SSE) MCP transports; `weave-mcp` is stdio only.
- Async Python tool functions.
- A step debugger or partial traces for failed runs.
- A performance claim. Live protocol-v3 results (OpenAI only, ten small
  tasks) show success on par with native tool calling and no speedup; see
  [benchmarks](benchmarks.md).
- PyPI distribution. Install the wheels from the GitHub release
  ([getting started](getting-started.md)).

## Checklist

- [ ] `Engine(worker_pool=N, isolate_all=True, worker_limits=...)` or
      `WeaveTool(..., worker_pool=N)`; check `engine.isolation()`.
- [ ] `max_wall_time_ms` and per-tool `timeout_ms` set.
- [ ] Effects reviewed; writes are `reversible`/`irreversible`; only real
      idempotent writes are `retry_safe`.
- [ ] Tool errors meant for the model use `weave.ToolError`; `debug` off.
- [ ] `weave-mcp` upstreams: explicit `env`, no `inherit_env`, stderr to a
      file you protect if you need it, unique prefixes.
- [ ] Traces stored like logs that contain user data.
- [ ] Untrusted tool code runs inside an OS sandbox you provide.

Related: [isolation](isolation.md), [privacy](privacy.md),
[cancellation and retries](cancellation-and-retries.md), [budgets](budgets.md),
[audit ledger](audit-status.md), [SECURITY.md](../SECURITY.md).
