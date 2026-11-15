<!-- DRAFT. Do not post without Atharv's approval. Attach the charts named in brackets. -->

1/ Agents built on function calling pay one model round trip per round of tool calls. Weave lets the model send one
short script instead: loops, branches, many tool calls, one turn. Open source, Rust + Python. 🧵

2/ The runtime runs independent reads concurrently, waits to run writes until their inputs are ready, and returns only
what the script prints. Your tools stay plain Python functions or MCP servers.

3/ Pre-registered benchmarks, every row committed. Real services (SQLite, git, files, HTTP, PyPI): GPT-4.1 30/30 vs
18/30, 2.1 s vs 4.6 s per task, ~0.5× tokens. [assets/benchmark-v7-real.png]

4/ Conversations: on tau-bench retail (a simulated customer replies between steps) it is a draw. Nothing to batch when
every step waits for a person. [assets/benchmark-v10-tau.png]

5/ Public benchmark, BFCL v4 multi-turn, grader unchanged: same success within noise, 0.6 fewer model calls, but 1.22×
tokens and 1.36× cost. A loss on cost; short chains don't pay for the script. [assets/benchmark-v11-bfcl.png]

6/ So: use it for fan-out and chains (audit 40 dependencies, scan a dozen repos), skip it for one-call tasks and chat.

7/ Works with the OpenAI Agents SDK, LangGraph, and Claude Desktop/Cursor via MCP (`weave mcp-config --write`).
Paper with every loss: github.com/AtharvRaotole/weave
