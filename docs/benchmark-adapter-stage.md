# Protocol v3 adapter stage — completed offline

This finite stage connects the controlled harness to **shipped execution paths**
and OpenAI Responses wire messages. Tests use hand-authored **synthetic loopback
HTTP responses**, not live models or an unseen evaluation. No live run is started
by the tests or CLI. Existing v0.1.0 assets/tag and warnings are unchanged. No new speedup, model-accuracy or cost claim follows.

## What is connected

- `ResponsesAdapter` encodes `/v1/responses` requests and decodes function calls,
  final messages, reasoning items, status and usage. It round-trips actual
  `call_id`/`function_call_output` pairs and full history, with no hidden SDK
  retries or `previous_response_id` session reuse. Public wire references:
  [Responses create](https://developers.openai.com/api/reference/resources/responses/methods/create)
  and [function calling](https://developers.openai.com/API/docs/guides/function-calling).
- `RemoteFixture(path="weave_tool")` installs actual MCP-backed functions into the
  shipped `WeaveTool`, then executes the shipped `responses_tool()` / `handle()`
  surfaces. `path="weave_mcp"` uses that same wrapper with a small Engine facade
  calling shipped `weave_check` / `weave_run` through the real `weave-mcp` binary.
  Models cannot override the configured tool set or request debug traces.
- Native Responses tools call the **same** proxy tools. Three owned local MCP
  subprocesses implement a pinned fixture subset over disposable real files, a
  real Git repository and a persisted JSON memory map. They are **not** the
  npm/PyPI reference MCP server packages, a remote service or a vendor conformance
  certification. No fetch/network workload is added in this stage.
- The fixture validates on-disk file contents, actual Git head/tracked files and
  actual memory contents, plus successful **and attempted** write logs. A claimed
  answer without mutations, repeated/blocked/incorrect writes, or script stdout
  without a correct final provider answer cannot pass.

## Equivalent information and controls

Both modes receive the same complete pinned input/output tool contracts, task,
common instructions and generation settings. Native receives individual function
results; Weave receives an aggregate containing the same full backend observations.
Read-only segments are canonicalized; write barriers remain ordered. Neither gets
expected answers, expected writes or privileged sampled examples. Script stdout,
bindings and trace are deliberately omitted from provider feedback in this
**full-observation controlled mode**. Common instructions explicitly describe
that override to the generic shipped tool description. This is not the shipped
compact-output context-saving mode or the entire OpenAI Agents SDK agent loop.

Native calls within a turn execute concurrent independent reads and drain before
writes. All three server subprocesses share the same OS-file-lock capacity slots,
so the same cap applies even when shipped MCP script dispatch bypasses Python's
client semaphore. Tests inspect actual begin/end logs: peak is 2 for native,
WeaveTool and MCP-script paths at width 2. A shared 64-call server budget also
prevents scripts bypassing the normalized trial call limit. No automatic tool
write retries occur. HTTP retries resend identical provider payloads and retain
all waits/known usage; they do not rerun already-executed tool calls.

## Pins and safe defaults

Configuration requires the real checkout commit and exact SHA-256 pins for the
local fixture server, proxy binary, installed native extension and shipped
WeaveTool source. Pins are checked **before HTTP dispatch**; actual source-file,
schema/contract hashes, dirty-source state, Git version and initial commit are
also recorded. Since weave-mcp now forwards upstream `outputSchema`, discovered
output schemas are checked equal to the pinned fixture contract before HTTP.

The provider's reported model revision must match the declared snapshot. Reasoning
items and token-detail subsets are preserved, not added twice to total usage.
Absent or malformed usage stays unknown; explicit zero stays zero. The declared
seed is harness metadata, **not** silently sent as an unsupported Responses seed
field. No client cache/session is reused; provider-side caching is not guaranteed
disabled, and reported cached input is retained. These are wire/reporting checks,
not live billing/model/version attestations.

Default transport permits only literal `http://127.0.0.1:PORT/v1/responses`.
Environment proxies and redirects are disabled; loopback sends no Authorization
header, even if a caller passes a key. No API key is read from the environment.
Remote access requires **both** explicit adapter `live_opt_in=True` with a supplied
key and finite request cap, **and** runner `allow_live=True`; only the fixed
HTTPS OpenAI API host is accepted remotely. No live CLI switch was added, and no
such run is authorized or performed by this stage. Request count is **not a USD
budget guarantee**; credentials, pricing and any later spending approval remain
outside this offline stage.

## Durable append-only accounting

`TrialLedger(path)` stores private mode-0600 JSONL. Each append is protected by a
POSIX file lock, checksummed/hash-chained, file-fsynced and parent-directory-fsynced.
It records trial start before fixture setup, actual fixture manifests, attempt
start before request, HTTP request body/hash before dispatch, response/usage as
soon as available, attempt finish, and the terminal trial row. Headers and API keys
are not recorded. Both the local request and response origin are explicitly
labelled synthetic; full prompts/outputs can still be sensitive in other uses.

```python
from bench.protocol_v3.accounting import TrialLedger
ledger = TrialLedger("/trusted/private/path/trials.jsonl")
# Pass ledger=ledger, trial_id=<unique id> to core.run(...).
rows = ledger.trials(recover_incomplete=True)  # use after workers have stopped
```

Every started trial remains a projected row. Caught interrupts append
`interrupted`; timeout rows retain prior known usage and completed effects.
An unclosed trial defaults to `incomplete`; explicit recovery after worker exit
labels it `interrupted`, **not an inferred timeout/crash**. A supervisor with the
actual process exit code can append `terminated(..., "crashed", exit_code=...)`.
Duplicate trial IDs and duplicate terminal records are rejected atomically.
A received usage record survives a crash before attempt finish. An unanswered
attempt contributes unknown usage, never a fabricated zero. Missing elapsed time
after a crash stays `null`/incomplete; it must not be used as zero latency or
silently removed from total-trial denominators. The projection is accounting
state, not the same schema as `core.summarize`'s completed runner rows.

Truncated tails are retained byte-for-byte; subsequent appends start a new line
and report damaged-line indexes. Nothing truncates or rewrites ledger history.
This is single-filesystem POSIX durability, **not WORM/tamperproof storage**, a
power-cut-tested service, a distributed reconciliation system or a privacy
redactor. Operator-supplied ledger paths must be trusted. Use recovery only after
confirming workers have stopped; active trials are not automatically called
interrupted.

## Run the finite stage without spending

```sh
cargo build -p weave-mcp
python -m unittest bench.protocol_v3.tests.test_adapters -v
python tests/audit/test_benchmark.py
```

Use the installed local Weave extension. The adapter suite had **21 tests** at the
end of this stage (24 after the live runner added multi-script and generation
field tests); with the mechanics tests, **58 tests** now run inside the audit
acceptance matrix. Coverage includes both shipped paths/native parity, real mutations,
actual cross-process capacity, error/call-budget controls, model/artifact drift,
HTTP retry/no-write-retry, zero/missing usage, durable pre-dispatch records,
timeout-after-write, actual abrupt process exit and SIGINT, truncated recovery,
concurrent append and terminal uniqueness. Test evidence is retained under
`tests/audit/evidence/protocol-v3-adapters/`.

## Remaining limitations of this completed stage

- No live provider request, provider-side schema acceptance/billing attestation,
  model capability measurement, held-out experiment or speedup evaluation occurred.
  Synthetic fixtures are development mechanics evidence only.
- Local MCP servers cover this explicit file/git/memory subset, not all reference
  server behavior, external infrastructure, multimodal results or other providers.
- Owned local subprocess groups are quiesced before final state inspection, and
  ordinary shutdown closes their resources. Deadlines remain soft: HTTP reads,
  pipe/file I/O, fsync or host callbacks can overrun before control returns. Killing
  local work does not roll back completed writes or cancel arbitrary remote effects.
- Journal privacy is access control, not comprehensive redaction; prompts, responses,
  tool values and tracebacks emitted by separately configured servers can be sensitive.
  Hard process death cannot invent missing elapsed/billing data; it preserves the
  started row and explicit unknowns instead.

Stage complete. The later live runs (2026-11-09) used `bench/protocol_v3/live.py`;
see [benchmarks](benchmarks.md#live-results-protocol-v3-2026-11-09).
