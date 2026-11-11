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
| dev (used for tuning) | `57d0ad58ea3b4ee2` | fanout_sum n8, partial_failure n8, order_value n5, chain n4, triage n8 |
| **held_out** | `654aa649fbbd00e1` | fanout_sum n20 s101 and n12 s102, partial_failure n16, order_value n10, chain n6, triage n15, notify_gold n12 and n20, top_customer n8 and n15 |
| scaling | `6302bdbc79c4a8a6` | fanout_sum n = 5, 10, 20, 35, 50; chain depth 2, 4, 8 |

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
