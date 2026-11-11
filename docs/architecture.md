# Architecture

Weave turns a model-written script into a dataflow graph of tool calls and runs it under effect rules. This
page is the map for contributors. For the user-facing guarantees, see [production.md](production.md).

```
source text ──parse.rs──▶ AST (ast.rs) ──check.rs──▶ diagnostics (static errors with hints)
                                  │
                           analysis.rs: which names a loop carries, block effects
                                  │
                              exec.rs: launch every statement as futures
                                  │      env = name → shared future
                                  ▼
                tools.rs registry ─▶ gates by effect ─▶ host tool (in-process, worker, MCP upstream)
                                  │
                         trace.rs (record / exact replay), speculate.rs (footprint cache),
                         cancel.rs (deadlines, cancellation), limits.rs (budgets), stream.rs (sessions)
```

## Crates

| crate | role |
|---|---|
| `crates/weave` | the runtime: parser, checker, scheduler, values, traces |
| `crates/weave-py` | PyO3 bindings (`weave._native`), Python tool calls, worker pool bridge |
| `crates/weave-mcp` | stdio MCP server that exposes one script tool and proxies other stdio MCP servers |
| `crates/weave-cli` | `weave check / run / replay` |
| `python/weave` | Python package: `Engine`, `WeaveTool` (agent tool), `declare`, workers, CLI |
| `bench/v4` | the pre-registered benchmark harness (see [benchmarks.md](benchmarks.md)) |

## Execution model (`exec.rs`)

- **Futures, not threads.** Each expression becomes a shared future. A statement does not wait for the previous one;
  it waits only for the values it reads, so independent calls overlap without the script saying so.
- **Effects gate calls.**
  - `pure` calls wait for their arguments only.
  - A `read` also waits for the last mutation.
  - A `reversible` write waits for every earlier observable call.
  - An `irreversible` write also waits for the whole script to finish (`complete_rx`), so a script fails before it
    sends anything.
- **Branches and loops.**
  - An `if` waits for its condition (`schedule_if`), then launches the taken branch.
  - A `for` loop runs in parallel when no iteration depends on another and the body only reads.
  - A read-only loop that carries names (accumulators, dict fills, appends) is *pipelined*. Windows of 64 iterations
    are launched at once, the environment is threaded through them as futures, and carried values are materialised
    between windows. A loop that writes runs its iterations in order.
- **Desugaring.** `break`/`continue` become guard flags, and `xs.append(v)` becomes `xs += [v]`.
- **Errors.** A failed call becomes a `ToolError` value that `try/except` can catch. A failed expression stops later
  writes but does not undo earlier ones.

## Traces and replay (`trace.rs`)

Every call is recorded with its invocation path (line, column and loop indices), arguments, result and completion
order. Replay matches calls by invocation path and returns the recorded results, so it is exact even when the live
run's timing differed.

## Agent tool (`python/weave/agent_tool.py`)

`WeaveTool` renders the rules and a typed function listing, checks and runs scripts, echoes a final bare expression,
and turns every failure into text the model can repair from. `handle_many` runs the read-only scripts of one model
turn concurrently.

## Where to start reading

1. `crates/weave/tests/runtime.rs` shows the behaviour from the outside.
2. `exec.rs::launch_block`, `schedule_for` (with `run_parallel_for` and `run_sequential_for`) and `schedule_call` are the scheduler.
3. `check.rs` is the static check, and the place to improve error messages.
