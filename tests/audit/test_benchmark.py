"""No API calls: equivalent endpoint accounting and native concurrency."""

import json
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from bench import real_tools_eval as ev  # noqa: E402


class Fake:
    def call(self, name, args):
        return json.dumps({"stdout": "42\n", "status": "ok"}), False


task = ev.RealTask("fake", "parallel", "Give the answer", "42")
usage = {"input_tokens": 7, "output_tokens": 3}
for final, expected in [("42", True), ("wrong", False)]:
    with (
        patch.object(ev.llm_eval, "complete", return_value=("print(42)", usage)),
        patch.object(ev, "final_answer", return_value=(final, usage)) as synthesis,
    ):
        r = ev.run_weave(
            "fake-model",
            task,
            Fake(),
            [{"name": "weave_run", "description": "Tools callable from the script:\n"}],
            Path("/tmp"),
        )
        assert r["passed"] is expected
        assert r["model_turns"] == r["attempts"] * 2
        assert r["input_tokens"] == r["attempts"] * 14
        assert synthesis.call_count == r["attempts"]

with tempfile.TemporaryDirectory() as temp:
    cfg = Path(temp) / "config.json"
    cfg.write_text(
        json.dumps(
            {
                "tools": [
                    {
                        "name": n,
                        "effect": "read",
                        "behavior": "const",
                        "result": {"t": "str", "v": n},
                        "delay_ms": 150,
                    }
                    for n in ("a", "b", "c")
                ]
            }
        )
    )
    c = ev.McpClient([str(ROOT / "target/debug/weave-mcp"), "--config", str(cfg)])
    try:
        tools = c.request("tools/list")["tools"]
        started = time.monotonic()
        out = ev.native_calls(c, tools, [(n, {}) for n in ("a", "b", "c")])
        elapsed = time.monotonic() - started
        assert elapsed < 0.4, elapsed  # serial minimum .45s
        assert len(out) == 3 and all(not error for _, error in out)
        print("native 3x150ms direct-call batch elapsed seconds", elapsed)
    finally:
        c.close()
print(
    "offline equal-final-endpoint, token accounting, and native parallel transport checks passed"
)

# V3 is the controlled offline successor; legacy v2 checks above are only
# historical function-level regressions, not a credible live evaluation.
suite = unittest.defaultTestLoader.loadTestsFromNames(
    ["bench.protocol_v3.tests.test_protocol", "bench.protocol_v3.tests.test_adapters"]
)
result = unittest.TextTestRunner(verbosity=2).run(suite)
if not result.wasSuccessful():
    raise SystemExit(1)
print("v3 deterministic protocol mechanics passed (live runs: python -m bench.protocol_v3.live, see docs/benchmarks.md)")
