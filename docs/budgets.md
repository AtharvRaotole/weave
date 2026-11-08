# Aggregate byte budgets (experimental source)

These are runtime/transport accounting limits, **not a process memory cap**.
Existing v0.1.0 downloads do not include them.

| Budget | Default | Accounting and failure |
|---|---:|---|
| `max_source_bytes` | 1 MiB | UTF-8 bytes accumulated across all Session.push chunks, checked before parsing a new chunk. Engine.check/run also reject oversized source. |
| `max_output_bytes` | 1 MiB | Aggregate emitted stdout UTF-8 bytes, including separators/newlines; checked after formatting, before retaining the next print. No silent truncation. |
| `max_trace_bytes` | 4 MiB | Compact JSON bytes of retained events plus source/tool-declaration envelope. Envelope reserved as source arrives; events charged before retention. Final full trace checked again. Pretty JSON formatting is not the accounting unit. |
| MCP frame | 8 MiB per body | Proxy and upstream input, newline and Content-Length framing; outgoing frames serialized into a bounded buffer before any bytes are emitted. Header bytes capped at 16 KiB. Oversize/invalid frames fail the connection, not truncated JSON. |

Rust: set fields on `Limits { ..Default::default() }` and call `Engine::set_limits`.
Python: `engine.set_limits(max_source_bytes=..., max_output_bytes=..., max_trace_bytes=...)`.
Zero is a real zero-byte budget, not “unlimited”. MCP transport limits are fixed
constants; embedding Rust tests can call `read_message_limited` with a smaller cap.

Aggregate budget breaches are run-fatal (not catchable by bare except). Cancellation
is requested, preventing later guarded dispatch; work already started may finish.
Streaming reads may already have run before a later source chunk exceeds a budget.
A tool's result size is only known after it returns: trace/output failure does
not roll back a completed write or any other remote effect.

Allocations outside these retained byte totals include parser/AST/future overhead,
host functions, Python conversion, temporary formatting, decoded JSON, bindings,
cache values, pending work, allocator overhead and SDK buffers. There is still no
process-wide memory, CPU, trace-count or in-flight-tool-call cap. Use separately
configured process/container limits for untrusted workloads. Direct MCP transport
request concurrency remains bounded independently. Debug output and diagnostics
may contain sensitive data; budgets do not provide redaction or authorization.
