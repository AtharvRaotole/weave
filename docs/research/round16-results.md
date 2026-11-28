# Round 16 results

Plan: [`round16-preregistration.md`](round16-preregistration.md), committed and pushed before the first round 16
model call (branch `research/round16-prereg`, registration commit `c30cd3e`). Analysis code:
`bench/bfcl/report_r16.py` (pre-registered). Raw rows and traces: `bench/results/v16/`. Merged miss_func repeat 1
and reports: `bench/results/v16/analysis/`.

## Short version

- **Primary (all 163 fresh (category, id) tasks, 2 repeats):** Weave passed 0.319, native function calling 0.282.
  The difference is **+0.037, 95% CI -0.025 to +0.098**.
- **The headline rule is not met.** The rule was "difference at least +0.10 and CI excludes 0". The point estimate
  is well below +0.10, and the CI includes 0. By the pre-registered wording, **round 15's result is not confirmed**.
- **The direct long-context check also misses.** On the 39 fresh long-context tasks, Weave was +0.064 (95% CI
  -0.038 to +0.167). That interval includes 0, and it does not reproduce round 15's +0.100 (95% CI +0.005 to +0.200)
  on 100 held-out long-context tasks.
- **The round 9 loss stands:** GPT-5.4 mini on BFCL base, 18/60 vs 27/60 (OpenAI API, round 9).
- **Spend:** $14.81 of the $25 cap for rounds 15 and 16 together ($8.76 in round 15, $6.05 in round 16). No billing
  outside the cap.

## Setup (as pre-registered)

- Model `gpt-5.4-mini-2026-03-17`, reasoning effort none, through Azure AI Foundry (the same endpoint as round 15).
  Round 9 used the OpenAI API, so comparisons with round 9 are across providers.
- Arms: native BFCL function calling, and Weave with the same config as round 15
  (`compact,learned,trace,typecheck,gatechoice,errkey,recover`). No Weave-arm code changed between the two rounds.
- Fresh means a (category, id) pair that no earlier round ran live: 39 long-context, 57 miss_param, and 67 miss_func
  tasks, 2 repeats each (326 trial pairs). Every scenario id has been run live in some category before. 107 of the
  163 tasks have ids that were used to tune Weave in the base category; 56 do not. That 56-task split is descriptive
  only.
- Per-task pass rate is the mean over the 2 repeats. CI is a paired bootstrap over tasks (10,000 resamples, seed
  2026, percentile). McNemar is exact, two-sided, on repeat 0 and, separately, on repeat 1.
- Headline rule, unchanged: the primary difference is at least +0.10 and its CI excludes 0. A descriptive split
  cannot become a headline after the fact.

## Primary: all 163 fresh tasks

| | Weave | Native | Difference | 95% CI |
|---|---|---|---|---|
| Pass rate (326 trial pairs) | 0.319 (104) | 0.282 (92) | **+0.037** | -0.025 to +0.098 |
| Model calls per task | 9.77 | 10.37 | -0.60 | -0.92 to -0.28 |
| Tokens per task | 31,495 | 40,128 | -8,633 | -12,257 to -5,634 |
| List-price USD per task | 0.00891 | 0.00941 | -0.00050 | -0.00119 to +0.00005 |
| Wall seconds per task (see caveat) | 57.0 | 36.2 | +20.8 | -1.8 to +65.3 |

Headline rule: difference at least +0.10 and CI excludes 0. **Not met.** The CI includes 0, and +0.037 is far from
the +0.10 bar. Tokens are lower for Weave (the token CI excludes 0, about 22% fewer). List-price cost is slightly
lower, but that CI includes 0.

Repeat by repeat (pre-registered as reported McNemar, not as the headline):

| Repeat | Weave | Native | Weave only | Native only | McNemar p |
|---|---|---|---|---|---|
| 0 | 49/163 | 45/163 | 27 | 23 | 0.67 |
| 1 | 55/163 | 47/163 | 25 | 17 | 0.28 |

Neither repeat is significant on its own.

## Descriptive: each category

These were pre-registered as descriptive. None of them can be treated as a headline.

| Category | Tasks | Pairs | Weave | Native | Difference | 95% CI | Repeat 0 McNemar p |
|---|---|---|---|---|---|---|---|
| long context | 39 | 78 | 0.333 (26) | 0.269 (21) | +0.064 | -0.038 to +0.167 | 0.73 |
| miss_func | 67 | 134 | 0.306 (41) | 0.209 (28) | +0.097 | +0.022 to +0.179 | 0.48 |
| miss_param | 57 | 114 | 0.325 (37) | 0.377 (43) | -0.053 | -0.184 to +0.070 | 0.84 |

The long-context result is the direct check of round 15. It points in the same direction, but it is smaller, its CI
includes 0, and 39 tasks is a much smaller set than the original 100. miss_func is the only category whose CI
excludes 0 (difference +0.097, 95% CI +0.022 to +0.179). That is still a descriptive split, and its repeat 0 McNemar
is not significant (p = 0.48); repeat 1 is p = 0.049 (23/67 vs 14/67). miss_param's point estimate favors native and
is not significant.

## Descriptive: 56 never-tuned tasks

The 56 tasks whose ids are not in `DEV`, `R9_DEV`, `R9_DEV_B`, `R10_DEV_MP`, or `R10_DEV_MF` (22 long-context, 17
miss_param, 17 miss_func):

| | Weave | Native | Difference | 95% CI |
|---|---|---|---|---|
| Pass rate (112 trial pairs) | 0.330 (37) | 0.277 (31) | +0.054 | -0.045 to +0.152 |

The other 107 tasks, whose ids were used to tune Weave in base: 0.313 (67/214) vs 0.285 (61/214), difference +0.028
(-0.051 to +0.107). Both intervals include 0. Repeat 0 on the 56 is 19/56 vs 16/56 (McNemar p = 0.61).

## Failure types and 429s

Over 326 trials per arm. Weave: 104 pass, 85 state mismatch, 39 response mismatch, 98 empty turn. Native: 92 pass,
98 state mismatch, 52 response mismatch, 84 empty turn. The small raw pass gap comes from fewer wrong final states
and fewer response mismatches, while Weave again has more empty-turn failures (the model gave no tool call in a
turn). That weakness showed up in round 15 too (21 vs 11 on the long-context set) and is still there.

HTTP 429s in the logs, after dropping false matches on spend figures: 2 in long-context repeat 0 (step 1) and 1 in
long-context repeat 1 (step 4). None in the miss_param or miss_func logs, including the resume. All 326 trial pairs
finished with a normal graded result, and no row carries a provider error. That is far fewer 429s than round 15,
which had on the order of 100+ in the long-context runs. The three ~7200 s stalls in miss_func repeat 1 (see
deviations) did not log a 429.

## Deviations and caveats

Written into the pre-registration before the resume trials they affect (commit `c23aae2`), except where noted.

1. **First chain start had no native module.** The first start used a new worktree that did not have the built
   `weave._native` extension. The weave arm errored on every trial. The process was killed after 10 rows; the native
   rows that had already made model calls cost about $0.067. That directory is kept as
   `...-ABORTED-no-native-module`, is never analysed, and is still counted in spend. The `.so` was copied from the
   round 15 worktree (same Rust sources). The chain then restarted from step 1 with fresh result directories.
2. **Chain wrapper.** The notes copy of the chain script adds `cd $W || exit 1`. The committed
   `bench/bfcl/r16_chain.sh` does not.
3. **miss_func repeat 1 was split.** Three trials stalled with wall times of about 7200 s (weave on id 103, both
   arms on id 30), even though the client timeout is 120 s. The runner was stopped at 81 of 134 trials (40 complete
   pairs, plus weave only on id 131). The remaining 27 ids were registered as `mf27r16resume` (hash
   `7ca5b193ef97258c`) and run with both arms, 2 workers, seed 2027, after that commit was pushed. This is a resume
   of the pre-registered `mf67r16` set, not a new sample. Analysis merges the two directories as repeat 1. Id 131
   weave was run twice; the merge keeps the resume row, and both rows are counted in spend.
4. **Wall times are unreliable.** The three stalls above dominate the wall-time row (two on Weave, one on native),
   which is why Weave looks slower in the primary table. Dropping those three task-repeat pairs, as a post hoc
   check, leaves 324 pairs and a wall difference of about -1.3 s (95% CI -2.0 to -0.5) in Weave's favor. Wall time
   is not an endpoint, and that check was not pre-registered. Treat the wall-time row as unreliable.
5. **How fresh the tasks are.** All 163 ids are base-category train scenarios. 107 of 163 have ids that earlier
   rounds used to tune Weave. The 56-task split is above. Round 10 and 11 already ran miss_param / miss_func with
   the write gate and did not show a significant win there.
6. **Single model, single provider.** Nothing here says what happens with other models. The round 9 loss (18/60 vs
   27/60 on the OpenAI API) stays on the record.

## Verdict

On 163 fresh (category, id) tasks with the same model, endpoint and Weave config as round 15, Weave passed a few
more trials than native function calling (104/326 vs 92/326), used about 22% fewer tokens, and did not show a
reliable cost gap. The pre-registered headline rule required a primary difference of at least +0.10 whose CI
excludes 0. The measured difference is +0.037 (95% CI -0.025 to +0.098). That does not meet the rule, so round 15's
barely-met long-context result (+0.100, 95% CI +0.005 to +0.200 on 100 tasks) is not confirmed. The 39-task
long-context slice here also includes 0. A descriptive miss_func gain is real on this sample but is not a headline.
miss_param slightly favors native and is not significant. The round 9 loss of 18/60 vs 27/60 stays on the record.
The honest summary is: Weave still looks a bit cheaper in tokens and maybe slightly ahead on some multi-step BFCL
categories with this model, but the confirmation run did not show a stable, pre-registered win, and it is not a
result that should be described as record-breaking or as proving the earlier long-context gain.
