# tau-bench with a Weave arm

[tau-bench](https://github.com/sierra-research/tau-bench) (Sierra, MIT license) is used unmodified, pinned at
`59a200c6d575d595120f1cb70fea53cef0632f6b`. Its tasks, wiki, tools, LLM user simulator and grader
(`Env.calculate_reward`) decide every result.

- Native arm: tau-bench's `ToolCallingAgent`, unchanged.
- Weave arm: `weave_agent.py`, the same loop with one `run_weave` tool; each tool call in a script is an
  `env.step(Action(...))`. Effects (`read`/`pure`/`irreversible`) are our declarations.
- `run.py` wraps litellm's `completion` to meter agent and user-simulator tokens separately and to send the same
  `parallel_tool_calls`, `reasoning_effort` and retry settings in both arms.

```bash
git clone https://github.com/sierra-research/tau-bench /tmp/tau-bench && git -C /tmp/tau-bench checkout 59a200c
python -m venv /tmp/tauv && /tmp/tauv/bin/pip install -e /tmp/tau-bench
WEAVE_BENCH_LIVE=1 PYTHONPATH=python:. /tmp/tauv/bin/python -m bench.taubench.run --allow-live \
  --model gpt-4.1-2025-04-14 --tasks @retail_test7 --workers 2 --token-budget 1500000   # spends ~100k tokens per trial
```

Non-train splits need a pre-registered manifest hash and a clean tree.
