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
        code = "from bench.v4.tasks import manifest; print(*(manifest(s)['hash'] for s in ('dev', 'held_out', 'scaling')))"
        outs = {subprocess.run([sys.executable, "-c", code], cwd=ROOT, capture_output=True, text=True, check=True,
                               env={**os.environ, "PYTHONHASHSEED": seed}).stdout for seed in ("1", "2", "3")}
        self.assertEqual(len(outs), 1, outs)

    def test_oracles_pass_every_task_in_both_modes(self):
        for suite in SUITES.values():
            for task in suite:
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


if __name__ == "__main__":
    unittest.main()
