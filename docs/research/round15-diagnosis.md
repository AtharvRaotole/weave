# Round 15 diagnosis: why GPT-5.4 mini lost on BFCL base in round 9

Round 9 (`mtb60r9`, run `bench/results/v12/20261116T054620Z-...-W3`): Weave 18/60, native 27/60, Δ −0.150
[−0.283, −0.017]. That loss stands and stays in the README. This note reads the committed traces only (no API calls)
to see what went wrong, so the fixes tried next come from failure types, not from individual tasks.

Reproduce: `bench/bfcl/diagnose_r9_losses.py RUN_DIR OUT.json` (needs `bfcl_eval` for the ground truth) and
`python bench/bfcl/error_flow.py`.

## The 17 discordant tasks

13 tasks passed native only, 4 passed Weave only, 14 passed both, 29 failed both. Grader verdicts on Weave's 42
failures: 29 state mismatch, 7 empty turn, 6 response mismatch (native's 33: 25, 6, 2).

| type | tasks (Weave lost) | what happened |
|---|---|---|
| Stopped after a tool error | 60, 189, 195 | A call failed (an `{"error": ...}` dict in 60, a raised error in 189 and 195) and the model answered instead of fixing the cause. Native looked up the missing value (airport codes) or met the precondition (lock doors) and retried. |
| Invented arguments for an extra write | 152, 167, 169 | The script called `authenticate_travel` with made-up values (`'unused_secret'`, `'First'`, `'Last'`), which changes state the grader compares. Native used the token the user gave. |
| Asked or declined instead of acting | 23, 117 | 23: refused to resolve a ticket with an empty resolution although the user asked for that. 117: asked to clarify an amount. |
| Skipped a conditional follow-up | 78 | Read the tire pressure (below the user's threshold) and did not go to the tire shop. |
| Wrote on an assumed state | 0, 1 | 0: `mkdir('temp')` before checking the working directory left a stray folder. 1: a loop compared `'./log.txt'` with `'log.txt'`, so the `mv` never ran and the script printed `done`. |
| Argument values | 147 | Same calls as native; tag and mention strings differed. |
| Unclear | 97 | Repeated reads and three lookups on error dicts; no single cause. |

The 4 Weave-only passes (55, 83, 84, 174) are mostly the mirror image of task 60: Weave recovered from
`startEngine`'s error dict and native stopped. With one sample per task, part of the 13 vs 4 split is the same failure
mode landing on different sides by chance. Repeats are needed to tell this apart.

## Error dicts across all recorded BFCL Weave traces (rounds 11 to 14)

From `bench/bfcl/error_flow.py`. "Stopped" means the model's next message was an answer, not another script. Some
stops are correct (the error was the answer), so these are not failure rates.

| | count |
|---|---|
| scripts where a tool returned an `{"error": ...}` dict | 626 |
| of those, the script still reported no error | 482 (77 %) |
| model stopped after such a script | 211 of 482 (44 %) |
| scripts where a tool raised instead | 61 |
| model stopped after a raised error | 23 of 61 (38 %) |
| scripts that made more calls after the first error dict | 76 (114 calls) |
| of those, with a state-changing call after it | 33 (56 calls) |

The last two rows are an upper bound on what "raise at the call that returned an error" could prevent: the `[calls]`
list is in start order, so a call that started alongside the failed one would have run anyway.

The stop rates after a dict (44 %) and after a raised error (38 %) are close, so these traces do not show that turning
error dicts into exceptions would make the model recover more often. They do show that most error dicts go by
without the script saying anything failed.
