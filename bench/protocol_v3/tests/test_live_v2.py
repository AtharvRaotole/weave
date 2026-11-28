"""Every live-v2 oracle is reachable: its reference call sequence, run against the real fixture (weave-mcp plus the
fixture servers, no model), grades as a pass, and a wrong answer or an extra write does not."""

import tempfile
import unittest
from pathlib import Path

from bench.protocol_v3.core import Clock
from bench.protocol_v3.live_tasks_v2 import FAMILIES, LIVE_V2, REFERENCE, live_v2_manifest
from bench.protocol_v3.local_mcp import ROOT, RemoteFixture


def play(task, calls, answer, extra=()):
    clock = Clock()
    with tempfile.TemporaryDirectory() as tmp:
        fixture = RemoteFixture(Path(tmp), task, clock, clock.now() + 60, 4, path="weave_mcp")
        try:
            events = [fixture.call(name, args) for name, args in [*calls, *extra]]
            fixture.quiesce()
            return events, task.grade(answer, fixture.snapshot(), fixture.writes, fixture.write_attempts)
        finally:
            fixture.close()


@unittest.skipUnless((ROOT / "target/debug/weave-mcp").exists(), "cargo build -p weave-mcp first")
class LiveV2(unittest.TestCase):
    def test_shape(self):
        self.assertEqual(len(LIVE_V2), 24)
        self.assertEqual(len({t.id for t in LIVE_V2}), 24)
        for family in FAMILIES:
            self.assertEqual(sum(t.family == family for t in LIVE_V2), 6, family)
        self.assertEqual(set(REFERENCE), {t.id for t in LIVE_V2})
        self.assertEqual(len(live_v2_manifest()["manifest_hash"]), 64)

    def test_reference_solutions_pass_and_wrong_ones_fail(self):
        for task in LIVE_V2:
            calls, answer = REFERENCE[task.id]
            with self.subTest(task=task.id):
                events, ok = play(task, calls, answer)
                self.assertTrue(ok, [(e["name"], e["error"]) for e in events])
                if task.family == "errors":
                    self.assertTrue(any(e["error"] for e in events), "an errors task must hit a failed call")
                self.assertFalse(play(task, calls, answer + "x")[1])
                self.assertFalse(play(task, calls, answer, [("fs_write", {"path": "extra.txt", "text": "x"})])[1])


if __name__ == "__main__":
    unittest.main()
