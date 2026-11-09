# Independent audit remediation ledger

**Experimental, source-only fixes. Not a production safety certification.**
The v0.1.0 tag and its 43 download assets are unchanged. Its release notes now
carry the experimental/audit warning. PR10's useful README/install/integration
and benchmark caveats were reviewed and preserved without its commit metadata.

Baseline independently reproduced at main `e1a8be5` on Linux x86_64,
CPython 3.13, Rust 1.99.1, with the supplied exact probes retained in
[`tests/audit/original-probes`](../tests/audit/original-probes/).
[Full original findings](../tests/audit/WEAVE_AUDIT.md),
[roadmap (future work, not implemented)](../tests/audit/RESEARCH_AND_ROADMAP.md),
[provenance / run instructions](../tests/audit/README.md).
No live model spending was used for this remediation.

“Fixed targeted case” means the named acceptance probes pass, not proof of a
complete property over arbitrary scripts, hosts, protocols, or operating systems.

| ID | Independently verified baseline | Source status and acceptance evidence |
|---|---|---|
| W01 | Writes after division failure: **12/12** | **Fixed targeted cases.** Prior computation success gates writes, including errors in arguments/branches/loops and caught tool failures. Exception bodies execute statement-by-statement. Original probe now **0/12 writes**, plus `failure_boundaries_skip_later_writes_and_keep_completed_bindings` asserts actual write logs, not just stdout. Independent reads outside exception regions still overlap. No rollback or exactly-once guarantee. |
| W02 | One worker / three calls: **350.49 ms** return for 100 ms run limit; completion marker later existed | **Fixed tested queue/lifecycle cases.** Nonblocking queue start; dispatch vs cancellation serialized; owned-job drop guard kills abandoned local jobs. Three trials each of tool deadlines, run deadlines, dropped sessions: no completion markers after a 900 ms observation window; replacement and recovery checked. Tool-timeout returns 52.70–54.25 ms; 100 ms run-budget returns 103.03–103.54 ms in this run. Not a real-time guarantee: plain sync threads cannot be killed; Rust isolated-start callbacks must be nonblocking; remote effects cannot be undone. |
| W03 | Identical reads replayed differently **12/12** | **Fixed tested identity cases.** Trace **v2** stores source, tool declarations and logical callsite/nested-loop paths. No matching by completion order. v1 / changed source / changed declarations rejected. Original probe now **0/12 mismatches**. Added 300 batch/stream recordings and 900 exact replays with loops, lazy expressions and handled errors; no live calls during replay. Function implementation/remote schema identity is NOT attested by this declaration manifest. |
| W04 | 64-byte limit accepted 4096-byte literals and tool results; output 660 bytes | **Value bypass and aggregate byte accounting fixed in source; not memory isolation.** 63/64/65-byte literal/tool/replay/speculative ingress acceptance tests; 4096 rejected; every expression result checked. Python conversion depth capped at 100, including cyclic results. Source/stdout/compact-trace budgets now default to 1/1/4 MiB, plus 8 MiB MCP frames and 16 KiB headers. See [exact semantics](budgets.md). No process-memory/in-flight-tool cap; conversion/parser/formatting allocations can precede checks. Not host resource isolation. |
| W05 | Duplicate direct call routed to A, script to B | **Fixed targeted configurations.** Config discovery rejects duplicate final names (error identifies A/B), runtime builtin / weave_* collisions, request override collisions, alias collisions. Both interfaces use the validated discovered names. Accepted unique-prefix config returns the same value through direct/script calls. |
| W06 | Inspected original unequal endpoints and serial native execution; did NOT rerun models | **Controlled v3 mechanics and finite adapter stage tested; live validation still deferred.** Default-offline successor has equal final-answer/actual-state grading, canonical full schemas/results/common context, equal backend concurrency caps and native parallel read barriers, repair/error feedback, fresh real-file fixtures and exact successful/attempted write logs, declared source/model/API/backend/generation pins, retry-inclusive usage/time and explicit unknown/failure rows. Development/candidate-held-out splits reject exact cross-split duplicates; no independence/unseen claim. Legacy v2 real-tools CLI retired. Historical JSON unchanged; no new live measurements or speedup claims. The finite [adapter stage](benchmark-adapter-stage.md) adds shipped WeaveTool/MCP execution, synthetic local Responses HTTP, real disposable file/git/memory writes, actual artifact pins and durable timeout/crash/interrupted accounting (21 new tests, 54 combined). Live provider acceptance/billing, reference-server coverage and genuine frozen held-out evaluation remain unmeasured; no unseen or performance claim. See [protocol/gaps](benchmark-protocol-v3.md). |
| W07 | Synthetic private value appeared in bindings/trace despite summary-only stdout | **Compact default fixed; broader privacy design deferred.** MCP run/replay now default to stdout/status; `debug:true` explicitly opts into full sensitive trace/bindings. Test ensures private value absent from successful default payload. Replay mismatch values and wholesale upstream script errors now omitted. No opaque host-side trace storage, script-only discovery mode or comprehensive error redaction; host exceptions, direct envelopes, prints, upstream stderr and explicit debug remain sensitive. See cancellation-and-retries.md. |
| W08 | Second-page tool absent | **Fixed bounded pagination.** Follow cursor with 64-page / 4096-tool caps and repeated-cursor rejection. Second-page tool is callable in local fixture. |
| W09 | `a_read-file()` parsed as subtraction | **Fixed aliases.** Deterministic ASCII escapes (`a_read_u2d_file`) and keyword aliases; reverse route retains original upstream name. Final alias collisions rejected, not silently overwritten. Acceptance fixture calls hyphen alias. Dots/keywords/alias-collision tests also live in Rust. |
| W10 | Structured-only direct tool returned empty text | **Fixed tested structured/text cases.** Direct calls preserve original envelope, script calls prefer structured JSON. Unsupported non-text-only script content raises explicit error, not successful empty text. No claim to decode arbitrary multimodal data or validate all outputSchema constraints. |
| W11 | `echo(x="ok")` rejected by positional arity/keyword checks | **Fixed tested Python signatures.** Derive keyword acceptance from inspectable signature, count positional+keyword arity, Python binds mixed/duplicate/unknown arguments before function entry. Injected cancel is not script-controlled. Opaque signatures remain conservative; async Python tools still unsupported. |
| W12 | Null/int/list/object source escaped as AttributeError | **Fixed.** Non-string source returns repairable `error:`; malformed JSON/top-level shapes covered offline. |
| W13 | Completed try assignment lost: `0,0` instead of `1,1` | **Fixed targeted nested state cases in source.** Ordered recursive exception-region execution retains completed assignments in failing branches/loops and nested handlers. Exact counterexample now prints `1,1`. Rust regression plus 504 independently executed CPython differential programs compare output, bindings and write logs across 1008 batch/stream runs and replays. This is bounded differential evidence, not arbitrary Python equivalence. |
| W14 | SDK heartbeat blocked **151.46 ms** by 150 ms tool | **Event-loop blocking fixed; cancellation is soft.** SDK wrapper offloads to thread; local probe now ~5–7 ms heartbeat gaps. Cancelling await does not kill sync host work in that thread. Use process-isolated tools for local termination, without promising remote cancellation. |
| W15 | Unpickleable function consumed last idle slot (0 left) | **Fixed.** Serialize before capacity acquisition; original probe now retains 1 slot. Next good call and killed-worker replacement good call both asserted. Pool close tracks queued/active jobs. |
| W16 | `python -m weave typo` returned zero silently | **Fixed.** Nonzero exit and usage diagnostic asserted. |
| W17 | Unicode find returned byte offset 2; slice empty | **Fixed targeted find/slice case.** Convert byte index to character index; `éx`.find(`x`) = 1 and slice = `x`. Does not assert full CPython string-method compatibility. |

## Retained W13 counterexample (now fixed)

```python
x = 0
try:
    if True:
        x = 1
        fail()  # registered read tool raises ToolError
except ToolError:
    print(x)
print(x)
```

Baseline source printed `0\n0\n`. Corrected source and independent CPython
both print `1\n1\n`; the original evidence is preserved unchanged.

## Other scope boundaries

- MCP input remains responsive during scripts/direct calls; request-ID cancellation
  is forwarded best effort to upstream IDs, and EOF cancels active/queued work.
  Admission is bounded at 16; scripts/writes are exclusive, direct reads can overlap.
  Sync callbacks/upstreams may continue; parsing/check/replay are not transport-preemptible.
- Write retries now require explicit host `retry_safe` idempotency support; default
  Reversible/Irreversible retries are clamped to zero. Effects/opt-in are host
  declarations, not authorization, rollback or exactly-once guarantees. Errors
  report cancellation/uncertain completion; no durable reconciliation ledger.
- Privacy remains partial: replay mismatch values and wholesale script upstream
  errors are omitted, but host exceptions, direct envelopes, prints, debug/traces
  and inherited upstream stderr can contain secrets. See [semantics/gaps](cancellation-and-retries.md).
- Failed runs do not yet expose full partial trace/debugger state. No debugger,
  approval UI, hosted service, language expansion or publishing was substituted
  for audit fixes.
- Exact-replay v1 compatibility intentionally removed. Trace source/manifest
  retain sensitive information in host/debug paths. Verify before sharing.
- Tests run on this Linux box and Ubuntu CI. macOS/Windows worker timing and
  all release wheels are not revalidated by these source-only tests.

## Evidence and running gates

`tests/audit/evidence/baseline-*`, `fixed-*`, `worker-acceptance.log` and
`differential-5000.log` retain measured outputs. `test_adapters.py`,
`test_replay.py`, `test_mcp.py`, `test_benchmark.py`, `test_workers.py`,
`test_exceptions.py`, `test_budgets.py`, `test_cancellation.py` are
assertion-based offline acceptance tests, included in CI. The Rust full suite
and clippy must pass before merging; the exact CI link is recorded in the PR.

Initial PR11 local gate: **90 Rust tests passed**, clippy with `-D warnings` passed,
**5000 differential seeds passed**, six installed-SDK adapter checks passed,
and all five offline audit acceptance scripts passed. See the retained logs.

Current follow-up gate: **93 Rust tests passed**, including 5000 differential
seeds; clippy `-D warnings` and all eight offline acceptance scripts passed.
W13 adds 504 independent CPython programs / 1008 batch-stream executions plus
exact replays. New cancellation/retry tests cover eight effect/registration cases,
script/direct string-ID forwarding, ignored upstream cancellation, queued-write
suppression, EOF, 16-request admission, duplicate IDs and replay error redaction.
Source changes do not repair already-downloaded v0.1.0 binaries.

Benchmark follow-up: **33 deterministic offline protocol tests** now run inside
`test_benchmark.py`; six hand-authored development cassette rows are labelled
`offline_mechanics`, not model-performance evidence. Source/config/schema/oracle
reporting and negative/failure/write checks are covered; corrected live runs remain
pending. See `tests/audit/evidence/benchmark-protocol/` and protocol gap checklist.

Finite adapter stage: **21 synthetic local-HTTP/MCP tests**, **54 combined**
protocol tests inside the benchmark acceptance gate. No provider API spending or
new performance measurements; see `tests/audit/evidence/protocol-v3-adapters/`
and [bounded stage semantics](benchmark-adapter-stage.md).
