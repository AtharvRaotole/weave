"""Offline tests for the v4 harness: suites, oracle agents, statistics and the live opt-in guard.

Run: python bench/v4/tests/test_v4.py with weave installed (or PYTHONPATH=python after building the extension)
"""

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))  # `weave` itself must be installed or on PYTHONPATH

from bench.v4 import stats  # noqa: E402
from bench.v4.agent import trial  # noqa: E402
from bench.v4.mock import OracleClient  # noqa: E402
from bench.v4.run import main  # noqa: E402
from bench.v4.tasks import SUITES, manifest  # noqa: E402


class Suites(unittest.TestCase):
    def test_splits_are_disjoint_and_stable(self):
        dev = {t.fingerprint() for t in SUITES["dev"]}
        held = {t.fingerprint() for t in SUITES["held_out"]}
        self.assertFalse(dev & held)
        self.assertEqual(manifest("held_out")["hash"], manifest("held_out")["hash"])
        families_dev = {t.family for t in SUITES["dev"]}
        self.assertIn("notify_gold", {t.family for t in SUITES["held_out"]} - families_dev)

    def test_suites_do_not_depend_on_the_hash_seed(self):
        import subprocess
        code = "from bench.v4.tasks import manifest; print(*(manifest(s)['hash'] for s in ('dev', 'held_out', 'scaling', 'confirm2', 'real')))"
        outs = {subprocess.run([sys.executable, "-c", code], cwd=ROOT, capture_output=True, text=True, check=True,
                               env={**os.environ, "PYTHONHASHSEED": seed}).stdout for seed in ("1", "2", "3")}
        self.assertEqual(len(outs), 1, outs)

    def test_oracles_pass_every_task_in_both_modes(self):
        for suite in SUITES.values():
            for task in suite:
                if task.family == "pypi_major" and os.environ.get("WEAVE_BENCH_NET") != "1":
                    continue  # calls pypi.org; run with WEAVE_BENCH_NET=1
                for mode in ("weave", "native"):
                    row = trial(task, mode, OracleClient(task, mode), latency_ms=0)
                    self.assertTrue(row["passed"], (task.id, mode, row["status"], row["error_detail"]))

    def test_wrong_writes_fail_grading(self):
        task = next(t for t in SUITES["dev"] if t.family == "triage")

        class Lazy(OracleClient):
            def complete(self, messages, tools):  # answers without doing the writes
                return {"role": "assistant", "content": task.expected_answer}, {"input": 1, "output": 1}, []

        row = trial(task, "native", Lazy(task, "native"), latency_ms=0)
        self.assertFalse(row["passed"])
        self.assertFalse(row["state_ok"])

    def test_real_backends_grade_the_real_end_state(self):
        for family in ("sqlite_triage", "git_review", "warehouse_shortfall"):
            task = next(t for t in SUITES["real_dev"] if t.family == family)

            class Lazy(OracleClient):
                def complete(self, messages, tools):  # the right answer with no writes
                    return {"content": task.answer}, {"input": 1, "output": 1, "cached": 0, "reasoning": 0}, [{}]

            row = trial(task, "native", Lazy(task, "native"), latency_ms=0)
            self.assertTrue(row["answer_ok"], family)
            self.assertFalse(row["state_ok"], family)
            self.assertFalse(row["passed"], family)
            row = trial(task, "weave", OracleClient(task, "weave"), latency_ms=0)
            self.assertTrue(row["passed"], (family, row["error_detail"]))
            self.assertTrue(row["footprints"])


class Stats(unittest.TestCase):
    def test_wilson(self):
        lo, hi = stats.wilson(10, 10)
        self.assertAlmostEqual(hi, 1.0)
        self.assertTrue(0.69 < lo < 0.73)
        self.assertIsNone(stats.wilson(0, 0))

    def test_paired_and_mcnemar(self):
        rows = []
        for t in range(4):
            for rep in range(2):
                rows.append({"task": f"t{t}", "repeat": rep, "mode": "weave", "x": 1.0, "passed": True})
                rows.append({"task": f"t{t}", "repeat": rep, "mode": "native", "x": 2.0, "passed": t != 0})
        p = stats.paired(rows, lambda r: r["x"])
        self.assertEqual(p["mean_diff"], -1.0)
        self.assertAlmostEqual(p["ratio_gm"], 0.5)
        self.assertEqual(p["weave_better_tasks"], 4)
        m = stats.mcnemar_exact(rows)
        self.assertEqual((m["weave_only"], m["native_only"]), (2, 0))
        self.assertAlmostEqual(m["p"], 0.5)


class Runner(unittest.TestCase):
    def test_live_needs_both_opt_ins(self):
        os.environ.pop("WEAVE_BENCH_LIVE", None)
        with self.assertRaises(SystemExit) as cm:
            main(["--allow-live", "--model", "x", "--suite", "dev"])
        self.assertIn("refusing", str(cm.exception))

    def test_mock_run_writes_results(self):
        with tempfile.TemporaryDirectory() as tmp:
            main(["--mock", "--suite", "dev", "--repeats", "1", "--latency-ms", "0,50", "--out", tmp])
            (out,) = Path(tmp).iterdir()
            summary = json.loads((out / "summary.json").read_text())
            rows = [json.loads(line) for line in (out / "rows.jsonl").read_text().splitlines()]
            self.assertEqual(len(rows), len(SUITES["dev"]) * 2 * 2)
            self.assertTrue(all(r["passed"] for r in rows))
            self.assertTrue(summary)

    def test_footprints_close_the_write_loop_gap(self):
        # The oracle's triage script reads every ticket and closes some in one
        # loop. With declared footprints the reads of other tickets no longer
        # wait for each close, so execution time drops to about native's.
        task = next(t for t in SUITES["held_out"] if t.family == "triage")
        exec_s = {}
        for mode, fp in (("weave", True), ("weave_nofp", False), ("native", True)):
            real = "native" if mode == "native" else "weave"
            row = trial(task, real, OracleClient(task, real), latency_ms=100, footprints=fp)
            self.assertTrue(row["passed"], row)
            exec_s[mode] = row["exec_s"]
        self.assertLess(exec_s["weave"], 0.7 * exec_s["weave_nofp"], exec_s)
        self.assertLess(exec_s["weave"], 1.3 * exec_s["native"], exec_s)


class Accounting(unittest.TestCase):
    """Provider-billed usage of a failed request is kept, not recorded as 0 (round 4)."""

    def _incomplete_body(self):
        return json.dumps({"model": "m", "status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"},
                           "usage": {"input_tokens": 1500, "output_tokens": 4096,
                                     "input_tokens_details": {"cached_tokens": 0},
                                     "output_tokens_details": {"reasoning_tokens": 0}},
                           "output": [{"type": "function_call", "call_id": "c", "name": "run_weave",
                                       "arguments": "x" * 5000}]}).encode()

    def test_incomplete_response_raises_with_billed_usage(self):
        from unittest import mock
        from bench.v4 import providers

        class Resp:
            status = 200
            def __init__(self, body): self.body = body
            def read(self): return self.body
            def __enter__(self): return self
            def __exit__(self, *a): return False

        client = providers.ChatClient.__new__(providers.ChatClient)
        client.__dict__.update(provider="openai", model="m", max_tokens=4096, reasoning_effort=None, temperature=0,
                               retries=0, request_cap=10, sent=0, min_interval_s=0.0, timeout_s=5,
                               base="http://x", _key="k")
        with mock.patch("urllib.request.urlopen", return_value=Resp(self._incomplete_body())):
            with self.assertRaises(providers.ProviderError) as cm:
                client.complete([{"role": "user", "content": "hi"}], [])
        self.assertEqual(cm.exception.kind, "incomplete")
        self.assertEqual(cm.exception.usage["output"], 4096)
        self.assertEqual(cm.exception.usage["input"], 1500)
        self.assertIn("omitted", cm.exception.partial)
        self.assertLess(len(cm.exception.partial), 1700)

    def test_trial_row_counts_billed_tokens_of_a_failed_turn(self):
        from bench.v4.providers import ProviderError

        class Client:
            provider, model = "fake", "fake"
            def complete(self, messages, tools):
                raise ProviderError("incomplete", "max_output_tokens",
                                    usage={"input": 1000, "output": 4096, "cached": 0, "reasoning": 0}, partial="abc")

        task = next(t for t in SUITES["dev"])
        row = trial(task, "weave", Client(), latency_ms=0)
        self.assertEqual(row["status"], "provider_error:incomplete")
        self.assertFalse(row["passed"])
        self.assertEqual(row["total_tokens"], 5096)
        self.assertEqual(row["billed_on_error"]["output"], 4096)
        self.assertEqual(row["partial_output"], "abc")


class RequestOptions(unittest.TestCase):
    def test_weave_arm_sends_one_script_per_turn_and_native_keeps_parallel_calls(self):
        seen = {}

        class Client:
            provider, model = "fake", "fake"
            parallel_tool_calls = True
            def __init__(self, mode): self.mode = mode
            def complete(self, messages, tools):
                seen[self.mode] = self.parallel_tool_calls
                return {"content": "0"}, {"input": 1, "output": 1, "cached": 0, "reasoning": 0}, [{}]

        task = next(t for t in SUITES["dev"])
        for mode in ("weave", "native"):
            trial(task, mode, Client(mode), latency_ms=0)
        self.assertEqual(seen, {"weave": False, "native": True})
        seen.clear()
        for mode, ptc in (("weave", True), ("native", False)):  # the round-5 ablation arms
            trial(task, mode, Client(mode), latency_ms=0, parallel_tool_calls=ptc)
        self.assertEqual(seen, {"weave": True, "native": False})


if __name__ == "__main__":
    unittest.main()
