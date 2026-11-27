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

## Addendum 3: round 3 (written and committed before any run on `confirm3` or `real2`)

Since Addendum 2, one thing changed: **the Python models reach for now runs** (commit `08fcb52`,
`crates/weave/src/parse/sugar.rs` and `crates/weave/src/anf.rs`). List, dict and set comprehensions, generator
expressions, `key=`/`reverse=`/`default=` on `sorted`/`min`/`max`/`list.sort`, `map`/`filter`, sets (as
duplicate-free lists), and top-level `def` helpers (inlined per call; recursion, nested `def`, `*args`, non-literal
defaults and in-place changes to arguments are rejected with hints). All of it lowers to the statements Weave
already had, so effects, scheduling, replay and limits are unchanged; `any`/`all` over a generator stop early, as in
Python. The agent rules in `python/weave/agent_tool.py` now list these forms instead of forbidding `def`, `lambda`
and comprehensions. That text is the Weave arm's tool description; the native arm is unchanged. All 46 scripts that
the round-1 and round-2 Weave arms had rejected for these forms (or for `list.sort`) pass the static check now
(offline re-check of the logged sources).

Dev measurement used for this round: `bench/results/v6/*-real_dev-*sugar-dev` (GPT-4.1, 2 repeats): Weave 7/8,
native 6/8; mean wall 2.58 s against 7.20 s; the one Weave failure split a PyPI task into six one-line scripts and
added the numbers itself. Nothing below was run before this addendum was committed.

| suite | hash | tasks |
|---|---|---|
| **confirm3** | `fec37fb8e1c51615` | the held-out recipe with seeds + 300, plus triage n10 s403, triage n24 s404, notify_gold n16 s403 |
| **real2** | `4ebcd0a29bb76666` | the `real` recipe with seeds + 100: pypi_major n10 s201 and n16 s202 (sum of major versions from the live PyPI API), sqlite_triage n12 and n20, git_review n8 and n14 (creating real git tags), warehouse_shortfall n10 and n16 |

Runs (each configuration once; same stopping and exclusion rules; round-3 live spend stops at 6M tokens):

| provider | model | settings | suite | modes | repeats | latency (ms) |
|---|---|---|---|---|---|---|
| openai | gpt-4.1-2025-04-14 | temperature 0 | confirm3 | weave, native | 3 | 200 |
| openai | gpt-5.4-mini-2026-03-17 | effort low | confirm3 | weave, native | 3 | 200 |
| openai | gpt-5.5-2026-04-23 | effort low | confirm3 | weave, native | 1 | 200 |
| openai | gpt-4.1-2025-04-14 | temperature 0 | real2 | weave, native | 3 | real |
| openai | gpt-5.4-mini-2026-03-17 | effort low | real2 | weave, native | 3 | real |
| openai | gpt-5.5-2026-04-23 | effort low | real2 | weave, native | 1 | real |
| gemini | gemini-3.5-flash | free tier, 1 worker, ≥ 12 s between requests, only if the daily quota has reset | real2, then confirm3 | weave, native | 1 | real / 200 |
| nvidia | z-ai/glm-5.3-flash | temperature 0, only if a 35 s smoke request answers | real2 | weave, native | 1 | real |

Gemini and GLM stay best effort and are not pooled with OpenAI data.

Hypotheses:

- **S1 (the rejections are gone).** Across every OpenAI Weave trial on `confirm3` and `real2`, no script fails the
  static check because of `def`, `lambda`, a comprehension or `list.sort`. We report every static-check failure
  by message, including the forms that remain unsupported.
- **S2 (the GPT-4.1 real-services loss is removed).** On `real2`, GPT-4.1: Weave is faster per task (paired mean
  Δ wall below 0, with the 95% bootstrap CI below 0), turns are lower (CI below 0), the pass rate is non-inferior
  (H4 margin), and tokens are no higher (ratio ≤ 1). In round 2 the wall-time difference was +0.1 s [−1.7, +2.0].
- **S3 (PyPI).** On the `pypi_major` tasks of `real2`, GPT-4.1 Weave mean wall time is at most native's. In round 2
  it was 7.5 s against 3.1 s. (Descriptive; with 6 pairs per arm we report the paired CI but do not require it to
  exclude 0.)
- **R2.** R1 (Addendum 2) holds on `real2` for GPT-5.5 and GPT-5.4 mini.
- **C4.** H1 (turns), H3 (tokens, ratio ≤ 1) and H4 (pass rate, non-inferiority) hold on `confirm3` for each OpenAI
  model. H2 (wall time at 200 ms) is tested for GPT-4.1 and GPT-5.4 mini.

## Addendum 4: round 4 (written and committed before any run on `confirm4`)

Since Addendum 3, three things changed.

1. **Accounting.** A Responses request that ends `incomplete` (for example at `max_output_tokens`) is billed in
   full but was recorded as 0 tokens. The harness now adds the provider-reported usage of such a request to the
   trial (`billed_on_error`) and keeps a short excerpt of its output (`partial_output`); every attempt also logs its
   usage. Requests that fail without a usage report (HTTP 429/5xx, timeouts) still add 0, as before. The round-1–3
   ledgers did not keep the usage of the three affected OpenAI trials (round-2 `real` GPT-4.1 ×1, round-3 `real2`
   GPT-4.1 ×2), so past tables are not re-derived; their unrecorded spend is at least 3 × 4,096 output tokens and is
   disclosed.
2. **One script per turn.** Diagnosis on the dev-only suite `real_dev_git` (`bench/results/v7/diag-turn1/NOTES.md`):
   GPT-4.1 at temperature 0 usually answers the first turn with two parallel `run_weave` calls, and in 6 of 64
   first-turn samples it then ran away to `max_output_tokens` with nothing more in the output. With
   `parallel_tool_calls` off it sent one script every time (0 of 28 ran away). The round-3 "printed the tags instead
   of creating them" failure also came from splitting the work across parallel scripts and re-typing results.
   `WeaveTool.request_options` now returns `{"parallel_tool_calls": false}` and the tool rules say to call the tool
   once per turn because scripts do not share variables. **Deviation from identical settings, declared here:** the
   Weave arm sends `parallel_tool_calls: false` (the product's recommended setting); the native arm keeps `true`,
   which is what native tool calling needs to batch calls. Model, temperature/effort, max output tokens (4,096),
   system prompt, task text, tools and backends stay identical.
3. Dev results after the change (not evidence): GPT-4.1 `real_dev_git` Weave 24/24, 2.00 turns, 2,233 tokens per trial
   (round-3 product on the same suite: 22/24, 2.92, 4,960, the two failures incomplete); GPT-4.1 `real_dev` Weave 12/12
   vs native 9/12; GPT-5.4 mini `real_dev_git` Weave 24/24.

| suite | hash | tasks |
|---|---|---|
| **confirm4** | `c3ae4a6c9fe95a31` | the `real` recipe with seeds + 200 (pypi_major n10 s301, n16 s302; sqlite_triage n12, n20; git_review n8, n14; warehouse_shortfall n10, n16) plus git_review n10 s303 and n18 s304 |

Runs (each configuration once; same stopping and exclusion rules; provider errors count as failures; round-4 live
spend stops at 1.5M tokens):

| provider | model | settings | suite | modes | repeats |
|---|---|---|---|---|---|
| openai | gpt-4.1-2025-04-14 | temperature 0 | confirm4 | weave, native | 3 |
| openai | gpt-5.4-mini-2026-03-17 | effort low | confirm4 | weave, native | 3 |
| gemini | gemini-3.5-flash | free tier, 1 worker, ≥ 12 s between requests; one attempt, only if a smoke request answers | confirm4 | weave, native | 1 |
| nvidia | z-ai/glm-5.3-flash | temperature 0; one attempt, only if a 35 s smoke request answers | confirm4 | weave, native | 1 |

Gemini and GLM stay best effort and are not pooled.

Hypotheses (OpenAI only):

- **G1 (no runaway).** No Weave trial on `confirm4` ends `provider_error:incomplete`.
- **G2 (git_review).** On the four `git_review` tasks, GPT-4.1 Weave passes at least as many trials as native
  (12 per arm) and its mean wall time is at most native's (descriptive; paired CI reported).
- **G3 (the S2 wall-time gap closes).** On `confirm4`, GPT-4.1: paired mean Δ wall < 0 with the 95% bootstrap CI below
  0; Δ turns CI below 0; token ratio CI upper bound ≤ 1; pass rate non-inferior (H4 margin).
- **G4.** The same four criteria for GPT-5.4 mini.
- **A1 (accounting).** Every row whose provider returned usage has it counted; no `incomplete` row has 0 tokens.

## Addendum 5: round 5 (written and committed before any run on `retail_test7` or `confirm5`)

Round 5 addresses two threats: the suites were written by us, and in round 4 the arms differed in one setting
(`parallel_tool_calls`).

**External benchmark: tau-bench retail.** [tau-bench](https://github.com/sierra-research/tau-bench) (Sierra, MIT
license), pinned at commit `59a200c6d575d595120f1cb70fea53cef0632f6b`, installed unmodified. Native arm: tau-bench's
own `ToolCallingAgent`, unchanged (one tool call per model step; it keeps only the first call of a step). Weave arm:
`bench/taubench/weave_agent.py`, the same loop with one `run_weave` tool whose script calls the same tau-bench tools,
each through `env.step(Action(...))`. Tasks, wiki (system prompt), tools, user simulator (tau-bench's LLM user,
`gpt-4.1-2025-04-14`, default sampling) and grader (`Env.calculate_reward`: database hash plus required outputs) are
tau-bench's. Effects declared by us: lookups are `read`, `calculate`/`think` are `pure`, the rest `irreversible`.
**Both arms send `parallel_tool_calls: false`** (one metering wrapper around litellm applies it to every agent request
with tools), so the request settings are identical. GPT-5.4 mini gets `reasoning_effort: low` and no temperature in both
arms; GPT-4.1 temperature 0. `max_num_steps` 30 (tau-bench's default).

| subset | manifest hash | tasks |
|---|---|---|
| **retail_test7** | `eaf9d60846f82626` | retail test split indices 18, 22, 45, 52, 56, 76, 105 = `sorted(random.Random("weave-round5-tau-retail-test").sample(range(115), 7))` |

Seven tasks, one repeat, per model and arm (28 trials). The size is set by the budget: dev runs on the train split
cost about 50k tokens per trial (agent plus user simulator). The suite was chosen by the seed above, not by looking at
tasks. Dev (train split tasks 0–5, GPT-4.1, not evidence): Weave 5/6, native 5/6; 10.2 against 11.0 agent calls; 52.7k
against 48.6k tokens per trial; 15.3 s against 15.3 s. Weave's scripts made one tool call each: these tasks are paced
by the conversation (the policy requires the user's confirmation before every write), so we expect little or no gain.

Hypotheses on `retail_test7` (two-sided, descriptive; n = 7 is too small for significance, and we will say so):

- **T1 (no harm).** Weave's pass count is at least native's minus 1, per model.
- **T2 (agent calls).** Mean agent model calls per task, Weave against native, with a paired CI.
- **T3 (tokens).** Total tokens (agent plus user simulator), geometric-mean ratio Weave/native with CI. Dev suggests a
  ratio above 1 (Weave's tool description is longer); we report it either way.

**Symmetric ablation of `parallel_tool_calls`: `confirm5`** (hash `28f7774434ab8d21`): the `confirm4` recipe with
seeds + 100. Four arms, every (task, repeat) run in each: `weave` (off, the product default), `weave_ptc` (Weave with
parallel calls on), `native` (on, round 1–4 setting) and `native_noptc` (native with them off). GPT-4.1 (temperature 0)
and GPT-5.4 mini (effort low), 1 repeat (40 trials per model). Hypotheses:

- **P1.** Native with parallel calls off against on: pass, turns, wall and tokens (paired CIs; descriptive).
- **P2.** Weave with parallel calls on against off: number of `incomplete` trials and pass count (descriptive; round 4
  predicts more runaways with them on for GPT-4.1).
- **P3 (the round-4 result holds under equal settings).** Weave against native with both off, and with both on: Δ wall
  CI below 0, Δ turns CI below 0, token ratio CI upper bound ≤ 1, pass non-inferior (H4 margin), per model.

Runs (once each; provider errors count as failures; round-5 live spend stops at 2.5M tokens; each tau-bench model run is
capped at 750k tokens, and trials not started because of the cap are reported as not run):

| harness | model | suite | arms |
|---|---|---|---|
| tau-bench | gpt-4.1-2025-04-14 (t = 0) | retail_test7 | weave, native |
| tau-bench | gpt-5.4-mini-2026-03-17 (effort low) | retail_test7 | weave, native |
| bench/v4 | gpt-4.1-2025-04-14 (t = 0) | confirm5 | weave, weave_ptc, native, native_noptc |
| bench/v4 | gpt-5.4-mini-2026-03-17 (effort low) | confirm5 | weave, weave_ptc, native, native_noptc |
| bench/v4 | gemini-3.5-flash, z-ai/glm-5.3-flash | confirm5 | weave, native; one attempt each, only if a smoke request answers |

## Addendum 6: round 6 (written and committed before any run on `retail_test10r6`)

Round 6 changes the Weave arm and reruns tau-bench retail on fresh test tasks. OpenAI models only.

**What changed since round 5 (all tuned only on the tau-bench train split and our dev suites).**

- Language: dict iteration, `except Exception` and builtin exception classes, `raise`/`assert`, `isinstance`,
  `round`, format specs, `str.format`, `%` formatting, `json.dumps`, `d.update`/`d.pop`; fixes to `//`, `%` and float
  printing found by a new CPython differential suite (`tests/audit/test_python_idioms.py`).
- Agent tool (`python/weave/agent_tool.py`): `WeaveTool(conversational=True)` adds rules for hosts where a person
  replies between turns (fetch everything the next reply needs in one script; no state change before the person
  confirms it); `persist_variables=True` keeps earlier scripts' variables, like a notebook; `shape_of_samples` renders
  return shapes from sample results.
- tau-bench Weave arm (`bench/taubench/weave_agent.py`): uses both options, and lists each read tool's return shape,
  merged from calls on a deep copy of the environment's database (`Tool.invoke`, never `env.step`, so no action is
  recorded and the graded state cannot change).
- Runner: the arms of one task run back to back, so a budget stop leaves complete pairs.

**Settings.** As in Addendum 5 except: **both arms send `parallel_tool_calls: true`** (OpenAI's default, which is what
tau-bench's own runs use; tau-bench's `ToolCallingAgent` executes only the first call of a step). GPT-4.1 temperature 0;
GPT-5.4 mini `reasoning_effort: low`, no temperature. User simulator `gpt-4.1-2025-04-14`. `max_num_steps` 30.

| subset | manifest hash | tasks |
|---|---|---|
| **retail_test10r6** | `5d4fdabdbbd74701` | retail test indices 39, 42, 49, 53, 60, 79, 85, 89, 96, 112 = `sorted(random.Random("weave-round6-tau-retail-test").sample([i for i in range(115) if i not in retail_test7], 10))` |

Ten tasks, one repeat, per model and arm (40 trials). We aimed for at least 20 tasks per model; the round's 5.0M-token
cap does not allow it. Round-5 test trials averaged about 100k tokens (agent plus user simulator), the round-6 dev runs
spent 1.01M tokens, and ten tasks per model is what fits under the remaining 3.79M with room for long trials.

Dev (train split, not evidence; `bench/results/v9/*-dev*`): GPT-4.1 Weave passed 6 of 8 trials (both failures are
task 322, where the user's "card ending in 4108" is the id `credit_card_6044108`); native passed 2 of 2; GPT-5.4 mini
Weave passed 3 of 3. Agent calls on the two tasks both arms ran: 6 vs 8 and 15 vs 15. Most scripts still made one or
two tool calls, so we again expect a small effect.

Hypotheses on `retail_test10r6` (two-sided 95% CIs, paired by task; with n = 10 per model they are wide and we will say
so):

- **U1 (no harm).** Weave's pass count is at least native's minus 1, per model.
- **U2 (agent calls).** Mean agent model calls per task, Weave minus native, with a paired bootstrap CI; per model and
  pooled over both models (20 pairs).
- **U3 (tokens).** Total tokens (agent plus user simulator), geometric-mean ratio Weave/native with CI; per model and
  pooled.
- **U4 (wall time).** Wall seconds per task, Weave minus native, with CI; per model and pooled.
- **U5 (script errors, descriptive).** Script errors per Weave trial against round 5's `retail_test7` rate.

Runs (once each; provider errors count as failures). The in-flight-aware guard caps each run: GPT-4.1 at 2.0M
tokens, GPT-5.4 mini at 1.75M, reserving 150k per trial in flight, 4 workers. With the 1.01M dev spend the round
stays under 4.8M. Trials not started because of a cap are reported as not run, and unmatched pairs are dropped from
paired statistics and listed.

| harness | model | subset | arms |
|---|---|---|---|
| tau-bench | gpt-4.1-2025-04-14 (t = 0) | retail_test10r6 | weave, native |
| tau-bench | gpt-5.4-mini-2026-03-17 (effort low) | retail_test10r6 | weave, native |

## Addendum 7: round 7 (written and committed before any run on `retail_test12r7`)

Round 7 reruns tau-bench retail on more fresh test tasks with round 6's Weave arm plus the fix found after round 6:
return shapes are sampled once per environment and cached (`cached_return_shapes`), instead of in every trial.
In round 6 that sampling added about 2.2 s of host time to every Weave trial, inside the measured wall time. OpenAI
models only. Hard cap for the round: 4.0M tokens, no new trial once 3.8M is committed.

**What changed since Addendum 6.** Nothing in the agent, the prompt or the tool description. Code changes since
round 6's evidence:

- the shape cache;
- the static check now accepts names assigned in one branch when the other ends in `continue`, `break`, `return` or
  `raise`;
- the runner's budget guard now waits for running trials rather than skipping a trial that does not fit;
- the analysis adds list-price cost and host time.

We considered cheaper first turns and direct calls for single reads, aimed at GPT-5.4 mini's wall-time loss, and made
neither change. The train-split dev runs of round 6 (`bench/results/v9/*-dev*`) show GPT-5.4 mini's Weave model calls
at 1.8–1.9 s each, and round 6's measured host time explains most of its loss. With 0.4M tokens for dev, any new change
could only be smoke-tested, so we put the whole budget into evidence. No dev runs this round: 0 tokens.

**Settings.** As in Addendum 6:

- both arms send `parallel_tool_calls: true`; tau-bench's `ToolCallingAgent` executes only the first call of a step;
- GPT-4.1 runs at temperature 0;
- GPT-5.4 mini runs at `reasoning_effort: low`, with no temperature;
- the user simulator is `gpt-4.1-2025-04-14`, and `max_num_steps` is 30;
- tau-bench is at commit `59a200c`, unmodified.

| subset | manifest hash | tasks |
|---|---|---|
| **retail_test12r7** | `7aba309eb345598b` | retail test indices 0, 2, 7, 9, 50, 54, 70, 72, 73, 83, 94, 101 = `sorted(random.Random("weave-round7-tau-retail-test").sample([i for i in range(115) if i not in retail_test7 + retail_test10r6], 12))`; disjoint from rounds 5 and 6 |

**Size.** Twelve tasks, one repeat, per model and arm: 48 trials, the same tasks for both models. We aimed for 20 or
more tasks per model, but the cap does not allow it. Round 6's test trials averaged 70.5k tokens with GPT-4.1 and
73.8k with GPT-5.4 mini (agent plus user simulator), so a task pair costs about 141k and 148k tokens. Twelve tasks per
model is about 3.47M tokens; 13 would be about 3.76M, too close to 3.8M; 20 would be about 5.8M.

**Hypotheses** (two-sided 95% paired cluster-bootstrap CIs over tasks, per model and pooled over 24 (model, task)
pairs; with 12 pairs per model they are wide, and we will say so):

- **V1 (no harm).** Weave's pass count is at least native's minus 1, per model. Exact McNemar p reported.
- **V2 (agent calls).** Mean agent model calls per task, Weave minus native. Replication target: round 6 found
  −4.4 for GPT-4.1 and −2.5 pooled. We predict a CI below 0 for GPT-4.1 and pooled.
- **V3 (wall time).** Wall seconds per task, Weave minus native. Round 6 found a GPT-5.4 mini loss of +4.8 s
  [+0.5, +9.5]. We predict that with the cache its CI includes 0 or lies below it. A CI above 0 for either model is a
  loss, and we will report it as one.
- **V4 (tokens).** Geometric-mean ratio of total tokens, Weave/native.
- **V5 (list-price cost).** Geometric-mean ratio of per-task dollars, Weave/native, using OpenAI list prices per 1M
  tokens: GPT-4.1 $2.00 uncached input, $0.50 cached input, $8.00 output; GPT-5.4 mini $0.75, $0.075, $4.50. Reasoning
  tokens count as output, and the user simulator is priced as GPT-4.1. The input, cached and output token split is
  reported per arm, with total dollars per run.
- **V6 (host time, checks the fix).** Wall time minus metered model time (agent plus user simulator), Weave minus
  native. Round 6: +2.7 s (GPT-4.1) and +3.4 s (mini). We predict under 1 s for both.
- **V7 (script errors, descriptive).** Script errors per Weave trial, against round 6.

Exploratory, not part of the hypotheses: rounds 6 and 7 pooled (22 tasks per model). Round 6's Weave wall time
includes the uncached sampling, so this pooling is labeled as exploratory wherever it appears.

**Runs** (each once; provider errors count as failures):

- The two runs execute concurrently, with 4 workers each.
- The in-flight-aware guard caps GPT-4.1 at 1.85M tokens and GPT-5.4 mini at 1.95M (3.8M together). It reserves 160k
  per trial in flight; the largest round-6 test trial used 152k.
- A trial that does not fit waits for running trials to finish. It is skipped only when nothing is running.
- Skipped trials are listed in `run.json` as not run. Unmatched pairs are dropped from paired statistics and listed.

| harness | model | subset | arms |
|---|---|---|---|
| tau-bench | gpt-4.1-2025-04-14 (t = 0) | retail_test12r7 | weave, native |
| tau-bench | gpt-5.4-mini-2026-03-17 (effort low) | retail_test12r7 | weave, native |

## Addendum 8: round 8, BFCL multi-turn (written and committed before any evidence trial on `mtb40`)

Round 8 adds a second external benchmark, one built around chained, multi-step tool use:
[BFCL](https://github.com/ShishirPatil/gorilla/tree/main/berkeley-function-call-leaderboard)'s multi-turn category.
BFCL is the Berkeley Function Calling Leaderboard, Apache-2.0, installed as `bfcl-eval==2026.3.23` from PyPI. Its
`multi_turn_base` category has 200 tasks. Each task is several user turns against stateful backends (a file system,
Twitter, a trading bot, travel booking, vehicle control, ...). A turn usually needs several dependent calls:
`cd`, `mkdir`, `mv`, `grep`, and so on.

**Harness** (`bench/bfcl/`). The tasks, function docs, backends and grader are BFCL's, unmodified.

- **Native arm.** BFCL's own `OpenAICompletionsHandler.inference_multi_turn_FC` (FC mode). It executes every tool
  call of a step and runs at most 20 steps per turn.
- **Weave arm.** The same loop with one `run_weave` tool. Every function the script calls runs through BFCL's
  `execute_multi_turn_func_call`. Each call is recorded in the native handler's format, so BFCL's
  `_evaluate_single_multi_turn_entry` grades both arms identically: it re-executes the recorded calls and checks
  state and responses against the ground truth.
- **Calls are kept in order.** Every backend function is declared `reversible` (a write), so Weave keeps calls in
  program order. On these stateful backends the gain can only come from fewer model round trips, not concurrency.
- **Offline check.** `bench/bfcl/test_offline.py` has an oracle that replays the ground truth; it passes in both arms,
  and a model that makes no calls fails in both.

**Settings, identical in both arms.**

- Chat Completions with OpenAI's default `parallel_tool_calls` (on), `store: false`, and the handler's rate-limit
  retries.
- GPT-4.1 (`gpt-4.1-2025-04-14`) runs at BFCL's default temperature, 0.001.
- GPT-5.4 mini (`gpt-5.4-mini-2026-03-17`) runs with `reasoning_effort: "none"` and no temperature. Chat Completions
  rejects function tools with any other effort for this model, as dev run B found.
- One repeat.

**Dev** (`dev6` = ids 4, 20, 25, 73, 135, 198, seeded; never evidence; `bench/results/v11/*-dev*`; $0.6499 and
831,878 tokens):

- GPT-4.1: Weave 1/6, native 2/6; 38 against 49 agent calls.
- GPT-5.4 mini: Weave 2/6, native 2/6; 38 against 49 agent calls.
- Weave cost more per trial: $0.032 against $0.025 (GPT-4.1) and $0.0069 against $0.0056 (GPT-5.4 mini). It used
  about 1k more input tokens per call, with a lower cached share.

One change came from dev. The Weave arm runs without `persist_variables`: in task 135 a persisted `order` let GPT-5.4
mini answer the last turn without the lookup BFCL's grader expects in every turn. After the change, Weave passed 3/6
(GPT-4.1) and 1/6 (GPT-5.4 mini) on dev, run without the native arm.

**Subset.**

| subset | manifest hash | tasks |
|---|---|---|
| **mtb40** | `3ab8f2865c012424` | `multi_turn_base` ids 13, 14, 17, 21, 28, 30, 36, 41, 43, 45, 52, 57, 61, 70, 80, 87, 88, 93, 95, 96, 102, 103, 108, 111, 116, 120, 126, 136, 140, 161, 163, 177, 181, 182, 187, 188, 192, 193, 194, 199 = `sorted(random.Random("weave-round8-bfcl-evidence").sample([i for i in range(200) if i not in dev6], 40))` |

**Size.** The same 40 tasks for both models: 160 trials. Dev trials cost about $0.03 (GPT-4.1) and $0.006
(GPT-5.4 mini) at list prices, so 40 tasks is about $2.3 and $0.5. With dev spend, that stays under the round's
$4.70 stop.

**Hypotheses** (paired by task; 95% cluster-bootstrap CIs; per model and pooled over 80 (model, task) pairs):

- **B1 (success).** Pass counts, exact McNemar p, and the paired difference in pass rate with a CI. Weave does "no
  harm" if that CI's lower bound is above −0.10.
- **B2 (agent model calls per task).** Weave minus native. We predict a CI below 0 for each model, because a turn's
  chain fits in one script.
- **B3 (wall time per task).** Weave minus native. No direction predicted: the backends are in-memory, so fewer calls
  are the only source of a gain.
- **B4 (tokens).** Geometric-mean ratio of tokens (input plus output), Weave/native. No direction predicted: dev
  showed more input per call for Weave.
- **B5 (list-price cost per task).** Geometric-mean ratio, Weave/native. Prices per 1M tokens: GPT-4.1 $2.00 input,
  $0.50 cached, $8.00 output; GPT-5.4 mini $0.75, $0.075, $4.50. Reasoning tokens count as output. Total dollars
  reported per run.
- **B6 (descriptive).** BFCL error types per arm, Weave scripts and script errors, and tool calls per script.

**Runs** (each once; provider errors count as failures):

- The two runs execute concurrently, with 4 workers each.
- The in-flight guard caps GPT-4.1 at $2.80 and GPT-5.4 mini at $0.85, reserving $0.10 and $0.03 per trial in
  flight. A trial that does not fit waits; it is skipped only when nothing is running. Skipped trials are listed and
  dropped from paired statistics.
- Round total: at most $0.65 dev + $3.65 evidence = $4.30, under the $4.70 stop.

| harness | model | subset | arms |
|---|---|---|---|
| BFCL v4 multi_turn_base (`bfcl-eval==2026.3.23`) | gpt-4.1-2025-04-14 (temperature 0.001) | mtb40 | weave, native |
| BFCL v4 multi_turn_base (`bfcl-eval==2026.3.23`) | gpt-5.4-mini-2026-03-17 (effort none) | mtb40 | weave, native |

## Addendum 9: round 9, a cheaper and checked Weave arm (written and committed before any evidence trial on `mtb60r9` or `confirm9`)

**Why.** Round 8 showed Weave costing 1.36× native on BFCL `multi_turn_base`. The diagnosis
(`docs/research/round9-notes.md`, `bench/bfcl/diagnose.py`) found a description billed ~850 tokens larger than the
native schemas on every request, repeated doc text, a GPT-4.1 cache-hit gap (57 % vs 72 %), result types that did
not match the backends, and a harness defect (Python-literal results passed to scripts as strings).

**Techniques** (ablated on train tasks only; rows in `bench/results/v12/*-dev-*`): compact description with shared
doc sentences factored out (`WeaveTool(compact=True)`), return shapes learned from train-split ground-truth
executions (`bench/bfcl/learn_shapes.py`, `shapes.json`; evidence tasks excluded), Python-literal result parsing, a
call trace in the script output, checked arguments (refused once on a simple type mismatch), `reply()` with a guard,
hybrid routing, and `prompt_cache_key` per trial.

**Dev results, 40 train tasks** (`dev40r9` = 20 tasks of mtb40 + 20 other train tasks; one run each; native with the
cache key):

| config | GPT-4.1 pass / $ per task | GPT-5.4 mini pass / $ per task |
|---|---|---|
| native | 25/40, $0.0257 | 18/40, $0.0064 |
| **W3** compact, learned, trace, typecheck | **28/40, $0.0215** | **22/40, $0.0054** |
| W4 = W3 + reply, guard | 18/40, $0.0168 | 23/40, $0.0042 |
| W5 hybrid, learned, trace, typecheck | — | 19/40, $0.0064 |

`reply()` saves a model call per turn but made GPT-4.1 end turns before doing the requested change (18/40), so it is
not used. Hybrid matched native in cost and success. **W3 is the evidence configuration for both models.**

**BFCL evidence subset.**

| subset | manifest hash | tasks |
|---|---|---|
| **mtb60r9** | `dd3384a5132d2183` | `multi_turn_base` ids 0, 1, 3, 11, 12, 22, 23, 31, 32, 33, 40, 50, 55, 56, 58, 60, 62, 64, 66, 69, 72, 76, 78, 79, 83, 84, 86, 90, 94, 97, 99, 106, 113, 115, 117, 123, 125, 128, 130, 141, 142, 147, 150, 152, 157, 160, 166, 167, 169, 170, 171, 174, 175, 179, 180, 183, 184, 185, 189, 195 = `sorted(random.Random("weave-round9-bfcl-evidence").sample(R9_POOL, 60))`, R9_POOL = ids not in dev6 or mtb40; never used for tuning or shape learning |

**Settings.** `bfcl-eval==2026.3.23`, tasks and grader unchanged; native arm = BFCL's `OpenAICompletionsHandler`
loop. Both arms: `prompt_cache_key=<arm>-<task>`, OpenAI default `parallel_tool_calls`, `store: false`.
GPT-4.1 at temperature 0.001; GPT-5.4 mini with `reasoning_effort: none`. Weave config
`compact,learned,trace,typecheck`. One repeat, 4 workers, runs concurrent.

**Hypotheses** (paired by task; 95 % cluster-bootstrap CIs; per model and pooled over 120 pairs):

- **C1 (success).** Pass counts, exact McNemar p, paired pass-rate difference. No harm if the CI lower bound is above
  −0.10. We predict a pooled point estimate ≥ 0 (dev: +7 tasks of 80); significance is not expected at this size.
- **C2 (cost).** Geometric-mean ratio of list-price dollars per task, Weave/native. We predict a pooled CI entirely
  below 1.
- **C3 (tokens)** ratio, **C4 (model calls)** difference, **C5 (wall time)** difference: reported, predicted ≤ 1,
  < 0 and ≈ 0.
- **C6 (descriptive).** Script errors, type-check refusals, cached share, error types per arm.

**Fan-out reconfirm, `confirm9`** (hash `a7ef5eb5aac5cd26`; the `confirm4` recipe with seeds + 500; real SQLite, git,
files, HTTP and PyPI; latency 0): modes `weave_compact` (`WeaveTool(compact=True)`), `weave` (round-4 listing) and
`native`, 2 repeats, GPT-4.1 (temperature 0) and GPT-5.4 mini (effort low).
- **F1.** `weave_compact` vs native: pass ≥, wall time and tokens lower (as in rounds 2–5).
- **F2.** `weave_compact` vs `weave`: fewer tokens, pass within noise.

**Caps** (list prices; in-flight guard): BFCL GPT-4.1 $3.40 (reserve $0.08 per trial), GPT-5.4 mini $0.95 (reserve
$0.03); `confirm9` 600k tokens per model. Round 9 dev spend before evidence: $6.07 (8.74M tokens). Round total at most
$12.1, under the $14 stop. Skipped trials are listed and their pairs dropped.

**Release rule** (from the owner's brief): release v1.1.0 and make the repository public only if BFCL shows Weave ≥
native on success and ≤ native on cost (point estimates, with no-harm met), or clear wins elsewhere with no hidden
losses.

## Addendum 10 (2026-11-16): the write gate on BFCL categories never run before

Written before any evidence trial of this round. Round 9 found GPT-5.4 mini losing on BFCL when a script batched
writes on a wrong assumption. Round 10 adds an opt-in **write gate** (`WeaveTool(write_gate=...)`): a script stops
before a write whose arguments were picked from a list the model has not seen (`choice`), or that follows a call that
failed (all modes), and the model gets the results so far. Read/write classes for BFCL come from differential
execution of train ground truth (`bench/bfcl/learn_effects.py`: 37 reads, 41 writes).

**Dev (train scenario ids only; GPT-5.4 mini, effort none, one run each; not evidence).** `dev40r9`, base category:
native 18/40 ($0.0064/task, round 9 rows), W3 22/40 ($0.0054), W3 + `flow` gate 20/40 ($0.0057; 13 pauses), **W3 +
`choice` gate 24/40 ($0.0048; 10 pauses)**. `devMP20r10` (miss_param): native 6/20, W3 5/20, W3 + `flow` 5/20.
`devMF10r10` (miss_func): native 3/10, W3 + `flow` 3/10. The `flow` gate paused on harmless single-value lookups and
once the model ended the turn after a pause; the pause text now says the request is not finished. Three runs were
accidentally run on `multi_turn_base` (a `--category` bug, fixed in `818fe94`) and are kept in
`bench/results/v13/mislabeled/`. **Chosen: W3 + `choice` gate** (`--weave-config compact,learned,trace,typecheck,gatechoice`).

**Evidence.** BFCL v4 categories `multi_turn_miss_param` and `multi_turn_miss_func` (`bfcl-eval==2026.3.23`, tasks,
backends and grader unchanged; never run in any earlier round). Scenario ids: the 60 of `R9_EVIDENCE`, which were never
train (no shapes or effects were learned from them) and never used for tuning.
- `mp60r10`: miss_param, 60 ids, hash `881d1f18b6aa5ce8`; `mf60r10`: miss_func, 60 ids, hash `1c0e404e20dee1d4` —
  GPT-5.4 mini (`reasoning_effort: none`).
- `mp20r10`: miss_param, a seeded 20 of those ids, hash `533996f83ad87686` — GPT-4.1 (temperature 0.001).
- Arms: native (BFCL's handler) and Weave (config above); `prompt_cache_key` in both; one repeat; 4 workers.

**Hypotheses** (paired by task; 95 % cluster-bootstrap CIs; exact McNemar):
- **D1 (success).** Pooled over the 120 mini pairs: Weave pass rate − native ≥ 0 (point estimate), no harm if the CI
  lower bound is above −0.10. Reported per category and for GPT-4.1.
- **D2 (cost).** Geometric-mean $ ratio Weave/native below 1 (pooled mini CI entirely below 1).
- **D3** tokens, **D4** model calls, **D5** wall time; **D6** descriptive: gate pauses, error types.

**Caps** (list prices, in-flight guard): mp60r10 $0.85, mf60r10 $0.95 (reserve $0.03 per trial), mp20r10 $1.05
(reserve $0.10). Skipped trials are listed and their pairs dropped. Round 10 spend before evidence: $1.18.

**Release rule** unchanged from Addendum 9; this round does not release or change visibility (owner's instruction).

## Addendum 11 (2026-11-17): Azure endpoint; completing round 10; a fresh confirmation

Written before any trial of this addendum. The OpenAI account ran out of credits during round 10. The rest of the
work runs on Azure AI Foundry deployments (Global Standard) of the same model versions, through the OpenAI-compatible
`/openai/v1` Chat Completions endpoint. Both arms use the same endpoint, client and settings (`--base-url`,
`--deployment`; `bench/bfcl/run.py`). A probe on 2026-11-17 returned `model = gpt-4.1-2025-04-14` (fingerprint
`fp_d362bfbada`) and `gpt-5.4-mini-2026-03-17`. It also accepted `prompt_cache_key`, `parallel_tool_calls`,
`reasoning_effort: none`, `store: false` and tools, and reported cached prompt tokens (prefix caching worked on the
second identical request). No parameter is dropped. Each row records the endpoint and the served model version. Costs
are computed at OpenAI list prices, as before; Azure's billing may differ.

**Part A: completing round 10 (a disclosed deviation).** The incomplete round-10 pairs are rerun on Azure, both arms
for each pair; orphan single-arm OpenAI rows stay unpaired. The pairs are: `mp60r10c` (mini, miss_param ids 31, 58,
72, 113, 185; hash `9522823274846d85`), `mf60r10c` (mini, miss_func ids 23, 31, 58, 72, 86, 113, 130, 141, 147, 185;
hash `bbdb204d53aba5fb`), and `mp20r10c` (GPT-4.1, miss_param ids 32, 117, 157, 175; hash `cdf61fa18942870a`; these 4
were budget skips). Round 10 is reported both as pre-registered (completed pairs only) and with these pairs added,
the latter labelled as mixed-endpoint.

**Part B: confirmation.** Same configuration as Addendum 10 (Weave = `compact,learned,trace,typecheck,gatechoice`;
native = BFCL's handler; `prompt_cache_key` in both arms; one repeat; GPT-5.4 mini at effort `none`, GPT-4.1 at
temperature 0.001). Nothing is tuned on these tasks. Task ids are the 63 scenario ids never run live in any category
or round (`R11_FRESH` in `bench/bfcl/subsets.py`). They are train ids, so round 9's learned return shapes and round
10's learned read/write classes include executions of their ground truth (API-level metadata only).
- `mtbfresh63r11` (multi_turn_base, hash `841b3aade6d644fc`), `mpfresh63r11` (miss_param, `71659cd456b87c86`),
  `mffresh63r11` (miss_func, `297acaa475fe6e32`). GPT-5.4 mini runs all three. GPT-4.1 runs all three if the budget
  allows, in the order base, miss_param, miss_func.

**Hypotheses** (paired by task; 95 % cluster-bootstrap CIs; exact McNemar; pooled over categories per model):
- **E1 (success).** Mini pooled (189 pairs): Weave − native ≥ 0 (point), no harm if the CI lower bound > −0.10.
  The same is reported for GPT-4.1, plus each category.
- **E2 (cost).** The pooled geometric-mean $ ratio Weave/native is ≤ 1 (point).
- **E3–E5:** tokens, model calls, wall time. **E6:** gate pauses, error types.

**Caps** (list prices, in-flight guard; whole job ≤ $25): Part A mini $0.40 (both), GPT-4.1 $0.40. Part B mini base
$1.30, miss_param $1.60, miss_func $1.60 (reserve $0.03); GPT-4.1 base $4.50, miss_param $4.80, miss_func $4.80
(reserve $0.10). Spent on this job before evidence: $0.07 (probe and a 2-task smoke run on train ids 5 and 6).

**Release rule** (owner's brief): Weave ≥ native on success and ≤ native on cost on BFCL, or clear wins elsewhere with
no hidden losses. This addendum only reports whether it is met; the owner decides.

## Addendum 15 (2026-11-27): round 15, error convention and recovery wording (written and committed before any evidence trial on `lc100r15`, `mtb60r9` or `live-v2`)

Full plan: `docs/research/round15-preregistration.md` (pushed 2026-11-27 6:20 PM ET, commit `6b0fb8f`). This addendum
fixes the details the runners need.

- **Arms.** native = BFCL's handler, unchanged. Weave = `compact,learned,trace,typecheck,gatechoice,errkey,recover`:
  round 11's config plus `error_key="error"` on every BFCL function (merged in PR #65, `facc052`, after its tests
  passed) and the two pre-registered sentences appended to the `run_weave` description (`weave_arm.RECOVER`).
- **Endpoint.** Azure AI Foundry `atharvraotole-4504-resource.services.ai.azure.com`, `/openai/v1`, deployment
  `gpt-5.4-mini`; a smoke run served `gpt-5.4-mini-2026-03-17`. Effort `none`, `prompt_cache_key` in both arms.
  Round 9 ran on the OpenAI API, so `mtb60r9` here is a different provider from round 9.
- **Manifests.** `lc100r15` (multi_turn_long_context, the 100 ids in the plan): hash `c67ab8708811a550`.
  `mtb60r9` (multi_turn_base): hash `dd3384a5132d2183` (unchanged from Addendum 9). `live-v2` (24 tasks,
  `bench/protocol_v3/live_tasks_v2.py`): manifest hash `72c7174a545b3621796326804b72685eb2ab9ab3e7b6f6bb60ee4cbe0a097bae`.
- **Disclosure: how held out `lc100r15` is.** No round has run the long-context category, and the round 15 changes
  came only from the round 9 evidence traces, which `lc100r15` excludes. But all 100 ids are base-category train
  scenarios: the learned return shapes and read/write classes include their base ground truth (API-level metadata),
  and 53 of them were run live in other categories in earlier rounds, where round 11's config was chosen. The 47 never
  run live (`R15_LC_NEVER_RUN`) are reported separately, descriptively; they are not a new endpoint.
- **Invocations, in order** (`bench/bfcl/run.py`, workers 4, reserve $0.05 per trial in flight):
  (A) `lc100r15` repeat 0, seed 2026, cap $4.00; (B) `mtb60r9` repeat 0, seed 2026, cap $1.50; (C) `lc100r15`
  repeat 1, seed 2027, cap $4.00; (D) `mtb60r9` repeat 1, seed 2027, cap $1.50. Each invocation interleaves the arms
  per task in a seeded random order. The runner stops starting trials when provider or harness errors exceed 10 % of
  the last 20 trials. Then `live-v2` (`bench/protocol_v3/live_cc.py`): 3 repeats (144 trials), seed 2026, cap
  $1.00. Caps total $12.00; with the $0.03 spent on smoke runs the round cannot pass $18.
- **Live A/B arms.** The same model, endpoint and Weave options, over the protocol-v3 fixture (fs/git/memory behind
  the real `weave-mcp`). Its tools raise opaque errors and never return error values, so `error_key` has no effect
  there. Unsafe writes: write attempts outside the task's expected writes, plus write attempts after a failed call.
- **Analysis.** `bench/bfcl/report_r15.py`, committed with this addendum, exactly as in the plan: complete pairs
  only; per-task means over repeats; paired bootstrap 95 % CI (10,000 resamples of tasks, seed 2026); exact
  McNemar on repeat 0. "State-changing calls after a failed call" are counted per model step for both arms by
  executing the recorded calls again on fresh BFCL backends (no model calls). The live set uses the same bootstrap.
- **Smoke runs before evidence ($0.0289):** one long-context task outside both sets (id 10, `smokelc15`, both arms
  failed) for cost, and a harness check of `live_cc.py` on 2 `live-v2` tasks (`copy_missing`, `publish_if_match`;
  both arms passed both). Nothing was changed after them.
- **Headline rule** unchanged: the primary difference is at least +0.10 and its CI excludes 0.
