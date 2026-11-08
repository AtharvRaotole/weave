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
| W02 | One worker / three calls: **350.49 ms** return for 100 ms run limit; completion marker later existed | **Fixed tested queue/lifecycle cases.** Nonblocking queue start; dispatch vs cancellation serialized; owned-job drop guard kills abandoned local jobs. Three trials each of tool deadlines, run deadlines, dropped sessions: no completion markers after a 900 ms observation window; replacement and recovery checked. Tool-timeout returns 53.31–54.05 ms; 100 ms run-budget returns 101.68–105.15 ms in this run. Not a real-time guarantee: plain sync threads cannot be killed; Rust isolated-start callbacks must be nonblocking; remote effects cannot be undone. |
| W03 | Identical reads replayed differently **12/12** | **Fixed tested identity cases.** Trace **v2** stores source, tool declarations and logical callsite/nested-loop paths. No matching by completion order. v1 / changed source / changed declarations rejected. Original probe now **0/12 mismatches**. Added 300 batch/stream recordings and 900 exact replays with loops, lazy expressions and handled errors; no live calls during replay. Function implementation/remote schema identity is NOT attested by this declaration manifest. |
| W04 | 64-byte limit accepted 4096-byte literals and tool results; output 660 bytes | **Value bypass fixed; broader budgets deferred.** 63/64/65-byte literal/tool/replay/speculative ingress acceptance tests; 4096 rejected; every expression result checked. Python conversion depth capped at 100, including cyclic results. Still no total memory/output/trace/in-flight/source/frame budget; conversion/parser allocations can precede checks. Not host resource isolation. |
| W05 | Duplicate direct call routed to A, script to B | **Fixed targeted configurations.** Config discovery rejects duplicate final names (error identifies A/B), runtime builtin / weave_* collisions, request override collisions, alias collisions. Both interfaces use the validated discovered names. Accepted unique-prefix config returns the same value through direct/script calls. |
| W06 | Inspected original unequal endpoints and serial native execution; did NOT rerun models | **Methodology corrected, validation partial.** v2 includes final-model-answer latency/tokens for both, and bounded concurrent native reads in both provider loops (writes remain ordered). Offline tests reject wrong final answers even when stdout is right, count synthesis turns/tokens and run 3x150 ms direct reads concurrently (~153 ms). Mode order alternates. Old JSON untouched and chart/history labelled unequal. No new live measurements or speedup claim; custom script prompt still differs from full public agent SDK loop, and held-out tasks, upstream/model version pinning, p95/cost/error studies remain deferred. |
| W07 | Synthetic private value appeared in bindings/trace despite summary-only stdout | **Compact default fixed; broader privacy design deferred.** MCP run/replay now default to stdout/status; `debug:true` explicitly opts into full sensitive trace/bindings. Test ensures private value absent from successful default payload. No opaque host-side trace storage, script-only discovery mode, or comprehensive error redaction yet; errors and explicit debug can expose arguments/results. |
| W08 | Second-page tool absent | **Fixed bounded pagination.** Follow cursor with 64-page / 4096-tool caps and repeated-cursor rejection. Second-page tool is callable in local fixture. |
| W09 | `a_read-file()` parsed as subtraction | **Fixed aliases.** Deterministic ASCII escapes (`a_read_u2d_file`) and keyword aliases; reverse route retains original upstream name. Final alias collisions rejected, not silently overwritten. Acceptance fixture calls hyphen alias. Dots/keywords/alias-collision tests also live in Rust. |
| W10 | Structured-only direct tool returned empty text | **Fixed tested structured/text cases.** Direct calls preserve original envelope, script calls prefer structured JSON. Unsupported non-text-only script content raises explicit error, not successful empty text. No claim to decode arbitrary multimodal data or validate all outputSchema constraints. |
| W11 | `echo(x="ok")` rejected by positional arity/keyword checks | **Fixed tested Python signatures.** Derive keyword acceptance from inspectable signature, count positional+keyword arity, Python binds mixed/duplicate/unknown arguments before function entry. Injected cancel is not script-controlled. Opaque signatures remain conservative; async Python tools still unsupported. |
| W12 | Null/int/list/object source escaped as AttributeError | **Fixed.** Non-string source returns repairable `error:`; malformed JSON/top-level shapes covered offline. |
| W13 | Completed try assignment lost: `0,0` instead of `1,1` | **Partially fixed; nested case deferred.** Simple completed statements preserve bindings (`1,1`) under later caught failure. **Still known:** assignment inside a failing compound `if`/loop statement may revert to its pre-statement snapshot. Example below remains `0,0`, not Python's `1,1`. Do not assume arbitrary Python exception-state equivalence. |
| W14 | SDK heartbeat blocked **151.46 ms** by 150 ms tool | **Event-loop blocking fixed; cancellation is soft.** SDK wrapper offloads to thread; local probe now ~5–7 ms heartbeat gaps. Cancelling await does not kill sync host work in that thread. Use process-isolated tools for local termination, without promising remote cancellation. |
| W15 | Unpickleable function consumed last idle slot (0 left) | **Fixed.** Serialize before capacity acquisition; original probe now retains 1 slot. Next good call and killed-worker replacement good call both asserted. Pool close tracks queued/active jobs. |
| W16 | `python -m weave typo` returned zero silently | **Fixed.** Nonzero exit and usage diagnostic asserted. |
| W17 | Unicode find returned byte offset 2; slice empty | **Fixed targeted find/slice case.** Convert byte index to character index; `éx`.find(`x`) = 1 and slice = `x`. Does not assert full CPython string-method compatibility. |

## Explicit remaining counterexample (W13)

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

Current source prints `0\n0\n`. Correct Python-style completed-binding semantics
would print `1\n1\n`. Fixing this needs per-control-region partial-state
propagation rather than hiding it by weakening acceptance assertions.

## Other scope boundaries

- MCP script calls still block processing of cancellation notifications; no
  cancellation forwarding or disconnect cleanup guarantee. The original
  cancellation fixture still completes its synthetic write. Concurrent *direct
  reads* were added for the native baseline, not full script request concurrency.
- Effects are host declarations, not authorization or automatic rollback.
  Retry policy is not idempotency-aware; do not automatically retry arbitrary
  production writes. Uncertain remote completion is not represented yet.
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
`test_replay.py`, `test_mcp.py`, `test_benchmark.py`, `test_workers.py` are
assertion-based offline acceptance tests, included in CI. The Rust full suite
and clippy must pass before merging; the exact CI link is recorded in the PR.
