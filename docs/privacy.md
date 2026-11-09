# What reaches scripts, models and logs

Weave's privacy boundary is **host internals vs. tool results**.

- **Tool results are model-visible by design.** A script exists to read tool
  results and print what the model needs, so any value a tool returns can reach
  the model (through `print`, error messages about that value, or `debug`
  output). Do not return secrets from tools you expose to a model.
- **Host internals are not.** Exception text, tracebacks, upstream server logs,
  upstream error payloads and the host environment stay out of scripts and
  client output unless the host opts in.

## Defaults (tested with planted secrets)

| Channel | Default | Opt-in | Test |
|---|---|---|---|
| Python tool exception (in-process, cooperative, isolated worker) | only the exception type: `RuntimeError (details redacted; ...)` | raise `weave.ToolError("safe message")`, or `Engine(debug=True)` / `WeaveTool(debug=True)` for full text + traceback | `tests/python/test_privacy.py`, `test_isolation.py` |
| Same error caught in a script (`except ToolError as e`) and its trace | redacted text only | same | `test_privacy.py` |
| `weave-mcp` upstream stderr | discarded | `"stderr": "inherit"` or `{"file": path}` per upstream | `crates/weave-mcp/tests/privacy.rs` |
| `weave-mcp` upstream environment | allowlist only (`PATH`, `HOME`, locale, temp dirs, Windows system vars, CA bundle vars) plus explicit `"env"` | `"inherit_env": true` | `privacy.rs` |
| Upstream `isError` content inside scripts | omitted | `"debug": true` per upstream | `privacy.rs` |
| Upstream JSON-RPC error messages (script and direct calls) | omitted | `"debug": true` | `privacy.rs` |
| Failed startup example samples in the tool listing | `ToolError (sample failed; content omitted)` | `"debug": true` | `privacy.rs` |
| `weave_run` / `weave_replay` bindings and trace | omitted; only `stdout` and `status` | `"debug": true` argument | `tests/audit/test_mcp.py` |
| Replay mismatch diagnostics | argument values omitted | none | `tests/audit/test_cancellation.py` |

## Remaining trust assumptions (not redacted)

- **Direct `weave-mcp` tool calls are a transparent proxy.** A successful or
  `isError` result envelope from a direct upstream call is the tool's own
  output and is returned unchanged; only JSON-RPC protocol errors are omitted.
- **Script prints and tool results** reach whoever reads the output.
- **Traces are sensitive host artifacts.** `Run.trace_json()` and `debug`
  output contain source, arguments and results. Store them like logs with user
  data. A redacted error message is stored redacted.
- **Weave's own error messages** may quote script values and source positions
  (e.g. a failed `int("...")` conversion). Those values came from the script or
  from tool results, which are already model-visible.
- `weave.ToolError` messages and anything under `debug=True` are shown verbatim:
  the host is asserting they are safe.
- No secret scanning or classification is performed. Redaction is structural
  (which channel), not content-based.
