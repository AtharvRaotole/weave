# Round 16 pre-registration: does round 15's long-context gain repeat on fresh tasks?

Written 2026-11-27 (evening), before any round 16 model call. It has to be committed and pushed before the first
call; the commit time is the registration time. Nothing below changes after that, except a deviations section
appended with dates.

## Why

Round 15 ([results](round15-results.md)) met its headline rule by the smallest possible margin: primary difference
+0.100 (95% CI +0.005 to +0.200) on 100 long-context tasks, McNemar p = 0.058 on repeat 0 and 0.23 on repeat 1. The
round 9 loss (GPT-5.4 mini on BFCL base, 18/60 vs 27/60) stays on the record. This round asks, on tasks that round 15
did not use, whether the same Weave config still beats native function calling by a clear margin.

## Arms and model (unchanged from round 15)

- **native**: BFCL's own `OpenAICompletionsHandler` function calling.
- **weave**: the round 15 config exactly: `--weave-config compact,learned,trace,typecheck,gatechoice,errkey,recover`
  (round 11's config, `error_key="error"` from PR #65, and the two `run_weave` sentences). No code of the Weave arm
  changes between round 15 and round 16.
- Model `gpt-5.4-mini-2026-03-17`, effort none, Azure AI Foundry deployment `gpt-5.4-mini` (the same endpoint as
  round 15), `prompt_cache_key` in both arms, 120 s client timeout with 2 retries.

## Task sets (fresh = (category, id) pairs no earlier round ran live)

Every scenario id has been run live in some BFCL category before, so freshness is per category. I searched every
`rows.jsonl` under `bench/results/` (any model, any round) for each category. Sets are frozen in `bench/bfcl/subsets.py`
(`lc39r16`, `mp57r16`, `mf67r16`):

| Set | Category | Tasks | Rule | Manifest hash | sha256(ids)[:16] |
|---|---|---|---|---|---|
| `lc39r16` | multi_turn_long_context | 39 | ids not in `lc100r15`, not in the round 9 evidence ids (read for the round 15 diagnosis), not the round 15 smoke id 10 | `7d46d0df070170a4` | `604a632f11257f6b` |
| `mp57r16` | multi_turn_miss_param | 57 | ids with no row in any results file | `f03bda7d42ad255e` | `dcd7f58e27069621` |
| `mf67r16` | multi_turn_miss_func | 67 | ids with no row in any results file | `8aff726f5bb3e2b2` | `5879caec7993ce72` |

Total 163 tasks, 2 repeats, 326 trial pairs.

**Disclosure on how fresh these are.** All ids are base-category train scenarios: round 9's learned return shapes and
round 10's read/write classes include executions of their ground truth (API metadata, no model runs). Many ids were
run live in the *base* category in earlier rounds, and 107 of the 163 are scenario ids that were used to tune the Weave
arm in base (`DEV`, `R9_DEV`, `R9_DEV_B`, `R10_DEV_MP`, `R10_DEV_MF`). The miss_param / miss_func sets are more
exposed to that than the long-context set: 33 of the 57 and 36 of the 67 are the round 8 base evidence ids. Round 10
and 11 ran Weave on miss_param / miss_func with the write gate and did not show a significant win there. These
are reported as descriptive splits (below), never as the headline.

## Endpoints

- **Primary:** per-task pass rate (mean over the 2 repeats), Weave minus native, pooled over all 163 fresh tasks (a task is a
  (category, id) pair), paired bootstrap 95% CI (10,000 resamples of tasks, seed 2026, percentile).
  Exact two-sided McNemar on repeat 0 and, separately, on repeat 1.
- **Descriptive, never headline:**
  - each category on its own, same method (the long-context result is the direct check of round 15);
  - the 56 tasks whose ids were never used to tune Weave (not in `DEV`, `R9_DEV`, `R9_DEV_B`, `R10_DEV_MP`, `R10_DEV_MF`);
  - model calls, input plus output tokens, list-price USD and wall seconds per task (wall times are affected by
    provider rate limits and are not an endpoint);
  - failure types per arm.
- The round 15 secondary "state-changing calls after a failed call" is not computed this round.

## What counts as a headline

The primary difference is at least +0.10 and its CI excludes 0. Anything else is reported as measured, losses
included, with no headline wording. A descriptive split cannot become a headline after the fact. If the primary is
below +0.10, or its CI includes 0, round 15's result is described as not confirmed.

## Power, honestly

With 163 tasks and 2 repeats, a true difference of +0.10 will exclude 0 only about half of the time; a true +0.15
most of the time. A miss would mean "not confirmed", not "disproved".

## Budget and stopping

- **Hard cap:** $25 for rounds 15 and 16 together, list price from the token counts in each response. Round 15 spent
  $8.7567 (including smoke runs).
- **Stop rule:** the chain starts no new invocation once cumulative spend reaches $20. Per-invocation caps
  (long context $2.00, miss_param $1.20, miss_func $1.60, per repeat) add up to $9.60, so the chain cannot pass
  about $18.4. Expected spend about $7 to $8.
- The runner also stops on a provider error rate above 10% in a 20-trial window. A stopped round reports the
  complete pairs it has. No tasks are added to make up the numbers.
- **Order and concurrency:** per repeat: long context, miss_param, miss_func; repeat 0 for all three, then repeat 1.
  Each invocation interleaves arms per task in a seeded order (seed 2026 for repeat 0, 2027 for repeat 1). Workers: 2
  for long context and 3 for the others, fewer than round 15's 4, to cut Azure 429 retries.
- The chain is `bench/bfcl/r16_chain.sh`, run with `nohup`, so an interrupted worker does not stop it. Logs are in
  `/workspace/weave-notes/r16-logs/`.

## Analysis

`bench/bfcl/report_r16.py`, committed with this file, run on the six result directories (one per category and
repeat) with the repeat number given on the command line. Complete pairs only. The script was tested on round 15's
data and reproduces its +0.100 (CI +0.005 to +0.200).

## Deviations

(none yet)
