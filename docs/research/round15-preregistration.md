# Round 15 pre-registration: does Weave have a large, repeatable win?

Written 2026-11-27, before any round 15 model call. This file has to be committed and pushed before the first call;
the commit time is the registration time. Nothing below changes after that, except the deviations section, appended
with dates.

## What we already know

- Round 9, GPT-5.4 mini on BFCL base: **18/60 vs 27/60, a significant loss**. It stays in the README.
- Round 11, same model and category, fresh 63 tasks, with the write gate: 31/63 vs 26/63, not significant.
- Pooled round 11: 159/378 vs 140/378, p = 0.07, not significant.
- Diagnosis of the round 9 loss: `docs/research/round15-diagnosis.md`. The three biggest failure types are stopping
  after a tool error, inventing arguments for an extra write, and asking instead of acting.

No result so far is a headline. This round tests whether general fixes for those failure types give a large gap that
holds on tasks nobody has looked at.

## Arms

- **native**: BFCL's own `OpenAICompletionsHandler` function calling, unchanged.
- **weave**: round 11's config (W3 plus `write_gate="choice"`) plus the two changes below. Nothing else changes.

### Changes under test, fixed now

1. **Declared error convention.** A tool can say that a dict result with a given key is a failure
   (`error_key="error"`). Weave then raises a `ToolError` with that message at the call, so the script stops before
   later writes and the model sees "tool X failed: ...", exactly as for a tool that raises. For BFCL the key is
   `"error"`: on the 140 train scenarios the backends report failures as `{"error": str}`. The change ships only if
   its unit tests pass; if it doesn't ship, the round runs without it and says so.
2. **Two sentences in the `run_weave` description**, general to any tool set:
   "If a call fails, fix the cause (look up what is missing or meet the precondition) and run the script again before
   you answer." and "Pass only values the user gave or a call returned. Never make up credentials or placeholders."

These came from failure types on the round 9 evidence traces, so the round 9 tasks are contaminated for this test.
They are rerun as a secondary set and reported separately, never pooled with the held-out set.

## Model and provider

`gpt-5.4-mini` (`2026-03-17` if the provider offers it), reasoning effort none, default temperature, through Azure
OpenAI. Round 9 used the OpenAI API. If the Azure deployment's model version differs, that goes in the deviations
section and the round 9 comparison is marked as a different provider.

## Task sets

- **Primary, held out: `lc100r15`.** BFCL `multi_turn_long_context`, a category no round has run. 100 ids:
  `sorted(random.Random("weave-round15-bfcl-heldout-lc").sample([i for i in range(200) if i not in R9_EVIDENCE], 100))`
  (`R9_EVIDENCE` from `bench/bfcl/subsets.py`, excluded because those 60 scenarios were read for the diagnosis).
- **Secondary, contaminated: `mtb60r9`.** The same 60 base tasks as round 9.
- **Repeats:** 2 per task per arm, so 400 + 240 = 640 trials.

## Endpoints

- **Primary:** per-task pass rate (mean over the 2 repeats), Weave minus native on `lc100r15`, with a paired
  bootstrap 95 % CI (10,000 resamples of tasks, seed 2026). Also an exact McNemar test on repeat 0.
- **Secondary, same CI method:**
  - pass rate on `mtb60r9`;
  - model calls, input and output tokens, list-price USD and wall seconds per task;
  - state-changing calls made after a failed call in the same script.

## What counts as a headline

The primary Δ is at least +0.10 and its CI excludes 0. Anything else is reported as measured, losses included, with
no headline wording. A secondary result can't become a headline after the fact.

## Budget and stopping

- **Hard cap:** $25 for all of round 15, counted at list price from the token counts in each response.
- **Plan:** about $12 for the BFCL part, measured as the run goes.
- **Stop rules:**
  - Stop starting trials when the running total reaches $18.
  - Stop at once on any provider error rate above 10 % in a 20-trial window.
  - A stopped round reports the complete pairs it has. No new tasks are added to make up the numbers.
- **Order:** `lc100r15` repeat 0, then `mtb60r9` repeat 0, then repeat 1 of each, interleaving the arms per task, so
  a stopped run still has a full paired primary sample.

## Live A/B outside BFCL

Same arms on `bench/protocol_v3`'s local MCP fixtures, with a new task set `live_v2` of 24 tasks: 6 each of fan-out
reads, dependent lookups, writes with a precondition, and error handling. The tasks are written and their manifest
hash is added here before any call. It runs only if the BFCL part leaves at least $6 of the cap. Endpoints: pass
rate, wall time, tokens, USD, and unsafe writes (a write not in the task's expected set, or after a failed call).

## Deviations

Recorded 2026-11-27, before any evidence trial (details in Addendum 15 of `docs/paper/preregistration.md`):

1. **"Held out" is narrower than it reads above.** All 100 `lc100r15` ids are base-category train scenarios (learned
   shapes and read/write classes include their base ground truth), and 53 were run live in other categories in
   earlier rounds. The 47 never run live are reported separately as a descriptive split.
2. **Run order in practice.** The runner shuffles all repeats of one invocation together, so each repeat is its own
   invocation (repeat 1 uses seed 2027), in the order given above.
3. **Live A/B harness.** protocol-v3's own runner is locked to api.openai.com and uses a different Weave mode, so the
   live A/B uses a new Chat Completions harness (`bench/protocol_v3/live_cc.py`) with the BFCL arm's general options
   on the same fixture and grader. The fixture's tools raise and never return error values, so `error_key` does not
   apply there. Repeats: 3.
4. **Smoke runs ($0.0289)** on a long-context id outside both sets, and on 2 `live-v2` tasks (both arms passed).
5. **Request timeout (2026-11-27 7:23 PM ET, during run A, before runs B, C, D and live-v2).** Azure left some
   long-context requests hanging until the client's default 600 s timeout, several times. From run B on, both arms'
   client uses a 120 s request timeout (still 2 retries). Run A keeps the default. This changes transport only, not
   the arms or the tasks; wall times in run A include these stalls and are reported with that caveat.
6. **Run A stopped and completed (2026-11-27, 7:36 PM ET).** Run A stalled repeatedly (no new row for 10 to 25 minutes at a time) on hung
   requests and was stopped with 178 rows: 89 complete pairs, no single-arm rows. Both arms of the other 11 tasks
   (`lc11r15a`: 20, 24, 52, 87, 109, 132, 133, 140, 149, 155, 159; manifest hash `c2723ce794503622`) run as repeat 0
   in a separate invocation with the 120 s timeout (seed 2026, cap $0.50), before run B. No result of run A was
   analysed before this decision; only row counts and spend were read.

