# Round 9 research notes: making Weave cheaper and at least as accurate on short chains

Written before the round-9 evidence runs. It covers what we read, what the round-8 BFCL traces showed, and which
techniques we built and ablated on train tasks. Results are in `docs/benchmarks.md` and paper §5.17.

## 1. What we read

| topic | source | what we took from it |
|---|---|---|
| Prompt caching mechanics | OpenAI, [Prompt caching guide](https://developers.openai.com/api/docs/guides/prompt-caching); [Prompt Caching 201](https://developers.openai.com/cookbook/examples/prompt_caching_201); [launch post](https://openai.com/index/api-prompt-caching/) | Exact-prefix reuse from 1,024 tokens in 128-token steps; tool definitions are part of the prefix; routing hashes the first ~256 tokens; about 15 requests per minute per prefix (and key) per machine before overflow; `prompt_cache_key` improves stickiness on models before GPT-5.6 (one customer: 60 % → 87 % hits). |
| Why a cache missed | OpenAI, [Prompt cache diagnostics](https://developers.openai.com/api/docs/guides/prompt-caching/diagnostics) | `tools_changed` and `input_changed` break reuse: keep tool text byte-stable and append-only history. |
| Code instead of tool calls | Wang et al., [CodeAct](https://arxiv.org/abs/2402.01030); Hugging Face [smolagents](https://github.com/huggingface/smolagents) | Code actions compose tools and cut steps (smolagents reports ~30 % fewer LLM calls); `final_answer()` inside the code ends the loop without another model turn. |
| Code Mode | Cloudflare, [Code Mode](https://blog.cloudflare.com/code-mode/); Anthropic, [Code execution with MCP](https://www.anthropic.com/research/code-execution-with-mcp); Anthropic, [Advanced tool use](https://www.anthropic.com/research/advanced-tool-use) and [programmatic tool calling](https://platform.claude.com/docs/en/agents-and-tools/tool-use/programmatic-tool-calling) | Present tools as a typed API; keep intermediate results out of context; load definitions on demand. Their gains come from many tools and large intermediate data, which BFCL multi-turn does not have. |
| Planning and parallel calls | Kim et al., [LLMCompiler](https://arxiv.org/abs/2312.04511); Xu et al., [ReWOO](https://arxiv.org/abs/2305.18323) | Plan a dependency graph, run independent calls in parallel, and answer from the evidence without re-prompting per observation (ReWOO: 5× token efficiency on HotpotQA). The Weave `reply()` below is the ReWOO "solver" folded into the script. |
| Tool design | Anthropic, [Writing effective tools for agents](https://www.anthropic.com/engineering/writing-tools-for-agents) | Token-efficient tool responses, clear namespaced descriptions, meaningful errors that steer the agent. |
| Verification | Madaan et al., [Self-Refine](https://arxiv.org/abs/2303.17651); Shinn et al., [Reflexion](https://arxiv.org/abs/2303.11366) | Feedback from execution helps the model repair; we use runtime checks (types, error values) as that feedback, at no model cost. |
| The benchmark | Gorilla, [BFCL V3 multi-turn](http://gorilla.cs.berkeley.edu/blogs/13_bfcl_v3_multi_turn.html) | A task passes only if backend state and required call results match after every turn. |

## 2. Diagnosis of round 8 (from committed traces; `bench/bfcl/diagnose.py`)

Per paired trial, Weave minus native: +6.2k input tokens (GPT-4.1 and GPT-5.4 mini alike), +190–200 output
tokens, −0.6 model calls.

- **The tool description is billed larger than native schemas.** Probed with the API (`max_tokens=1`, one user
  message): task 73 native tools bill 2,058 prompt tokens, the round-8 Weave description 2,902; task 4: 3,242 vs
  4,106. OpenAI renders JSON schemas compactly server-side (tiktoken counts the raw JSON at about 2× the billed size),
  while Weave's text description is billed as written. About 850 extra tokens on every request.
- **Repetition inside that description.** BFCL docs repeat the same sentences in every function ("This tool belongs
  to the vehicle control system, which allows users to …", "Note that the provided function is in Python 3
  syntax."), and the listing repeated `effect reversible` and argument types already in the signature.
- **Cache misses (GPT-4.1).** Weave's cached share was 57 % vs native's 72 %. Weave's first ~256 tokens are the same
  rules for every task, so concurrent trials of different tasks share one routing hash and overflow; native schemas
  start with task-specific names. Uncached input per GPT-4.1 trial: 13.8k vs 7.4k.
- **Wrong result types cost round trips.** 33 script errors read `order_history` where `get_order_history` returns
  `history`; 8 read a nested `tirePressure` that is flat; BFCL's doc `response` schemas are not what the backends
  return. 13 more came from a harness defect: backend results are Python `repr` text (single quotes), which
  `json.loads` rejected, so scripts got strings.
- **A turn always costs at least two model calls** (tool step, then a text reply that only restates the output).
  BFCL turns need about 1.6 calls per script, so the final restatement is a large share of all calls.
- **Failure modes.** Most failures are `instance_state_mismatch` in both arms. Weave-specific ones seen in dev:
  chaining blindly through odd data (`get_order_details('order_type')` because the history list ends with a label),
  and, once `reply()` existed, ending a turn before doing the requested change.

## 3. Techniques built (each general, not BFCL-specific, except where noted)

1. **Compact description** (`WeaveTool(compact=True)`): shorter rules, and `compact_listing` factors any sentence of
   40+ characters that appears in 3+ docs into a lettered note (`[A] …`) cited per line, prints a shared effect once,
   drops a label every doc starts with, and the BFCL adapter stops repeating argument types the signature already
   shows. Dev20 mean description: 3,259 → 2,057 tiktoken tokens.
2. **Cache routing key in both arms**: `prompt_cache_key=<arm>-<task>` on every request of a trial, the same setting
   for native and Weave (equal settings; it helps both).
3. **Typed results learned from train executions** (BFCL adapter; uses `shape_of_samples`): return shapes from
   executing ground-truth calls of the 140 train tasks (and argument-free functions on throwaway instances); the 60
   evidence tasks are excluded. Plus parsing Python-literal results (`ast.literal_eval`).
4. **Call trace in the output** (`trace`): every call of a script and its result (300 characters each, 30 calls max)
   follow the printed output, so the model sees what the native loop would have shown while calls stay batched.
5. **`reply(text)`** (`WeaveTool(reply=True)`): a script that succeeds and calls `reply` ends the turn with that text,
   saving the model call that only restates the output (smolagents' `final_answer`, ReWOO's solver).
6. **Guarded reply** (`guard`): the reply is withheld when any call returned an error value (`{"error": …}`), and the
   model continues with the output instead.
7. **Checked arguments** (`typecheck`): a call whose argument does not match its declared simple type (an int id
   given a str label) is refused once with an error the model can act on; the same call sent again goes through.
   On BFCL's ground truth for train tasks, 1 of 1,323 arguments would be refused the first time.
8. **Hybrid routing** (`hybrid`; `WeaveTool(listing=False)`): native schemas for every function plus a short
   `run_weave` for two or more calls.

Ablations, decisions and their numbers are in paper §5.17 (dev rows: `bench/results/v12/*-dev-*`).

## 4. Outcome

W3 (compact, learned shapes, trace, type check) ran on 60 fresh tasks per model. It was cheaper for both models
(0.85× and 0.88× list price), but GPT-5.4 mini passed 18/60 against native's 27/60, a significant loss; GPT-4.1
passed 38/60 against 35/60. The fan-out reconfirm passed 40/40 against 27/40 at about half the cost. Full tables:
paper §5.17. Next idea not built: read-before-write, i.e. stop a script at its first state-changing call whose
inputs came from an unverified assumption, so a small model sees one result before it batches writes.
