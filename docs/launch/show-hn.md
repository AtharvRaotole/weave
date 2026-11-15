<!-- DRAFT. Do not post without Atharv's approval. Requires the repo to be public first. -->

**Title:** Show HN: Weave – agents send one script instead of many tool calls (with benchmarks, including losses)

**URL:** https://github.com/AtharvRaotole/weave

**Text:**

Weave is a small Rust runtime with Python bindings. Instead of calling tools one round trip at a time, the model sends
one Python-like script through a single `run_weave` tool; Weave runs independent reads concurrently, orders writes by
declared effects, and returns only what the script prints.

It plugs into Chat Completions/Responses, the OpenAI Agents SDK, LangChain/LangGraph, and any MCP client
(`weave mcp-config --client claude --write` sets up Claude Desktop or Cursor).

We pre-registered the benchmarks and committed every row. The honest summary:

- Real services (SQLite, git, files, HTTP, PyPI): GPT-4.1 passed 30/30 vs 18/30 with native function calling, in
  2.1 s vs 4.6 s per task and about half the tokens.
- tau-bench retail (a simulated customer between steps): a draw.
- BFCL v4 multi-turn (public tasks and grader, unchanged): same success within noise, 0.6 fewer model calls per task,
  but 1.22× the tokens and 1.36× the cost. Short chains leave little to batch.

So it is for fan-out and chained tool work (audit 40 packages, scan 12 repos), not for chat flows or one-call tasks.
Paper, pre-registration and raw rows are in the repo. Feedback on the language subset and the effect model is welcome.
