# Round 4 diagnosis: GPT-4.1 first-turn runaway on git_review (dev suite only)

Dev tasks `git_review-n14-s902` and `git_review-n14-s903` (suite `real_dev_git`, never evidence).
Each sample is the exact first-turn Responses request of a Weave trial (system prompt, task, one
`run_weave` tool, temperature 0, max_output_tokens 4096), sent with `bench/v4/diag_turn1.py`.

| condition | samples | incomplete (max_output_tokens) | notes |
|---|---|---|---|
| round-3 tool text, parallel_tool_calls on (s902 only) | 8 | 0 | all 8 sent two scripts |
| round-3 tool text, parallel_tool_calls on (s902 + s903) | 28 | 5 | only incomplete lines were kept; each incomplete one held two completed scripts |
| + "Call this tool once per turn" rule, parallel on | 28 | 1 | 27 sent two scripts, 1 sent one |
| + rule, parallel_tool_calls off | 28 | 0 | every sample sent one script |

Every incomplete response contains two *completed* `function_call` items (the second one usually does
the whole task) and then spends the rest of the 4,096 output tokens on output the API does not return
(`incomplete-*.json`, 937 input + 4,096 output tokens billed each). The run in
`20261111T040923Z-real_dev_git-openai-gpt-4.1-2025-04-14-diag-baseline` (round-3 product, 24 Weave trials)
reproduced it end to end: 22/24 passed, 2 `provider_error:incomplete`, each now recorded with its 5,033
billed tokens.
