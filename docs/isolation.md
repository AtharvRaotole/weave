# Process isolation for host tools

Weave runs host tools on runtime threads by default. A plain synchronous Python
function on a thread **cannot be stopped**: a tool timeout or run deadline makes
the script stop waiting, but the function keeps running until it returns.

Worker isolation is the supported way to make local deadlines real.

```python
import weave

engine = weave.Engine(
    worker_pool=4,               # four spawn-started worker processes
    isolate_all=True,            # every registered tool runs in a worker
    worker_limits=weave.WorkerLimits(memory_bytes=1 << 30, cpu_seconds=30),
)
engine.install([fetch_invoice, send_email])   # module-level functions
engine.set_limits(max_wall_time_ms=10_000)
print(engine.isolation())                     # what is actually enforced

tool = weave.WeaveTool([fetch_invoice, send_email], worker_pool=4, max_wall_time_ms=10_000)
```

`hard_deadline=True` isolates a single tool on an engine with a pool.
Under `isolate_all=True` a tool opts out only by saying `hard_deadline=False`;
that choice is visible in `engine.isolation()["in_process_tools"]`.

## What is guaranteed (tested in `tests/python/test_isolation.py`)

| Event | Effect on an isolated tool |
|---|---|
| tool `timeout_ms` passes | worker killed; call fails `timed out after Nms`; pool replaces the worker |
| run `max_wall_time_ms` passes | every in-flight isolated call killed |
| `Session.cancel()`, `Session.cancel_handle().cancel()` from any thread, dropped session, `Engine` dropped/closed | worker killed |
| asyncio task awaiting `WeaveTool.run_async` / the Agents SDK tool is cancelled | session cancelled, worker killed |
| POSIX: tool started subprocesses | `SIGKILL` goes to the worker's whole process group (`setsid`) |
| `cpu_seconds` exceeded (POSIX) | kernel signals the worker (`SIGXCPU`, then `SIGKILL`); call fails, worker replaced |
| `memory_bytes` exceeded (Linux, `RLIMIT_AS`) | allocation raises `MemoryError` inside the worker |
| `open_files` / `file_size_bytes` exceeded (POSIX) | the OS call fails inside the worker |
| result bigger than `max_result_bytes` (default 16 MiB) | rejected before the host unpickles it |
| unpickleable tool (lambda, closure) with isolation requested | rejected at registration, not at first call |
| requested limit not enforceable on this platform | `Engine(...)` raises; limits are never silently ignored |
| host exception in a worker | only the exception type crosses unless it is `weave.ToolError` or `debug=True` |

Measured on the Linux CI runner: a 60 s sleep with `timeout_ms=200` returns in
~0.2 s and its process is gone; a CPU spin under a 300 ms run deadline returns
in ~0.3 s. The test bound is 3 s to tolerate slow runners. These are prompt
kills, **not real-time guarantees**.

## What isolation does not do

- **No rollback.** An HTTP request already sent, a row already written or a
  remote job already started stays done. Weave reports the call as failed or
  timed out; the effect may still have happened. Reconcile before retrying a
  write (automatic retries of writes are off unless `retry_safe=True`).
- **Not a security sandbox.** Workers run as the host user with the host's
  environment, files, network and credentials. Use containers, VMs, seccomp or
  separate users when tool code itself is untrusted.
- **Escapes from the process group** (`setsid`, daemonizing, `systemd-run`) are
  not tracked. On **Windows** only the worker process is terminated (no Job
  Object), and `WorkerLimits` are rejected. On **macOS** `memory_bytes` is
  rejected because `RLIMIT_AS` is not enforced there.
- `memory_bytes` caps *virtual* address space, not resident memory; native
  libraries that reserve large mappings may need more headroom.
- The host process itself is not limited: Weave's parser, checker, scheduler
  and value conversion run in the host (see `docs/budgets.md` for byte budgets).
- In-process tools (`hard_deadline=False`, tools taking a `cancel` token, Rust
  `register` tools, MCP upstream calls) keep **soft** cancellation: the script
  stops waiting, the work may continue. Cooperative tools should poll `cancel`.
- Workers start with `spawn`: one interpreter per slot, re-paid after each
  kill. Functions, arguments and results must be pickleable. Workers do not
  share the parent's memory.
