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
