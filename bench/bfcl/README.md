# BFCL multi-turn: Weave arm vs BFCL's own OpenAI handler

[BFCL](https://github.com/ShishirPatil/gorilla/tree/main/berkeley-function-call-leaderboard) (Apache-2.0)
`multi_turn_base`: 200 tasks of several user turns each against stateful backends. The tasks, function docs,
backends and grader come from `bfcl-eval` unmodified; see `run.py` and `weave_arm.py` for exactly what each arm does.

```bash
uv venv -p 3.12 .bfcl && VIRTUAL_ENV=.bfcl uv pip install -r bench/bfcl/requirements-lock.txt
VIRTUAL_ENV=.bfcl uv pip install weave-lang   # or a wheel / maturin build for Python 3.12
PYTHONPATH=python:. .bfcl/bin/python bench/bfcl/test_offline.py            # offline: oracle passes, no-op fails
WEAVE_BENCH_LIVE=1 PYTHONPATH=python:. .bfcl/bin/python -m bench.bfcl.run --allow-live \
    --model gpt-4.1-2025-04-14 --tasks @dev6 --usd-budget 0.5             # spends money
```

`bfcl-eval` pins Python packages that do not build on 3.13 (`tree_sitter==0.21.3`), hence Python 3.12. The lock file
omits BFCL's optional local-model and web-search extras (torch, sentence-transformers, faiss); multi-turn grading does
not use them.
