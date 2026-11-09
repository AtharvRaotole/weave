"""Synthetic loopback Responses HTTP + actual owned local MCP subprocesses.

No live providers, external network, API credentials or unseen model evaluations.
"""

from contextlib import contextmanager
from functools import partial
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

from bench.protocol_v3.accounting import TrialLedger
from bench.protocol_v3.core import Task, canonical, run
from bench.protocol_v3.local_mcp import RemoteFixture, execute_shipped, file_hash, ROOT
from bench.protocol_v3.responses import ResponsesAdapter
from bench.protocol_v3.tests.test_protocol import config

MODEL = "synthetic-responses-r1"
SOURCE = 'x=fs_read(path="item.txt")\nfs_write(path="receipt.txt",text="RECEIVED "+x["value"])\ngit_commit(path="receipt.txt",text="Record receipt")\nmemory_put(path="Receipt",text="recorded blue")\nprint("recorded")\n'
WRITES = (
    ("fs_write:receipt.txt", "RECEIVED blue"),
    ("git_commit:receipt.txt", "Record receipt"),
    ("memory_put:Receipt", "recorded blue"),
)


def task():
    return Task(
        "dev_local_mcp",
        "development",
        "local_mcp_integration",
        "Read item.txt, write receipt.txt as RECEIVED plus a space and its content, commit receipt.txt with message Record receipt, add memory Receipt with observation recorded blue, then answer recorded.",
        {"item.txt": "blue"},
        "recorded",
        {
            "item.txt": "blue",
            "receipt.txt": "RECEIVED blue",
            "@git_head": "Record receipt",
            "@git_tracked": "item.txt\nreceipt.txt",
            "@memory": canonical({"Receipt": ["recorded blue"]}),
        },
        WRITES,
    )


def pinned_config(**changes):
    import weave._native
    import weave.agent_tool

    changes.setdefault(
        "source_commit",
        subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
        ).strip(),
    )
    return config(
        provider="openai-responses-synthetic",
        api_version="responses-v1",
        model_revision=MODEL,
        resolved_model_revision=MODEL,
        server_versions={
            "fixture_server": "sha256:"
            + file_hash(ROOT / "bench/protocol_v3/fixture_server.py"),
            "weave_mcp": "sha256:" + file_hash(ROOT / "target/debug/weave-mcp"),
            "weave_native": "sha256:" + file_hash(weave._native.__file__),
            "weave_tool": "sha256:" + file_hash(weave.agent_tool.__file__),
        },
        **changes,
    )


def usage(input=10, output=4):
    return {
        "input_tokens": input,
        "output_tokens": output,
        "total_tokens": input + output,
        "input_tokens_details": {"cached_tokens": 0},
        "output_tokens_details": {"reasoning_tokens": 2 if output >= 2 else 0},
    }


def call(name, args, id):
    return {
        "type": "function_call",
        "id": "fc_" + id,
        "call_id": id,
        "name": name,
        "arguments": canonical(args),
        "status": "completed",
    }


def response(output, *, tokens=True, model=MODEL):
    body = {
        "id": "resp_synthetic",
        "object": "response",
        "status": "completed",
        "model": model,
        "output": output,
    }
    if tokens is not None:
        body["usage"] = usage() if tokens is True else tokens
    return body


def answer(text="recorded", **kwargs):
    return response(
        [
            {
                "type": "message",
                "id": "msg_synthetic",
                "role": "assistant",
                "status": "completed",
                "content": [{"type": "output_text", "text": text, "annotations": []}],
            }
        ],
        **kwargs,
    )


def script(**kwargs):
    return response([call("run_weave", {"source": SOURCE}, "script_1")], **kwargs)


def native_actions():
    return response(
        [
            call("fs_read", {"path": "item.txt"}, "read_1"),
            call(
                "fs_write", {"path": "receipt.txt", "text": "RECEIVED blue"}, "write_1"
            ),
            call(
                "git_commit",
                {"path": "receipt.txt", "text": "Record receipt"},
                "commit_1",
            ),
            call("memory_put", {"path": "Receipt", "text": "recorded blue"}, "put_1"),
        ]
    )


@contextmanager
def http_fixture(replies, on_request=None):
    requests = []
    pending = list(replies)

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append(
                {
                    "path": self.path,
                    "body": body,
                    "authorization": self.headers.get("Authorization"),
                }
            )
            if on_request:
                on_request()
            entry = pending.pop(0) if pending else (500, {}, 0, {})
            if isinstance(entry, dict):
                entry = (200, entry, 0, {})
            code, payload, delay, headers = entry
            if delay:
                time.sleep(delay)
            raw = payload if isinstance(payload, bytes) else canonical(payload).encode()
            try:
                self.send_response(code)
                for k, v in headers.items():
                    self.send_header(k, v)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)
            except (BrokenPipeError, ConnectionResetError):
                pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}/v1/responses", requests
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)


def trial(
    endpoint,
    *,
    mode="weave",
    path="weave_mcp",
    ledger=None,
    trial_id=None,
    cfg=None,
    **fixture_kwargs,
):
    provider = ResponsesAdapter(endpoint, api_key="SYNTHETIC_NOT_A_SECRET")
    row = run(
        task(),
        mode,
        provider,
        cfg or pinned_config(),
        fixture_factory=partial(RemoteFixture, path=path, **fixture_kwargs),
        execution=execute_shipped,
        ledger=ledger,
        trial_id=trial_id,
    )
    return row, provider


class AdapterTests(unittest.TestCase):
    def test_shipped_weave_tool_and_mcp_and_native_write_real_state(self):
        rows = []
        for mode, path, replies in [
            ("weave", "weave_tool", [script(), answer()]),
            ("weave", "weave_mcp", [script(), answer()]),
            ("native", "weave_mcp", [native_actions(), answer()]),
        ]:
            with http_fixture(replies) as (endpoint, requests):
                row, p = trial(endpoint, mode=mode, path=path)
            self.assertTrue(row["passed"], row["status"])
            self.assertEqual(row["writes"], list(WRITES))
            self.assertTrue(row["state_matches"])
            self.assertFalse(Path(row["fixture_root"]).exists())
            self.assertEqual(
                row["known_tokens"],
                {"input_tokens": 20, "output_tokens": 8, "cached_input_tokens": 0},
            )
            self.assertEqual(
                row["provider_attempts"][0]["usage_details"]["output_tokens_details"][
                    "reasoning_tokens"
                ],
                2,
            )
            self.assertEqual(row["fixture_manifest"]["path"], path)
            self.assertEqual(len(requests), 2)
            rows.append(row)
        self.assertEqual(len({r["initial_state_hash"] for r in rows}), 1)
        self.assertEqual(len({r["final_state_hash"] for r in rows}), 1)
        self.assertEqual(len({r["schema_hash"] for r in rows}), 1)

    def test_equivalent_information_wire_ids_and_shipped_definition(self):
        observed = []
        for mode, responses in [
            ("weave", [script(), answer()]),
            ("native", [native_actions(), answer()]),
        ]:
            with http_fixture(responses) as (endpoint, requests):
                row, p = trial(endpoint, mode=mode)
            observed.append(requests)
            self.assertTrue(row["passed"], row["status"])
            first = requests[0]["body"]
            second = requests[1]["body"]
            self.assertTrue(first["parallel_tool_calls"])
            self.assertFalse(first["store"])
            self.assertNotIn("seed", first)
            self.assertNotIn("previous_response_id", second)
            self.assertTrue(all(r["authorization"] is None for r in requests))
            self.assertEqual(
                {
                    x["call_id"]
                    for x in second["input"]
                    if x.get("type") == "function_call_output"
                },
                {"script_1"}
                if mode == "weave"
                else {"read_1", "write_1", "commit_1", "put_1"},
            )
            self.assertNotIn("SYNTHETIC_NOT_A_SECRET", canonical(row))
            self.assertIn("seed", row["provider_mapping"])
        self.assertEqual(
            observed[0][0]["body"]["input"], observed[1][0]["body"]["input"]
        )
        self.assertEqual(
            observed[0][0]["body"]["instructions"],
            observed[1][0]["body"]["instructions"],
        )
        self.assertEqual(observed[0][0]["body"]["tools"][0]["name"], "run_weave")
        left = [
            json.loads(x["output"])
            for x in observed[0][1]["body"]["input"]
            if x.get("type") == "function_call_output"
        ][0]["observations"]
        right = [
            json.loads(x["output"])
            for x in observed[1][1]["body"]["input"]
            if x.get("type") == "function_call_output"
        ]
        self.assertEqual(left, right)

    def test_provider_final_answer_not_script_stdout(self):
        with http_fixture([script(), answer("wrong")]) as (endpoint, _):
            row, _ = trial(endpoint, cfg=pinned_config(max_answers=1))
        self.assertFalse(row["passed"])
        self.assertTrue(row["state_matches"])
        self.assertEqual(row["status"], "wrong_answer_or_state")

    def test_claimed_answer_without_real_writes_fails(self):
        with http_fixture([answer()]) as (endpoint, _):
            row, _ = trial(endpoint, mode="native", cfg=pinned_config(max_answers=1))
        self.assertFalse(row["passed"])
        self.assertFalse(row["state_matches"])
        self.assertEqual(row["writes"], [])

    def test_retry_after_mutations_does_not_repeat_tools_and_includes_wait(self):
        with http_fixture(
            [
                script(),
                (429, {"usage": usage(0, 0)}, 0, {"Retry-After": ".03"}),
                answer(),
            ]
        ) as (endpoint, requests):
            row, _ = trial(endpoint)
        self.assertTrue(row["passed"], row["status"])
        self.assertEqual(row["writes"], list(WRITES))
        self.assertEqual(len(row["provider_attempts"]), 3)
        self.assertGreaterEqual(row["wall_s"], row["rate_limit_wait_s"])
        self.assertEqual(row["rate_limit_wait_s"], 0.03)
        self.assertEqual(requests[1]["body"], requests[2]["body"])
        self.assertEqual(row["unknown_usage_attempts"], 0)
        self.assertEqual(row["known_tokens"]["input_tokens"], 20)

    def test_missing_usage_is_not_reported_as_zero(self):
        with tempfile.TemporaryDirectory() as tmp:
            ledger = TrialLedger(Path(tmp) / "ledger.jsonl")
            with http_fixture([script(tokens=usage(0, 0)), answer(tokens=None)]) as (
                endpoint,
                _,
            ):
                row, _ = trial(endpoint, ledger=ledger, trial_id="missing")
            self.assertTrue(row["passed"], row["status"])
            self.assertEqual(row["known_tokens"]["input_tokens"], 0)
            recovered = ledger.trials()[0]
            self.assertEqual(recovered["unknown_usage_attempts"], 1)
            self.assertFalse(recovered["token_totals_complete"])
            ends = [
                e["payload"] for e in ledger.records()[0] if e["event"] == "attempt_end"
            ]
            self.assertEqual(ends[0]["usage"]["input_tokens"], 0)
            self.assertIsNone(ends[1]["usage"])

    def test_attempt_durable_before_http_and_timeout_retains_completed_writes(self):
        with tempfile.TemporaryDirectory() as tmp:
            ledger = TrialLedger(Path(tmp) / "ledger.jsonl")
            seen = []

            def check():
                seen.append(ledger.records()[0][-1]["event"])

            with http_fixture([script(), (200, answer(), 1, {})], check) as (
                endpoint,
                _,
            ):
                row, _ = trial(
                    endpoint,
                    ledger=ledger,
                    trial_id="timeout",
                    cfg=pinned_config(timeout_s=0.7),
                )
            self.assertEqual(row["status"], "timeout")
            self.assertTrue(row["state_matches"])
            self.assertEqual(row["writes"], list(WRITES))
            self.assertEqual(seen, ["http_begin", "http_begin"])
            saved = ledger.trials()[0]
            self.assertEqual(saved["status"], "timeout")
            self.assertEqual(saved["known_tokens"]["input_tokens"], 10)
            self.assertEqual(saved["unknown_usage_attempts"], 1)

    def test_native_parallel_and_both_shipped_paths_share_capacity(self):
        source = (
            'a=fs_read(path="item.txt")\nb=git_head()\nc=memory_read(path="Receipt")\n'
        )
        native = response(
            [
                call("fs_read", {"path": "item.txt"}, "a"),
                call("git_head", {}, "b"),
                call("memory_read", {"path": "Receipt"}, "c"),
            ]
        )
        for mode, path, first in [
            ("native", "weave_mcp", native),
            (
                "weave",
                "weave_tool",
                response([call("run_weave", {"source": source}, "s")]),
            ),
            (
                "weave",
                "weave_mcp",
                response([call("run_weave", {"source": source}, "s")]),
            ),
        ]:
            captured = []

            def factory(*args, **kwargs):
                f = RemoteFixture(*args, path=path, delay=0.05, **kwargs)
                captured.append(f)
                return f

            with http_fixture([first, answer("wrong")]) as (endpoint, _):
                p = ResponsesAdapter(endpoint)
                row = run(
                    task(),
                    mode,
                    p,
                    pinned_config(native_parallel=2, max_answers=1),
                    fixture_factory=factory,
                    execution=execute_shipped,
                )
            # Endpoints report all three results; pinned fixture enforces 2 slots
            # across all three actual upstream subprocesses, not just the client.
            self.assertEqual(len(row["feedback"][0]["observations"]), 3)
            self.assertEqual(row["fixture_manifest"]["global_backend_capacity"], 2)
            self.assertEqual(row["fixture_manifest"]["observed_peak_backend_calls"], 2)
            self.assertEqual(
                row["fixture_manifest"]["local_callbacks_still_unfinished_at_close"], 0
            )
            self.assertTrue(captured[0].closed)

    def test_model_pin_drift_stops_before_tool_execution(self):
        with http_fixture([script(model="drift")]) as (endpoint, _):
            row, _ = trial(endpoint)
        self.assertEqual(row["status"], "provider_error")
        self.assertEqual(row["writes"], [])
        self.assertEqual(row["known_tokens"]["input_tokens"], 10)

    def test_binary_pin_drift_stops_before_http(self):
        cfg = pinned_config()
        cfg.server_versions["weave_mcp"] = "sha256:" + "0" * 64
        with http_fixture([]) as (endpoint, requests):
            row, _ = trial(endpoint, cfg=cfg)
        self.assertFalse(row["passed"])
        self.assertEqual(requests, [])

    def test_redirect_and_malformed_incomplete_http_do_not_become_success(self):
        for entry in [
            (302, {}, 0, {"Location": "https://api.openai.com/v1/responses"}),
            (200, b"not-json", 0, {}),
            {**script(), "status": "incomplete"},
        ]:
            with http_fixture([entry]) as (endpoint, requests):
                row, _ = trial(endpoint)
            self.assertEqual(len(requests), 1)
            self.assertEqual(row["status"], "provider_error")
            self.assertEqual(row["writes"], [])

    def test_explicit_dual_live_opt_in_and_finite_request_cap(self):
        for endpoint in [
            "https://api.openai.com/v1/responses",
            "http://localhost:12/v1/responses",
            "http://127.0.0.1.evil.test:1/v1/responses",
        ]:
            with self.assertRaises(ValueError):
                ResponsesAdapter(endpoint)
        p = ResponsesAdapter(
            "https://api.openai.com/v1/responses",
            live_opt_in=True,
            api_key="SYNTHETIC_NOT_A_SECRET",
            max_live_requests=1,
        )
        with self.assertRaisesRegex(ValueError, "explicit runner opt-in"):
            run(task(), "weave", p, pinned_config())
        # Exhaustion is checked before transport: this never accesses the network.
        p.sent = 1
        with patch.object(
            p.opener, "open", side_effect=AssertionError("unexpected live request")
        ):
            from bench.protocol_v3.core import ProviderFailure

            with self.assertRaises(ProviderFailure) as error:
                p.request({}, time.monotonic() + 1)
        self.assertEqual(error.exception.kind, "budget")
        self.assertEqual(error.exception.usage["input_tokens"], 0)

    def test_reasoning_and_function_call_items_are_preserved(self):
        first = script()
        first["output"].insert(
            0, {"type": "reasoning", "id": "rs_synthetic", "summary": []}
        )
        with http_fixture([first, answer()]) as (endpoint, requests):
            row, _ = trial(endpoint)
        self.assertTrue(row["passed"], row["status"])
        self.assertTrue(
            any(i.get("id") == "rs_synthetic" for i in requests[1]["body"]["input"])
        )

    def test_error_feedback_and_shared_call_limit_through_mcp(self):
        source = 'try:\n    x=fs_read(path="missing.txt")\nexcept ToolError:\n    x=fs_read(path="item.txt")\n'
        feedback = []
        native = response(
            [
                call("fs_read", {"path": "missing.txt"}, "missing"),
                call("fs_read", {"path": "item.txt"}, "fallback"),
            ]
        )
        for mode, first in [
            ("weave", response([call("run_weave", {"source": source}, "script")])),
            ("native", native),
        ]:
            with http_fixture([first, answer("wrong")]) as (endpoint, _):
                row, _ = trial(endpoint, mode=mode, cfg=pinned_config(max_answers=1))
            feedback.append(row["feedback"][0]["observations"])
        self.assertEqual(feedback[0], feedback[1])
        source = 'for i in range(65):\n    x=fs_read(path="item.txt")\n'
        with http_fixture(
            [response([call("run_weave", {"source": source}, "limit")])]
        ) as (endpoint, requests):
            row, _ = trial(endpoint)
        self.assertEqual(row["status"], "resource_limit")
        self.assertEqual(len(requests), 1)

    def test_several_scripts_in_one_turn_get_repair_feedback_not_termination(self):
        two = response(
            [
                call("run_weave", {"source": 'print("a")\n'}, "s1"),
                call("run_weave", {"source": 'print("b")\n'}, "s2"),
            ]
        )
        with http_fixture([two, script(), answer()]) as (endpoint, requests):
            row, _ = trial(endpoint, mode="weave")
        self.assertTrue(row["passed"], row["status"])
        self.assertEqual(row["execution_errors"], 1)
        self.assertFalse(row["first_pass"])
        self.assertEqual(row["model_turns"], 3)
        outputs = [
            x
            for x in requests[1]["body"]["input"]
            if x.get("type") == "function_call_output"
        ]
        self.assertEqual({o["call_id"] for o in outputs}, {"s1", "s2"})
        self.assertEqual(len({o["output"] for o in outputs}), 1)
        self.assertIn("execution failed", outputs[0]["output"])

    def test_unsent_generation_fields_and_request_timeout_bounds(self):
        cfg = pinned_config(
            generation={"temperature": None, "max_output_tokens": 64, "reasoning": "low"}
        )
        with http_fixture([script(), answer()]) as (endpoint, requests):
            row, _ = trial(endpoint, cfg=cfg)
        self.assertTrue(row["passed"], row["status"])
        body = requests[0]["body"]
        self.assertNotIn("temperature", body)
        self.assertEqual(body["reasoning"], {"effort": "low"})
        cfg = pinned_config(
            generation={"temperature": 0, "max_output_tokens": 64, "reasoning": None}
        )
        with http_fixture([script(), answer()]) as (endpoint, requests):
            trial(endpoint, cfg=cfg)
        self.assertNotIn("reasoning", requests[0]["body"])
        self.assertEqual(requests[0]["body"]["temperature"], 0)
        for bad in (0, -1, 601):
            with self.assertRaises(ValueError):
                ResponsesAdapter("http://127.0.0.1:9/v1/responses", request_timeout_s=bad)

    def test_malformed_success_body_keeps_known_usage(self):
        body = response(["not-an-output-item"])
        with tempfile.TemporaryDirectory() as tmp:
            ledger = TrialLedger(Path(tmp) / "ledger.jsonl")
            with http_fixture([body]) as (endpoint, _):
                row, _ = trial(endpoint, ledger=ledger, trial_id="malformed")
            self.assertEqual(row["status"], "provider_error")
            self.assertEqual(row["known_tokens"]["input_tokens"], 10)
            self.assertEqual(ledger.trials()[0]["known_tokens"]["input_tokens"], 10)


class LedgerTests(unittest.TestCase):
    def test_abrupt_crash_retains_started_row_and_missing_usage(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "ledger.jsonl"
            code = 'from bench.protocol_v3.accounting import TrialLedger; import os,sys; j=TrialLedger(sys.argv[1]); j.begin("crash",{}); j.append("crash","attempt_begin",{"attempt_index":0}); os._exit(17)'
            child = subprocess.run([sys.executable, "-c", code, str(path)], cwd=ROOT)
            self.assertEqual(child.returncode, 17)
            ledger = TrialLedger(path)
            before = path.read_bytes()
            self.assertEqual(
                ledger.trials(recover_incomplete=True)[0]["status"], "interrupted"
            )
            ledger.terminated("crash", "crashed", exit_code=child.returncode)
            row = ledger.trials()[0]
            self.assertEqual(row["status"], "crashed")
            self.assertEqual(row["unknown_usage_attempts"], 1)
            self.assertTrue(path.read_bytes().startswith(before))
            self.assertEqual(ledger.records()[1], [])

    def test_interrupted_core_records_terminal_and_retains_attempt(self):
        from bench.protocol_v3.fakes import ScriptedProvider, ManualClock
        from bench.protocol_v3.tasks import DEVELOPMENT

        with tempfile.TemporaryDirectory() as tmp:
            ledger = TrialLedger(Path(tmp) / "ledger.jsonl")
            clock = ManualClock()

            class Interrupted(ScriptedProvider):
                def request(self, *args):
                    raise KeyboardInterrupt()

            with self.assertRaises(KeyboardInterrupt):
                run(
                    DEVELOPMENT[0],
                    "weave",
                    Interrupted([], clock),
                    config(),
                    clock=clock,
                    ledger=ledger,
                    trial_id="interrupted",
                )
            row = ledger.trials()[0]
            self.assertEqual(row["status"], "interrupted")
            self.assertTrue(row["terminal_recorded"])
            self.assertEqual(row["unknown_usage_attempts"], 1)

    def test_truncated_tail_preserved_and_recovery_append_valid(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "ledger.jsonl"
            ledger = TrialLedger(path)
            ledger.begin("partial", {})
            with path.open("ab") as f:
                f.write(b'{"partial":')
            before = path.read_bytes()
            ledger.terminated("partial", "interrupted")
            self.assertTrue(path.read_bytes().startswith(before))
            self.assertTrue(ledger.records()[1])
            self.assertEqual(ledger.trials()[0]["status"], "interrupted")
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_concurrent_append_chains_and_duplicate_ids(self):
        from concurrent.futures import ThreadPoolExecutor

        with tempfile.TemporaryDirectory() as tmp:
            ledger = TrialLedger(Path(tmp) / "ledger.jsonl")

            def work(i):
                ledger.begin(str(i), {})
                ledger.terminated(str(i), "interrupted")

            with ThreadPoolExecutor(max_workers=4) as pool:
                list(pool.map(work, range(8)))
            self.assertEqual(len(ledger.trials()), 8)
            self.assertEqual(ledger.records()[1], [])
            with self.assertRaises(ValueError):
                ledger.begin("0", {})

    def test_known_response_usage_survives_missing_attempt_end(self):
        with tempfile.TemporaryDirectory() as tmp:
            ledger = TrialLedger(Path(tmp) / "ledger.jsonl")
            ledger.begin("response-crash", {"task": "synthetic", "mode": "native"})
            ledger.append("response-crash", "attempt_begin", {"attempt_index": 0})
            ledger.append(
                "response-crash",
                "http_response",
                {
                    "attempt_index": 0,
                    "usage": {
                        "input_tokens": 0,
                        "output_tokens": 0,
                        "cached_input_tokens": 0,
                    },
                },
            )
            row = ledger.trials(recover_incomplete=True)[0]
            self.assertTrue(row["token_totals_complete"])
            self.assertEqual(row["unknown_usage_attempts"], 0)
            self.assertIsNone(row["wall_s"])
            self.assertFalse(row["wall_time_complete"])
            self.assertEqual(row["task"], "synthetic")

    def test_signal_interruption_is_recovered_without_rewriting_history(self):
        import os
        import signal
        import select

        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "ledger.jsonl"
            code = 'from bench.protocol_v3.accounting import TrialLedger; import sys,time; j=TrialLedger(sys.argv[1]); j.begin("signal",{}); j.append("signal","attempt_begin",{"attempt_index":0}); print("ready",flush=True); time.sleep(60)'
            child = subprocess.Popen(
                [sys.executable, "-c", code, str(path)],
                cwd=ROOT,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                text=True,
            )
            try:
                self.assertTrue(select.select([child.stdout], [], [], 3)[0])
                self.assertEqual(child.stdout.readline().strip(), "ready")
                before = path.read_bytes()
                os.kill(child.pid, signal.SIGINT)
                child.wait(timeout=3)
                ledger = TrialLedger(path)
                self.assertEqual(
                    ledger.trials(recover_incomplete=True)[0]["status"], "interrupted"
                )
                ledger.terminated("signal", "interrupted", exit_code=child.returncode)
                self.assertTrue(path.read_bytes().startswith(before))
                self.assertEqual(ledger.trials()[0]["unknown_usage_attempts"], 1)
                with self.assertRaises(ValueError):
                    ledger.terminated("signal", "crashed")
            finally:
                if child.poll() is None:
                    child.kill()
                    child.wait()
                child.stdout.close()


if __name__ == "__main__":
    unittest.main(verbosity=2)
