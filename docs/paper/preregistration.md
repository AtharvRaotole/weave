# Pre-registration: Weave benchmark protocol v4

Written and committed **before any live run on the held-out or scaling suites** (2026-11-10, America/New_York).
The commit that adds this file freezes the Weave engine, the agent prompt and tool listing
(`python/weave/agent_tool.py`), the harness (`bench/v4/`), and the task suites. Any later change to those files
before the held-out runs finish voids the held-out claim, and the paper must say so.

## What is compared

Two agent arms solve the same tasks against the same simulated backend (`bench/v4/world.py`):

- **native**: the model gets one JSON tool per backend function (equal schemas, equal descriptions, return types
  shown) and may emit parallel tool calls; the harness runs the reads of one turn concurrently and the writes in
  order, like a well-built agent loop.
- **weave**: the model gets one tool, `run_weave`, whose description is the Weave rules and the same function list.
  Several `run_weave` calls in one turn run concurrently if read-only.

Same system prompt, same model, same settings, same endpoint, at most 16 model turns. A trial passes only if the
final answer matches **and** the backend's write log matches the expected writes exactly (no missing, extra or
duplicate writes). Tokens include every HTTP attempt, retries included. Per-call backend latency is drawn
from mean × U(0.5, 1.5) with a seeded RNG, at most 8 backend calls in flight.

## Suites (frozen)

Manifest hashes from `python -c "from bench.v4.tasks import manifest; print(manifest('held_out')['hash'])"`:

| suite | hash | tasks |
|---|---|---|
| dev (used for tuning) | `f66da35bc9ee37a3` | fanout_sum n8, partial_failure n8, order_value n5, chain n4, triage n8 |
| **held_out** | `c23fb383af6b5ef2` | fanout_sum n20 s101 and n12 s102, partial_failure n16, order_value n10, chain n6, triage n15, notify_gold n12 and n20, top_customer n8 and n15 |
| scaling | `256f002c7ff5b742` | fanout_sum n = 5, 10, 20, 35, 50; chain depth 2, 4, 8 |

Revision note: the first version of this file (commit 7285676) listed hashes that changed from process to
process, because task ids were shuffled from a `set` whose order depends on `PYTHONHASHSEED`. The held-out guard in
`bench/v4/run.py` refused every live run for that reason, so no held-out or scaling trial ran before this fix. The
generator now sorts before shuffling and a test checks the hashes under three hash seeds. Dev runs made before
the fix drew a different random dev instance in each process; they were used only for tuning.

Held-out uses new seeds and sizes for every dev family and two families never run live during development:
`notify_gold` (conditional fan-out, dedup, irreversible sends) and `top_customer` (fan-out + argmax).

## Hypotheses

Pooled over held-out tasks, per model, at 200 ms mean latency. "Difference" is weave minus native, mean of per-task
means, 95% cluster-bootstrap CI over tasks (4000 resamples, seed 0). "Ratio" is the geometric mean of weave/native.

- **H1 (turns).** Weave uses fewer model turns: difference CI entirely below 0.
- **H2 (wall time, 200 ms).** Weave has lower end-to-end wall time: difference CI entirely below 0.
- **H3 (tokens).** Weave uses no more total tokens than native: ratio point estimate ≤ 1.0. (Reported with CI; we
  expect savings to grow with fan-out width because native carries every tool result in context.)
- **H4 (success, non-inferiority).** Weave pass rate is not more than 10 percentage points below native. Reported
  with Wilson 95% intervals and an exact McNemar test on paired outcomes.
- **H5 (expected negative).** On write-in-loop families (`triage`, `notify_gold`) Weave's script execution time is
  **higher** than native's, because a read issued after a write in a loop waits for that write (reads gate on the
  last mutation; footprints are not yet used for scheduling). We report it whatever the result.
- **H6 (chain).** On `chain`, native needs about one turn per hop while Weave needs about two turns in total.
- **H7 (zero latency).** At 0 ms backend latency, wall-time gains shrink to whatever the saved model turns are
  worth; H2 is not claimed at 0 ms.
- **H8 (scaling).** On the scaling suite, native tokens grow faster with fan-out width than Weave tokens
  (least-squares slope of tokens vs n), and native turns grow with chain depth while Weave turns stay flat.

## Runs (each held-out configuration run once)

| provider | model | settings | suite | repeats | latency (ms) |
|---|---|---|---|---|---|
| openai | gpt-5.4-mini-2026-03-17 | effort low | held_out | 3 | 200, 0 |
| openai | gpt-5.5-2026-04-23 | effort low | held_out | 3 | 200, 0 |
| openai | gpt-4.1-2025-04-14 | temperature 0 | held_out | 3 | 200, 0 |
| openai | gpt-5.4-mini-2026-03-17 | effort low | scaling | 3 | 200, 0 |
| openai | gpt-5.4-mini-2026-03-17 | effort low, modes weave_nopipe,weave | held_out | 1 | 200 |
| gemini | gemini-3.5-flash | defaults (free tier) | held_out | 1 | 200 |
| nvidia | z-ai/glm-5.3-flash | temperature 0, best effort | held_out | 1 | 200 |

Gemini runs on the free tier (about 20 requests a day), so it is expected to cover only part of the held-out suite;
we report how many trials completed and do not pool partial Gemini or GLM data with OpenAI data. If the NVIDIA
endpoint does not answer (it timed out on 2026-11-10 at 20:49 ET), the GLM row is reported as not run.

## Stopping and exclusion rules

- `--token-budget 3000000` per invocation; total live spend for the round stops at 8M tokens.
- No held-out configuration is rerun to get a better number. A configuration is rerun only if the run crashed
  with no model output at all (harness or network failure), and the paper lists every such rerun.
- Trials that end with a provider error count as failures for both success and cost; they are not dropped.
- No changes to Weave, the prompt, the listing or the tasks between held-out runs. Fixes found from held-out traces
  go into a later version and are not evaluated on this held-out set.

## Analysis

`bench/v4/run.py` writes `summary.json` per run (per split and latency: per-mode pass rate with Wilson CI,
bootstrap CIs of turns, wall time and tokens, paired differences and ratios, McNemar, per-family breakdowns).
`bench/v4/charts.py` builds every figure in the paper from the committed `bench/results/v4/` data only.

## Addendum (written after the held-out runs, before any run on `confirm`)

The held-out runs finished at commit `8dae373` (ablation at `29f7057`, which only added `bench/v4/charts.py`).
Their traces showed that every GPT-4.1 Weave failure (4 of 30) was the same thing: the script called
`max(pairs, key=lambda p: p[1])`, the static check rejected `lambda` with an unhelpful hint, and the model then
guessed. Commit `59a5465` changed only error messages and list ordering (lists now compare lexicographically, and
`key=`/`lambda` errors name the loop to write). Those fixes are **not** evaluated on the held-out set.

To test them we add a `confirm` suite: the held-out recipe with every seed shifted by 100, so none of its tasks were
seen before (manifest hash `f0829e845bfd6e2b`).

| provider | model | settings | suite | repeats | latency (ms) |
|---|---|---|---|---|---|
| openai | gpt-4.1-2025-04-14 | temperature 0 | confirm | 3 | 200 |

- **C1.** GPT-4.1 Weave pass rate on `confirm` is at least its held-out rate (28/30), and no Weave failure is a
  repeat of the `key=lambda` dead end.
- **C2.** H1–H3 hold on `confirm` for GPT-4.1 (same tests as above).
The same stopping and exclusion rules apply; this suite is run once.

## Addendum 2: round 2 (written and committed before any run on `confirm2` or `real`)

Since `confirm`, three things changed. (1) **Footprint-aware ordering** (`crates/weave/src/exec.rs`): a read waits
only for earlier writes whose declared footprint overlaps its own, and loops whose bodies write are pipelined (a
call behind a condition or a `break` is still never issued early). Tools without declarations keep the old order.
The benchmark tools now declare footprints (`FOOTPRINTS` in `bench/v4/world.py` and `bench/v4/real.py`); the
`weave_nofp` arm is the same engine with no declarations. (2) **Final-expression echo**
(`python/weave/agent_tool.py`, made from `confirm` traces): a script whose last statement is a bare expression
prints it. It has not been run live. (3) A **real-services backend** (`bench/v4/real.py`): SQLite, a git repository,
the filesystem, a real HTTP server on 127.0.0.1, and the public PyPI JSON API. No injected latency.

Dev measurements used for this round: `bench/results/v5/*-dev-*` and `*-real_dev-*` (GPT-5.4 mini). Nothing below
was run before this addendum was committed.

| suite | hash | tasks |
|---|---|---|
| **confirm2** | `436f52507b73d53f` | the held-out recipe with seeds + 200, plus triage n10 s303, triage n24 s304, notify_gold n16 s303 |
| **real** | `69b9d2427132f664` | pypi_major n10 s101 and n16 s102, sqlite_triage n12 and n20, git_review n8 and n14, warehouse_shortfall n10 and n16 |

Runs (each configuration once; same stopping and exclusion rules; round-2 live spend stops at 8M tokens):

| provider | model | settings | suite | modes | repeats | latency (ms) |
|---|---|---|---|---|---|---|
| openai | gpt-4.1-2025-04-14 | temperature 0 | confirm2 | weave, native, weave_nofp | 3 | 200 |
| openai | gpt-5.4-mini-2026-03-17 | effort low | confirm2 | weave, native, weave_nofp | 3 | 200 |
| openai | gpt-5.5-2026-04-23 | effort low | confirm2 | weave, native, weave_nofp | 1 | 200 |
| openai | gpt-4.1-2025-04-14 | temperature 0 | real | weave, native | 3 | real |
| openai | gpt-5.4-mini-2026-03-17 | effort low | real | weave, native | 3 | real |
| openai | gpt-5.5-2026-04-23 | effort low | real | weave, native | 1 | real |
| gemini | gemini-3.5-flash | free tier, 1 worker, ≥ 12 s between requests | real, then confirm2 | weave, native | 1 | real / 200 |
| nvidia | z-ai/glm-5.3-flash | temperature 0, only if a 35 s smoke request answers | real | weave, native | 1 | real |

Gemini and GLM are best effort: if quota or the endpoint stops a run, we report the trials that finished and the
reason, and do not pool them with OpenAI data. Pacing waits are recorded in `retry_wait_s`, and Gemini wall time
is reported net of them.

Hypotheses:

- **F1 (footprints close the write-loop gap).** On the `triage` and `notify_gold` tasks of `confirm2`, per OpenAI
  model, the geometric-mean ratio of Weave to native script/tool execution time (`exec_s`) is at most 1.2. Round 1
  measured about 2× on held-out triage.
- **F2 (the gap was the ordering).** On the same tasks, Weave `exec_s` is lower than `weave_nofp` `exec_s`:
  paired (task, repeat) ratio below 1 with a 95% bootstrap CI below 1, for GPT-4.1 and GPT-5.4 mini.
- **F3 (no correctness cost).** Weave pass rate on `confirm2` is not lower than `weave_nofp` by more than one
  trial per model, and no Weave failure shows a read that saw a stale value because of reordering.
- **E1 (echo).** On `confirm2`, no Weave script whose last statement is a bare expression gets the
  "the script printed nothing" reply (the case the echo fixes). GPT-4.1 Weave pass rate is at least 90%.
- **C3.** H1 (turns), H3 (tokens, ratio ≤ 1) and H4 (pass rate, non-inferiority) hold on `confirm2` for each OpenAI
  model. H2 (wall time at 200 ms) is tested for GPT-4.1 and GPT-5.4 mini.
- **R1 (real services).** On `real`, per OpenAI model: Weave pass rate is non-inferior (H4 margin), turns are lower
  (CI below 0), and tokens are no higher (ratio ≤ 1). We report the wall-time difference with its CI but predict
  a gain only on the multi-round families (`sqlite_triage`, `git_review`, `warehouse_shortfall`). On
  `pypi_major` (one round of parallel calls for native) we expect no clear wall-time difference.
