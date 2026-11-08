# Cancellation, retries and privacy (experimental source)

## Transport cancellation

The MCP stdio input loop stays available during `weave_run` and direct upstream
calls. `notifications/cancelled.params.requestId` is matched exactly (number vs
string included). Active script sessions receive cancellation; upstream calls
poll a cooperative token and send a best-effort upstream cancellation with the
**upstream** request ID. No live requests are automatically retried by the proxy.
EOF or a framing error cancels active and queued requests before joining workers.
Forwarding during shutdown is best effort: the owned child may be stopped before
it consumes the notification. This is not cancellation of arbitrary remote effects.

At most 16 tool requests are admitted. Additional requests and duplicate active
IDs get explicit errors without blocking notification processing. Declared direct
reads can overlap; scripts/writes take an exclusive gate. Replies can arrive out
of order, and independent requests have no FIFO side-effect ordering guarantee.
Clients must match IDs and explicitly sequence dependent requests.

Cancellation is cooperative/soft. A callback already running can finish; an
upstream can ignore notifications. Upstream stdio writes can block on a stuck
peer, parsing/static checking and replay are not preemptible via the transport,
and cancellation cannot undo completed effects. Script timeouts cancel queued
work, not remote reality. Use owned process isolation for terminable local tools;
even killing that process does not roll back a request already sent remotely.
A cancellation response explicitly reports uncertain completion; reconcile with
the host before trying a write again. There is no durable completion ledger.

## Safe retry policy

`retries=N` only applies automatically to Pure/Read tools by default. Reversible
and Irreversible tools get **one attempt** unless the registering host explicitly
sets `retry_safe=true` (Python tool/declaration) or
`ToolSpec::with_retry_safe(true)` (Rust). This is host policy, not a script option
or an MCP annotation. Mislabelled read effects are still unsafe host declarations.

Only opt in if the host implements stable idempotency keys/deduplication across
all attempts, including timeout/uncertain-completion paths. Weave does not invent
keys, validate your server's guarantee, roll back writes or provide exactly-once
execution. Sync attempts may overlap after timeout; the host must safely handle
that too. Existing explicit timeout/cancellation uncertainty must not be treated
as proof an operation never ran. Without real idempotency support, do not opt in.

## Privacy gaps remain

- Default MCP results omit bindings and trace. `debug:true` deliberately exposes
  source, tool manifest, argument/result values and bindings. Traces remain
  sensitive host artifacts, not redacted shareable telemetry.
- Replay mismatch diagnostics omit actual/recorded argument values. Script MCP
  `isError` and JSON-RPC error envelopes no longer get copied wholesale into
  errors; inspect the upstream directly when details are needed. Direct tool
  result envelopes are intentionally preserved, including content/error values.
- Host/Python exception messages, prints, literal-related parser/runtime errors,
  configured names/paths, user-requested debug output and inherited upstream
  stderr can still include secrets. There is no comprehensive redactor or
  secret classification. Avoid sensitive exception messages; restrict logs and
  trace access. There is no production privacy certification.
