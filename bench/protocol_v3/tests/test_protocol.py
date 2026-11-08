"""No sockets, API keys, external servers or model predictions. Mechanical oracles only."""

from dataclasses import replace
import json
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import patch

from bench.protocol_v3.core import (
    Config,
    Fixture,
    ProviderFailure,
    Reply,
    execute,
    manifest,
    run,
    summarize,
)
from bench.protocol_v3.fakes import ManualClock, ScriptedProvider, TOKENS
from bench.protocol_v3.tasks import DEVELOPMENT, HELD_OUT, validate_splits


def config(**changes):
    values = dict(
        provider="deterministic-fake",
        model_revision="fake-revision-001",
        resolved_model_revision="fake-revision-001",
        api_version="offline-v1",
        source_commit="a" * 40,
        server_versions={"fixture-backend": "protocol-v3.1"},
        generation={"temperature": 0, "max_output_tokens": 256, "reasoning": "none"},
    )
    values.update(changes)
    return Config(**values)


def evaluate(responses, mode="weave", task=DEVELOPMENT[0], **cfg):
    clock = ManualClock()
    provider = ScriptedProvider(responses, clock)
    return run(task, mode, provider, config(**cfg), clock=clock)


def calls(*entries):
    return {"calls": [{"name": name, "arguments": args} for name, args in entries]}


class ProtocolTests(unittest.TestCase):
    def test_final_answer_not_stdout_is_the_endpoint(self):
        for mode, action in [("weave", {"script": "print(31)\n"}), ("native", calls())]:
            bad = evaluate([action, {"answer": "wrong"}], mode, max_answers=1)
            self.assertFalse(bad["passed"])
            self.assertEqual(bad["status"], "wrong_answer_or_state")
            self.assertEqual(bad["known_tokens"], {k: v * 2 for k, v in TOKENS.items()})
            good = evaluate([action, {"answer": "31"}], mode)
            self.assertTrue(good["passed"])
            self.assertEqual(good["model_turns"], 2)
            self.assertEqual(good["endpoint"], "provider_final_answer_and_actual_state")

    def test_same_schemas_common_context_and_result_bytes(self):
        rows = []
        for mode, action in [
            ("weave", {"script": 'a=read_file(path="a.txt")\n'}),
            ("native", calls(("read_file", {"path": "a.txt"}))),
        ]:
            row = evaluate([action, {"answer": "31"}], mode)
            self.assertTrue(row["passed"])
            rows.append(row)
        a, b = rows
        self.assertEqual(a["schema_hash"], b["schema_hash"])
        self.assertEqual(a["context_hash"], b["context_hash"])
        for key in ("system", "task", "tools", "generation"):
            self.assertEqual(a["requests"][0][key], b["requests"][0][key])
        self.assertEqual(a["feedback"], b["feedback"])
        # No script stdout or trace is substituted as a final answer/context payload.
        self.assertNotIn("stdout", json.dumps(a["feedback"]))

    def test_same_tool_error_feedback_and_fallback_data(self):
        task = DEVELOPMENT[1]
        script = 'try:\n    x=read_file(path="missing.txt")\nexcept ToolError:\n    x=read_file(path="backup.txt")\n'
        a = evaluate([{"script": script}, {"answer": "safe-backup"}], task=task)
        b = evaluate(
            [
                calls(("read_file", {"path": "missing.txt"})),
                calls(("read_file", {"path": "backup.txt"})),
                {"answer": "safe-backup"},
            ],
            "native",
            task,
        )
        self.assertTrue(a["passed"] and b["passed"])
        self.assertEqual(
            sorted(
                a["feedback"][0]["observations"], key=lambda o: o["arguments"]["path"]
            ),
            sorted(
                [o for f in b["feedback"] for o in f.get("observations", [])],
                key=lambda o: o["arguments"]["path"],
            ),
        )
        self.assertEqual(
            next(o["error"] for o in a["feedback"][0]["observations"] if o["error"]),
            {"kind": "FileNotFoundError", "message": "tool request failed"},
        )

    def test_real_file_write_validation_not_claimed_answer(self):
        task = DEVELOPMENT[2]
        for mode in ("weave", "native"):
            self.assertFalse(
                evaluate([{"answer": "recorded"}], mode, task, max_answers=1)["passed"]
            )
            action = (
                {"script": 'write_file(path="receipt.txt",text="RECEIVED blue")\n'}
                if mode == "weave"
                else calls(
                    ("write_file", {"path": "receipt.txt", "text": "RECEIVED blue"})
                )
            )
            row = evaluate([action, {"answer": "recorded"}], mode, task)
            self.assertTrue(row["passed"])
            self.assertEqual(row["writes"], [("receipt.txt", "RECEIVED blue")])
            # Additional write, even with identical final contents, is not allowed.
            bad = evaluate(
                [action, action, {"answer": "recorded"}], mode, task, max_answers=1
            )
            self.assertFalse(bad["passed"])
            wrong = (
                {"script": 'write_file(path="receipt.txt",text="WRONG")\n'}
                if mode == "weave"
                else calls(("write_file", {"path": "receipt.txt", "text": "WRONG"}))
            )
            self.assertFalse(
                evaluate([wrong, {"answer": "recorded"}], mode, task, max_answers=1)[
                    "passed"
                ]
            )

    def test_unexpected_write_and_path_escape_fail(self):
        for action in [
            calls(("write_file", {"path": "extra", "text": "x"})),
            calls(("write_file", {"path": "../outside", "text": "x"})),
        ]:
            row = evaluate([action, {"answer": "31"}], "native", max_answers=1)
            self.assertFalse(row["passed"]) if "extra" in json.dumps(
                action
            ) else self.assertIsNotNone(row["feedback"][0]["observations"][0]["error"])

    def test_fresh_isolated_state_every_replica_and_mode(self):
        task = DEVELOPMENT[2]
        roots = set()
        hashes = set()
        for mode in ("native", "weave"):
            for _ in range(3):
                row = evaluate([{"answer": "recorded"}], mode, task, max_answers=1)
                roots.add(row["fixture_root"])
                hashes.add(row["initial_state_hash"])
                self.assertFalse(Path(row["fixture_root"]).exists())
                self.assertEqual(row["writes"], [])
        self.assertEqual(len(roots), 6)
        self.assertEqual(len(hashes), 1)

    def test_provider_transport_retry_timing_and_all_known_usage(self):
        for mode in ("native", "weave"):
            row = evaluate(
                [
                    ProviderFailure("rate_limit", usage=TOKENS, retry_after_s=2),
                    {"answer": "31"},
                ],
                mode,
            )
            self.assertTrue(row["passed"])
            self.assertEqual(row["wall_s"], 4)
            self.assertEqual(row["rate_limit_wait_s"], 2)
            self.assertEqual(row["known_tokens"], {k: v * 2 for k, v in TOKENS.items()})
            self.assertEqual(len(row["provider_attempts"]), 2)
            self.assertEqual(
                len({a["request_hash"] for a in row["provider_attempts"]}), 1
            )
            self.assertEqual(row["model_turns"], 1)

    def test_unknown_failure_usage_is_not_zero_or_silently_complete(self):
        row = evaluate([ProviderFailure("transient"), {"answer": "31"}])
        self.assertTrue(row["passed"])
        self.assertEqual(row["known_tokens"], TOKENS)
        self.assertEqual(row["unknown_usage_attempts"], 1)
        self.assertFalse(row["token_totals_complete"])

    def test_answer_repairs_get_equal_feedback_and_budget_no_oracle_leak(self):
        rows = [
            evaluate([{"answer": "wrong"}, {"answer": "31"}], mode)
            for mode in ("native", "weave")
        ]
        for row in rows:
            self.assertTrue(row["passed"])
            self.assertFalse(row["first_pass"])
            self.assertEqual(row["wall_s"], 2)
            self.assertEqual(row["answer_submissions"], 2)
            self.assertEqual(
                row["feedback"],
                [
                    {
                        "validation": "final answer or actual state invalid; expected values withheld"
                    }
                ],
            )
            prompt = json.dumps(row["requests"][0])
            self.assertNotIn("31", prompt)
            self.assertNotIn("final_files", prompt)
            self.assertNotIn("expected_writes", prompt)
        self.assertEqual(rows[0]["feedback"], rows[1]["feedback"])

    def test_failure_timeout_budget_and_malformed_rows_retained(self):
        rows = [
            evaluate([ProviderFailure("timeout", usage=TOKENS)]),
            evaluate([ProviderFailure("budget")]),
            evaluate([{"garbage": True}]),
            evaluate([{"script": "pass\n"}], max_turns=1),
            evaluate([{"answer": "no"}], max_answers=1),
        ]
        self.assertEqual(
            [r["status"] for r in rows],
            [
                "timeout",
                "budget_exhausted",
                "malformed_response",
                "turn_limit",
                "wrong_answer_or_state",
            ],
        )
        self.assertTrue(all(r["wall_s"] == 1 and not r["passed"] for r in rows))
        summary = summarize(rows)
        self.assertEqual(summary["runs"], 5)
        self.assertEqual(sum(summary["failures"].values()), 5)
        self.assertEqual(summary["p95_all_wall_s"], 1)

    def test_timeout_wait_is_inclusive_and_cannot_dispatch_write(self):
        row = evaluate(
            [ProviderFailure("rate_limit", retry_after_s=100)],
            task=DEVELOPMENT[2],
            timeout_s=3,
        )
        self.assertEqual(row["wall_s"], 3)
        self.assertEqual(row["status"], "timeout")
        self.assertEqual(row["writes"], [])
        late = evaluate([{"answer": "31"}], timeout_s=0.5)
        self.assertEqual(late["status"], "timeout")
        self.assertEqual(late["wall_s"], 1)
        self.assertEqual(late["known_tokens"], TOKENS)

    def test_source_model_server_generation_config_pins_and_drift(self):
        for changes in [
            dict(source_commit="main"),
            dict(model_revision="latest"),
            dict(server_versions={"fs": "*"}),
            dict(generation={}),
            dict(
                generation={
                    "temperature": 0,
                    "max_output_tokens": 3,
                    "reasoning": "none",
                    "api_key": "MUST_NOT_APPEAR",
                }
            ),
        ]:
            with self.assertRaises(ValueError):
                config(**changes)
        row = evaluate([Reply({"answer": "31"}, TOKENS, "different-revision")])
        self.assertEqual(row["status"], "provider_error")
        self.assertFalse(row["passed"])
        self.assertEqual(
            row["provider_attempts"][0]["outcome"], "model_revision_mismatch"
        )
        self.assertEqual(row["config"]["model_revision"], "fake-revision-001")
        self.assertNotIn("environ", json.dumps(row["config"]))

    def test_live_provider_rejected_before_any_request(self):
        clock = ManualClock()
        p = ScriptedProvider([], clock)
        p.offline = False
        with self.assertRaisesRegex(ValueError, "live adapters"):
            run(DEVELOPMENT[0], "weave", p, config(), clock=clock)
        self.assertEqual(p.requests, [])

    def test_development_heldout_partition_and_unfamiliar_schemas(self):
        split = validate_splits()
        self.assertFalse(split["statistically_independent"])
        self.assertFalse(split["secret_or_unseen"])
        with self.assertRaises(ValueError):
            validate_splits(
                DEVELOPMENT, (replace(DEVELOPMENT[0], split="held_out_candidate"),)
            )
        self.assertNotEqual(manifest(DEVELOPMENT[0]), manifest(HELD_OUT[0]))
        row = evaluate([{"answer": "2"}], task=HELD_OUT[0])
        self.assertTrue(row["passed"])
        self.assertEqual(row["split"], "held_out_candidate")
        self.assertNotIn("answer", row["requests"][0]["task"])

    def test_parallel_reads_barrier_around_write_and_stable_result_order(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = Fixture(Path(tmp), DEVELOPMENT[0], ManualClock(), 100)
            original = fixture.call
            barrier = threading.Barrier(2)
            reads_done = []
            lock = threading.Lock()

            def observed(name, args):
                if name == "read_file":
                    barrier.wait(timeout=3)
                    result = original(name, args)
                    with lock:
                        reads_done.append(args["path"])
                    return result
                self.assertEqual(len(reads_done), 2)
                return original(name, args)

            fixture.call = observed
            result = execute(
                fixture,
                "native",
                calls(
                    ("read_file", {"path": "a.txt"}),
                    ("read_file", {"path": "b.txt"}),
                    ("write_file", {"path": "out", "text": "done"}),
                ),
                2,
            )
            self.assertEqual(
                [r["result"] for r in result["observations"]], ["11", "20", None]
            )
            self.assertEqual(fixture.writes, [("out", "done")])

    def test_parallel_width_is_bounded_without_wall_clock_speed_claim(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = Fixture(Path(tmp), DEVELOPMENT[0], ManualClock(), 100)
            original = fixture.call
            barrier = threading.Barrier(2)
            lock = threading.Lock()
            active = peak = 0

            def observed(name, args):
                nonlocal active, peak
                with lock:
                    active += 1
                    peak = max(peak, active)
                barrier.wait(timeout=3)
                value = original(name, args)
                with lock:
                    active -= 1
                return value

            fixture.call = observed
            execute(
                fixture,
                "native",
                calls(*[("read_file", {"path": "a.txt"}) for _ in range(6)]),
                2,
            )
            self.assertEqual(peak, 2)

    def test_transient_wait_is_separate_from_rate_limit_wait(self):
        row = evaluate(
            [ProviderFailure("transient", retry_after_s=2), {"answer": "31"}]
        )
        self.assertEqual(row["wall_s"], 4)
        self.assertEqual(row["retry_wait_s"], 2)
        self.assertEqual(row["rate_limit_wait_s"], 0)

    def test_failure_after_consumed_tokens_does_not_reset_row(self):
        row = evaluate([{"script": "pass\n"}, ProviderFailure("timeout", usage=None)])
        self.assertEqual(row["status"], "timeout")
        self.assertEqual(row["wall_s"], 2)
        self.assertEqual(row["known_tokens"], TOKENS)
        self.assertFalse(row["token_totals_complete"])
        self.assertEqual(len(row["provider_attempts"]), 2)

    def test_fixture_error_keeps_failure_row(self):
        with patch(
            "bench.protocol_v3.core.Fixture",
            side_effect=OSError("synthetic fixture failure"),
        ):
            row = evaluate([])
        self.assertEqual(row["status"], "fixture_error")
        self.assertFalse(row["passed"])
        self.assertEqual(summarize([row])["runs"], 1)

    def test_malformed_usage_and_decision_are_not_success(self):
        for decision in (None, {"unexpected": "invalid"}):
            row = evaluate(
                [Reply(decision, TOKENS, "fake-revision-001")],
                mode="native"
                if isinstance(decision, dict) and "calls" in decision
                else "weave",
            )
            self.assertEqual(row["status"], "malformed_response")
        bad = evaluate(
            [Reply({"answer": "31"}, {"input_tokens": 2}, "fake-revision-001")]
        )
        self.assertFalse(bad["passed"])
        self.assertFalse(bad["token_totals_complete"])

    def test_first_pass_does_not_hide_failed_script_repair(self):
        row = evaluate([{"script": "x=1/0\n"}, {"script": "pass\n"}, {"answer": "31"}])
        self.assertTrue(row["passed"])
        self.assertFalse(row["first_pass"])
        self.assertTrue(row["first_answer_pass"])
        self.assertEqual(row["execution_errors"], 1)

    def test_structured_list_result_bytes_equal_and_no_extra_parameter(self):
        a = evaluate([{"script": "x=list_files()\n"}, {"answer": "31"}])
        b = evaluate([calls(("list_files", {})), {"answer": "31"}], "native")
        self.assertEqual(a["feedback"], b["feedback"])
        self.assertEqual(
            a["feedback"][0]["observations"][0]["result"], ["a.txt", "b.txt"]
        )
        # Host closure names/cancellation metadata cannot become extra tool arguments.
        a = evaluate(
            [
                {
                    "script": 'try:\n    read_file(path="a.txt",extra="x")\nexcept ToolError:\n    pass\n'
                },
                {"answer": "31"},
            ]
        )
        b = evaluate(
            [calls(("read_file", {"path": "a.txt", "extra": "x"})), {"answer": "31"}],
            "native",
        )
        self.assertEqual(a["feedback"], b["feedback"])

    def test_legacy_live_entrypoint_fails_closed(self):
        from bench import real_tools_eval

        with patch.object(
            real_tools_eval.llm_eval,
            "openai_responses",
            side_effect=AssertionError("network attempted"),
        ):
            self.assertEqual(real_tools_eval.main(), 2)

    def test_invalid_invocation_feedback_and_repair_budget_equal(self):
        rows = []
        for mode, bad in [("weave", {"script": 3}), ("native", {"calls": "invalid"})]:
            row = evaluate([bad, {"answer": "31"}], mode)
            rows.append(row)
            self.assertTrue(row["passed"])
            self.assertFalse(row["first_pass"])
            self.assertEqual(row["execution_errors"], 1)
        self.assertEqual(rows[0]["feedback"], rows[1]["feedback"])

    def test_independent_read_result_context_is_canonical_both_modes(self):
        a = evaluate(
            [
                {"script": 'b=read_file(path="b.txt")\na=read_file(path="a.txt")\n'},
                {"answer": "31"},
            ]
        )
        b = evaluate(
            [
                calls(
                    ("read_file", {"path": "a.txt"}), ("read_file", {"path": "b.txt"})
                ),
                {"answer": "31"},
            ],
            "native",
        )
        self.assertTrue(a["passed"] and b["passed"])
        self.assertEqual(a["feedback"], b["feedback"])

    def test_backend_concurrency_cap_is_equal_for_weave_and_native(self):
        original = Fixture._call
        for mode in ("native", "weave"):
            barrier = threading.Barrier(2)
            lock = threading.Lock()
            active = peak = 0

            def observed(fixture, name, args):
                nonlocal active, peak
                with lock:
                    active += 1
                    peak = max(peak, active)
                barrier.wait(timeout=3)
                value = original(fixture, name, args)
                with lock:
                    active -= 1
                return value

            action = (
                calls(*[("read_file", {"path": "a.txt"}) for _ in range(6)])
                if mode == "native"
                else {
                    "script": "".join(
                        f'x{i}=read_file(path="a.txt")\n' for i in range(6)
                    )
                }
            )
            with patch.object(Fixture, "_call", observed):
                row = evaluate([action, {"answer": "31"}], mode, native_parallel=2)
            self.assertTrue(row["passed"])
            self.assertEqual(peak, 2)

    def test_negative_case_does_not_encode_weave_as_winner(self):
        bad = evaluate([{"script": "x=1/0\n"}, {"answer": "wrong"}], max_answers=1)
        native = evaluate([{"answer": "31"}], "native")
        self.assertFalse(bad["passed"])
        self.assertTrue(native["passed"])
        self.assertEqual(summarize([bad, native])["passed"], 1)

    def test_p95_includes_failed_slow_tail(self):
        rows = [evaluate([{"answer": "31"}]) for _ in range(19)]
        failed = evaluate(
            [ProviderFailure("rate_limit", retry_after_s=8), ProviderFailure("timeout")]
        )
        self.assertEqual(failed["wall_s"], 10)
        rows.extend([failed, failed])
        report = summarize(rows)
        self.assertEqual(report["runs"], 21)
        self.assertEqual(report["p95_all_wall_s"], 10)
        self.assertEqual(report["failures"], {"timeout": 2})


if __name__ == "__main__":
    unittest.main(verbosity=2)
