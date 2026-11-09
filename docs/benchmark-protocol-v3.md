# Controlled benchmark protocol v3

**Live results now exist:** see [benchmarks](benchmarks.md#live-results-protocol-v3-2026-11-09)
for the 2026-11-09 OpenAI runs (three models, 60 trials each, two recorded
runs). They show no speedup and none is claimed. This page defines the
protocol; the sections below on remaining gates still apply to any broader
claim (other providers, held-out tasks, compact-output mode).

This implements a controlled comparison core, not a claim that the shipped MCP /
public agent-SDK integration has been evaluated fairly end-to-end. It replaces
neither historical measurements nor independent empirical review. Audit W06 and
the roadmap's benchmark recommendations are the acceptance checklist.

## Run without credentials or network

```sh
python -m unittest bench.protocol_v3.tests.test_protocol -v
python tests/audit/test_benchmark.py
python -m bench.protocol_v3.offline --output /tmp/weave-offline-mechanics.json
```

Use the locally built/installed Weave Python extension. The first command tests
protocol mechanics with scripted fake provider responses; the second also keeps
legacy v2 endpoint/parallel-MCP regression checks. The final command emits six
**hand-authored development cassette** rows using a synthetic clock. This is
not a recorded model replay, a model prediction, or a live performance sample.
No API credentials are read by v3. [The completed adapter stage](benchmark-adapter-stage.md) adds Responses wire
formatting and tests with synthetic loopback HTTP, plus actual local MCP execution.
Remote providers remain rejected by default; adapter and runner both require
explicit non-default opt-ins, which are not enabled in these tests. This is an interface guard, not a
network sandbox against malicious Python code claiming to be offline.

The old `real_tools_eval.py` live entrypoint exits 2 without contacting a provider.
Its runner helpers remain only for historical regression fixtures. Historical
`bench/results/*` files and measurements are untouched; no old response is
regraded as a new live evaluation, and no historical chart is regenerated.

## Exact controlled contract

1. Each task/mode/repeat gets a new temporary filesystem backend and a new provider
   conversation. Baseline bytes are identical; initial/final state hashes and the
   canonical schema/context hashes are recorded. Runtime/fixture startup, grading and owned-process quiescence before final
   inspection are inside the row timer; temporary-directory deletion follows the
   endpoint and is excluded.
   Roots are never put into model prompts. Requests use relative paths, reject
   traversal, and do not access an arbitrary user's files.
2. Both modes receive the **same** task instruction, complete JSON input/output
   schemas, tool names, descriptions, effects, generation settings and common
   system text. Neither gets demonstrations, grader answers, expected writes or
   oracle state. Only the mode's action grammar differs (`script` vs `calls`).
3. Both execute the same canonical backend. No mode-specific result shaping,
   textual prefix, truncation, sampled examples or privileged data. Strings,
   nulls, arrays and tool-error objects use identical JSON representations. A
   script's prints/bindings/trace are **not** sent as special extra context or
   accepted as the final answer. Both receive all underlying tool observations.
   Read-only segments are canonically ordered for feedback, with write barriers
   retained. This intentionally disables Weave's compact-output/context-saving
   advantage: it is a controlled full-observation ablation, not that product mode.
4. Native independent literal calls in one provider turn use a bounded thread
   pool. Reads drain before a write; later reads wait for the write. The backend
   semaphore applies the **same concurrency cap to both modes** (default 4,
   range 1–16). Writes are never automatically retried. Dependent native calls
   need another provider turn; scripts can express dependencies internally.
5. Only a provider's final `answer` decision reaches the grader. Grading requires
   exact answer (outer whitespace ignored), exact actual file snapshot, and exact
   ordered successful-write **and attempted-write** logs. Even a forbidden write
   blocked by the backend fails the action contract; unchanged state is not enough. A correct claimed answer without its write,
   wrong content, an extra file, or a repeated same-content write fails. Grading
   reads real temporary files, not the provider's assertion or tool status alone.
6. Both modes get at most 10 logical provider turns, 3 final-answer submissions,
   and 2 provider transport retries per turn. Each turn allows at most 64 calls;
   the fixture limits total calls to 64. Weave also retains its runtime source,
   value/output/trace/step limits; native has no language parser/step equivalent.
   Invalid invocation shape gets the same generic execution-repair feedback.
   Backend tool errors have identical objects; Weave-only parse/runtime errors
   necessarily add a generic execution diagnostic. Wrong answer/state feedback
   is identical and never reveals expected values. State is **not reset during
   repairs**: prior effects persist and repeated writes can cause failure.
7. Each provider attempt records turn/retry index, request hash, outcome, usage,
   elapsed time and retry wait. Retried requests have identical content hashes.
   Total elapsed time **includes** rate limits, backoff, failed attempts, tool
   execution and synthesis; waits are never subtracted. Rate-limit wait and all
   retry wait are separate. Known input/output usage accumulates for every attempt;
   cached input is a subset of input, not extra tokens. Unknown failure usage is
   explicitly counted and makes totals incomplete, never silently zero/complete.
8. Timeout, exhausted budget, provider/adapter error, malformed response, turn
   exhaustion, fixture/harness failure, call-budget exhaustion and wrong answer/state retain failure rows
   with actual observed elapsed time and prior usage. Timeout can overrun: the
   synchronous adapter/backend must return before accounting can finish. No hard
   termination claim. Failed/slow rows remain in total-run denominators and
   all-run p50/p95 (nearest-rank p95). Missing required writes, unexpected successful
   writes and actual state mismatches are reported separately, including effects
   completed before a later provider timeout. There is no success-only speedup statistic.
9. `first_pass` means first final submission passed with no prior execution
   repair; `first_answer_pass` separately reports first final-submission grading.
   Transport retries and expected/caught backend errors are separately visible,
   not silently removed. Model turns and physical provider attempts are distinct.

## Pins and reproducibility

`Config` requires a full source commit, declared requested/resolved model revision,
provider/API version, server/backend version map, temperature/max-output/reasoning
settings, seed, cache policy, concurrency and retry/answer/turn/deadline limits.
Replies with a different reported model revision fail. Unresolved/latest markers
and incomplete configurations are rejected. Reports whitelist config fields and
never dump environment variables, authentication headers or keys. The standalone
runner additionally reports source dirty state and a hash of protocol source files.

The adapter stage also checks actual local artifact SHA-256 pins before HTTP
dispatch. These checks validate reporting mechanics, **not remote attestation**. A literal
version string is not proof of immutable server/model weights; no live package
lock resolution, artifact download, SDK retry interception or billing verification
has occurred. No client cache/session is reused; provider-side caching is not guaranteed
disabled and reported cached usage is retained. Live price/cost accounting is not implemented.
Seed/config fields do not assert provider determinism. Controlled mode has no
cache or shared provider session across rows.

## Development versus candidate held-out sets

`bench/protocol_v3/tasks.py` separates three development tasks from three
`held_out_candidate` tasks and exposes a versioned manifest hash. The routine
cassette runner uses **development only**. Tests reject cross-split duplicate
IDs, families, exact instructions and fixture hashes. One candidate uses unfamiliar
tool aliases/schemas; mechanics tests verify those propagate to both modes.

The candidate set is public, maintainer-authored and exercised by mechanics tests.
It is **not secret, genuinely unseen, or statistically independent**. Exact
collision checks do not detect semantic near-duplicates. Tasks share primitives
and families may still be related; repetitions do not become new independent
workloads. Do not tune prompts on this candidate set and then call it held-out
model evidence. Freeze prompts, adapters and all manifest/oracle hashes, then
obtain a separately reviewed, sequestered final evaluation set; group analysis
by task/family rather than treating every repeat or variant as independent.
Expected answers/state live in grader objects and fake response cassettes, never
in the provider request. Scripted cassette answers prove no model capability.

## Remaining gates before a valid live evaluation

- The completed adapter stage now implements/reviews Responses wire mechanics
  with synthetic loopback HTTP and disabled hidden retries. Actual provider-side
  acceptance and billing attestation have not been exercised; retain
  bounded transport calls, returned model IDs, all usage/cache/thinking categories,
  rate-limit/budget accounting, model/package/API/price pins and partial evidence
  checkpoints. Unknown billable usage must remain an explicit cost gap.
- Validate both provider wire schemas/results/error feedback and actual shipped
  MCP/agent-SDK integrations against this core. The normalized action grammar
  is minimal; actual language guides, provider wire prompts and equivalent
  affordances still require independent review before live use. The core default backend is local filesystem; the completed adapter stage adds
  actual local MCP subprocesses with file/git/memory mutation oracles. These are
  explicitly pinned fixture subsets, not the reference packages or network stack.
- Provide owned process/transport cancellation and termination for timeout cases;
  synchronous callbacks can overrun. The completed stage now journals trial start before allocation and retains
  crash/interrupted rows. Hard-death elapsed/usage values can remain unknown; no
  distributed reconciliation or rollback guarantee is implied.
- Preregister a seeded, counterbalanced/interleaved run schedule, frozen private
  held-out family groups, repeat/sample sizes, total spend cap, stopping rules,
  error/timeout/wrong-write rates and uncertainty analysis. The offline demonstration
  alternates mode order, not a statistical experimental schedule. Log full prompts
  and response/result bytes with explicit privacy policy and human review.
- Evaluate compact-output mode separately with its unequal observations explicitly
  declared; compare native-serial, native-parallel and other code-execution options.
  Include tasks where native wins and report all failures, first-pass/eventual
  outcomes, p50/p95 and tokens/costs without survivor filtering.
- Fresh live evaluations were run on 2026-11-09 with explicit spending
  authorization (OpenAI only). No speedup or cross-provider generalization
  claim is supported by them.

## Live runner

`python -m bench.protocol_v3.live --allow-live --model <snapshot>[:effort]`
is the only live entrypoint. It requires the runner flag **and** sets the
adapter's `live_opt_in` itself, reads `OPENAI_API_KEY` from the environment
(never written anywhere), refuses a dirty checkout, caps HTTP requests, runs
the frozen [`live_tasks.py`](../bench/protocol_v3/live_tasks.py) set over
`RemoteFixture(path="weave_mcp")` for **both** modes, and writes `run.json`,
the append-only `trials.jsonl` ledger, `rows.jsonl` and `summary.json`
(Wilson intervals for success, seeded bootstrap intervals for means,
nearest-rank p95). `python -m bench.protocol_v3.report` prints the markdown
tables used in the docs.

Adapter details added for live use: a pinned generation field set to `None`
is declared and not sent (reasoning models reject `temperature`; others reject
`reasoning`), and the per-request HTTP timeout is configurable (default 10 s;
the runner uses 120 s). Several well-formed `run_weave` calls in one provider
turn run in call order and each `call_id` receives its own observations (at
most 8; more is an invalid shape that gets the generic repair feedback). This
matches how an agent loop executes parallel tool calls, and native mode
likewise accepts several calls per turn.

## Current acceptance evidence

34 deterministic protocol tests plus 24 synthetic-HTTP/local-MCP adapter tests
(58 combined) cover real filesystem/native-versus-
Engine checks, shared concurrency barriers (not a wall-clock speed assertion),
negative cases, failure tails, oracle/schema hashing, resource-limit failure,
several scripts per turn, unsent generation fields, and write completion before
timeout. The adapter stage adds real git/memory
writes, shipped execution paths, artifact pins and durable interruption recovery.
`tests/audit/test_benchmark.py` runs these
inside the existing audit matrix. `bench/results/offline-v3-mechanics.json` is
six scripted development rows, explicitly offline and synthetic-clock; it is not
a model success measurement. Live rows are under
`bench/results/protocol-v3-live/`. Full local Rust/clippy, SDK and eight audit
acceptance gates are retained under `tests/audit/evidence/benchmark-protocol/`.

Adapter-stage logs are retained separately under
`tests/audit/evidence/protocol-v3-adapters/`; original evidence is unchanged.
