# Round 15 results

Plan: [`round15-preregistration.md`](round15-preregistration.md), committed and pushed before the first evidence
trial (branch `research/round15-prereg`, final registration commit `e6baea7`). Analysis code: `bench/bfcl/report_r15.py`
(pre-registered) and `bench/bfcl/r15_extras.py` (descriptive extras, written after the results, not endpoints).
Raw rows and traces: `bench/results/v15/`. Merged reports: `bench/results/v15/analysis/`.

## Short version

- **Primary (held-out long-context set, 100 tasks, 2 repeats):** Weave passed 0.515, native function calling 0.415.
  The difference is **+0.100, 95% CI +0.005 to +0.200**.
- **The headline rule is met on the letter, barely.** The rule was "difference at least +0.10 and CI excludes 0".
  The difference is exactly +0.100 (103 vs 83 passes out of 200 trial pairs) and the CI lower bound is +0.005.
  The exact McNemar test on repeat 0 is **p = 0.058**, and on repeat 1 it is **p = 0.23**. Neither is significant on
  its own. We read this as a promising, modest result that needs a bigger, fresh confirmation, not as a proven win.
- **The round 9 loss stands:** GPT-5.4 mini on BFCL base, 18/60 vs 27/60 (OpenAI API, round 9). Round 15 reran those
  same 60 tasks on Azure as a contaminated secondary set and got 26/60 vs 26/60 on repeat 0, but that set was used to
  design the fixes, so it is not evidence that the loss is gone.
- **Live A/B (24 tasks, 3 repeats): a tie**, 69/72 vs 69/72. The tasks were too easy to separate the arms.
- **Spend:** $8.76 of the $25 cap, including $0.03 of smoke runs. No billing outside the cap.

## Setup (as pre-registered)

- Model `gpt-5.4-mini-2026-03-17`, reasoning effort none, through Azure AI Foundry. Round 9 used the OpenAI API, so
  comparisons with round 9 are across providers.
- Arms: native BFCL function calling, and Weave with round 11's config plus `error_key="error"` (merged in PR #65) and
  the two added sentences in the `run_weave` description.
- Primary set `lc100r15`: BFCL `multi_turn_long_context`, 100 ids, 2 repeats per arm. Secondary `mtb60r9`: the same
  60 base tasks as round 9, 2 repeats, contaminated (the fixes came from reading their failures).
- Per-task pass rate is the mean over the 2 repeats. CI is a paired bootstrap over tasks (10,000 resamples, seed 2026).
  McNemar is exact, two-sided, on repeat 0.

## Primary: `lc100r15`

| | Weave | Native | Difference | 95% CI |
|---|---|---|---|---|
| Pass rate (200 trial pairs) | 0.515 (103) | 0.415 (83) | **+0.100** | +0.005 to +0.200 |
| Model calls per task | 8.58 | 9.36 | -0.78 | -1.12 to -0.45 |
| Tokens per task | 55,236 | 79,603 | -24,367 | -31,505 to -17,789 |
| List-price USD per task | 0.0148 | 0.0193 | -0.0045 | -0.0093 to -0.0016 |
| Wall seconds per task (see caveat) | 59.4 | 87.2 | -27.8 | -50.7 to -6.2 |
| State-changing calls after a failed call | 13 | 13 | 0 | -0.035 to +0.040 (per task) |

Headline rule: difference at least +0.10 and CI excludes 0. **Met, with no margin on the difference and a lower bound
of +0.005.**

Repeat by repeat (descriptive, not pre-registered):

| Repeat | Weave | Native | Weave only | Native only | McNemar p | Difference (95% CI) |
|---|---|---|---|---|---|---|
| 0 | 51/100 | 39/100 | 23 | 11 | 0.058 | +0.12 (+0.01 to +0.23) |
| 1 | 52/100 | 44/100 | 21 | 13 | 0.23 | +0.08 (-0.03 to +0.20) |

Other descriptive cuts (post hoc, so read them lightly):

- Per task, Weave did better on 32 tasks and native on 19 (sign test p = 0.09).
- Failure types, over 200 trials per arm. Weave: 103 pass, 49 state mismatch, 27 response mismatch, 21 empty turn.
  Native: 83 pass, 80 state mismatch, 26 response mismatch, 11 empty turn. So the gain comes from fewer wrong final
  states, while Weave has more "empty turn" failures (the model gave no tool call in a turn). That is a Weave-side
  weakness worth fixing.
- Dropping every task-repeat where either arm ended in an empty turn (91 tasks left): +0.121 (+0.022 to +0.220).
- The 47 ids that were never run live in any earlier round (reported separately as pre-registered): 56/94 vs 40/94,
  difference +0.170 (+0.021 to +0.330). The other 53, which earlier rounds touched: 47/106 vs 43/106, difference
  +0.038 (-0.075 to +0.151). The gain is concentrated in the ids that earlier rounds never saw, which is a little
  encouraging, but 47 tasks is small and the split was not a pre-registered endpoint.

## Secondary: `mtb60r9` (contaminated, reported separately, never pooled with the primary)

| | Weave | Native | Difference | 95% CI |
|---|---|---|---|---|
| Pass rate (120 trial pairs) | 0.450 (54) | 0.408 (49) | +0.042 | -0.058 to +0.142 |
| Repeat 0 only | 26/60 | 26/60 | 0 | McNemar p = 1.0 (9 vs 9) |
| Model calls per task | 8.45 | 9.43 | -0.98 | -1.44 to -0.54 |
| Tokens per task | 24,063 | 29,202 | -5,139 | -7,092 to -3,421 |
| List-price USD per task | 0.00707 | 0.00710 | -0.00003 | -0.00045 to +0.00037 |
| Wall seconds per task | 17.6 | 24.0 | -6.4 | -15.4 to +1.7 |
| State-changing calls after a failed call | 8 | 10 | -0.017 | -0.075 to +0.042 (per task) |

This is not significant either way. It does not erase the round 9 loss (18/60 vs 27/60 on the OpenAI API). The
native arm scored almost the same here as in round 9 (26/60 vs 27/60), while Weave went from 18/60 to 26/60 after the
fixes, but those fixes were designed from these very tasks.

## Live A/B: `live-v2` (24 tasks, 3 repeats, local MCP fixtures)

| | Weave | Native | Difference | 95% CI |
|---|---|---|---|---|
| Pass rate (72 trial pairs) | 0.958 (69) | 0.958 (69) | 0.000 | 0.000 to 0.000 |
| Model calls per task | 2.26 | 2.96 | -0.69 | -1.06 to -0.35 |
| Tokens per task | 1,593 | 1,198 | +395 | +225 to +564 |
| List-price USD per task | 0.00160 | 0.00116 | +0.00044 | +0.00027 to +0.00061 |
| Wall seconds per task | 2.27 | 2.69 | -0.42 | -0.75 to -0.10 |
| Unsafe write attempts | 9 | 9 | 0 | 0 to 0 |

By family (Weave and native passed the same counts): fan-out 18/18, dependent lookups 18/18, error handling 18/18,
writes with a precondition 15/18. Each arm failed 3 of the 18 precondition trials (I did not check whether they were the same trials). Provider errors: 0.
On these small tasks Weave used fewer model calls and a bit less time, but **more tokens and slightly higher cost**,
because the tool descriptions and script format cost more than they save on 2 to 3 step tasks. No quality gap.

## Deviations and caveats

From the pre-registration (deviations 1 to 6 were written down before the evidence trials they affect):

1. **Run A was split.** Repeat 0 of `lc100r15` ran in two invocations (A with 89 complete pairs, then A2 with the other
   11 tasks), because run A stalled on hung Azure requests. The analysis merges them as repeat 0. No result of run A
   was looked at before that decision.
2. **Timeout changed mid-round.** Run A used the client's default 600 s timeout, later runs 120 s. Transport only.
3. **The primary set is less "held out" than the name says.** All 100 ids are base-category train scenarios, and 53 were
   run live before. See the 47 / 53 split above.
4. **Live A/B used a new harness** (`live_cc.py`), not protocol-v3's runner, and `error_key` does not apply there.
5. **Provider rate limits (HTTP 429).** Azure returned many 429s, mostly in runs A, A2 and C (about 149, 15 and 174
   log lines; 1 and 2 in B and D; none in the live run). The client retried with backoff. Every one of the 640 BFCL
   trials and 144 live trials finished with a normal graded result, no row carries a provider error, and every pair is
   complete, so none were dropped. What I can not do is say how the 429s split between the arms: the logs do not tag
   them by arm. Native sends about 44% more tokens per task, so it probably hit the limit a bit more often, and the
   waiting time lands in wall seconds. **Treat the wall-time rows as unreliable, probably tilted toward Weave.** The
   "Failed to decode the model response" lines in the logs are BFCL's normal message when the model answers with text
   instead of a call; they also show up in runs B and D, which had almost no 429s.
6. **Empty turns are uneven** (21 for Weave vs 11 for native on the primary set). These are model behaviour, graded as
   failures by BFCL, and they are counted against Weave in every number above.
7. **Single model, single provider, one benchmark category.** Nothing here says what happens with other models. The
   round 9 evidence is that a different model (GPT-5.4 mini on the OpenAI API) lost.

## Verdict

On a category nobody had run before, with the pre-registered plan (run A was stopped on provider stalls and completed as described below), Weave passed about 10 points
more tasks than native function calling with the same model, and did it with about 31% fewer tokens and 23% less
money per task. By the rule we wrote before running, that counts as a headline, but only just: the CI starts at
+0.005, one of the two repeats is not significant by itself, and the 53 ids earlier rounds had touched show
almost no gain. On the contaminated round 9 tasks the two arms now tie, and the round 9 loss of 18/60 vs 27/60 stays on
the record. On small live tasks they tie on quality and Weave costs a little more. So the honest summary is: Weave is
probably ahead on long multi-step BFCL tasks with this model, by a modest amount that needs a larger fresh run to
confirm, and it is not a clean win everywhere.
