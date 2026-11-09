# Cancellation, retries and privacy

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

## What cancellation reaches

| Phase | Preemptible? | How it is bounded |
| --- | --- | --- |
| Queued / not-yet-started tool calls | yes, never started | — |
| Isolated Python tool running in an owned worker | yes: worker process group SIGKILLed | `tests/python/test_isolation.py` |
| In-process Python tool already running | no; its result is discarded | finishes on its own; use isolation |
| Upstream MCP call in flight | cooperative: `notifications/cancelled` sent | upstream may ignore it |
| Parse and static check | no | 1 MiB source cap, nesting cap; `test_bounded_phases.py` (1 MiB check ≈ 0.02 s release) |
| Replay | no (runs on the calling thread) | trace byte cap; linear in calls, checked by `test_bounded_phases.py` |
| asyncio task awaiting `WeaveTool.run_async` / Agents SDK tool | yes: session cancelled from the event loop | `test_isolation.py` cancellation tests |

Timings are regression bounds measured on CI runners, not real-time guarantees.

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

## Privacy

Host exception text, upstream stderr, upstream environment and upstream error
payloads are withheld by default and opt-in per host. See [privacy.md](privacy.md)
for every channel, the opt-ins and the remaining trust assumptions.
