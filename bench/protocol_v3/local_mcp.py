"""Shipped WeaveTool + weave-mcp integration over owned local fixture servers."""

from concurrent.futures import Future
import inspect
import fcntl
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import threading
from types import SimpleNamespace

from .core import CallBudgetExceeded, canonical, digest, execute, normalized_feedback
from .fixture_server import contracts, git

ROOT = Path(__file__).resolve().parents[2]


def file_hash(path):
    import hashlib

    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


class OwnedMcpClient:
    def __init__(self, argv, env, stderr):
        self.proc = subprocess.Popen(
            argv,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=stderr,
            text=True,
            env=env,
            start_new_session=True,
        )
        self.lock = threading.Lock()
        self.pending = {}
        self.next_id = 0
        self.closed = False
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()
        try:
            self.request(
                "initialize",
                {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "v3-local-fixture", "version": "3.2"},
                },
            )
        except BaseException:
            self.close()
            raise

    def _read(self):
        try:
            for line in self.proc.stdout:
                reply = json.loads(line)
                with self.lock:
                    future = self.pending.pop(reply.get("id"), None)
                if future is not None:
                    future.set_result(reply)
        finally:
            with self.lock:
                for future in self.pending.values():
                    future.set_exception(RuntimeError("owned MCP process closed"))
                self.pending.clear()

    def request(self, method, params=None, *, timeout=5):
        future = Future()
        with self.lock:
            if self.closed:
                raise RuntimeError("owned MCP client closed")
            self.next_id += 1
            id = self.next_id
            self.pending[id] = future
            try:
                self.proc.stdin.write(
                    canonical(
                        {
                            "jsonrpc": "2.0",
                            "id": id,
                            "method": method,
                            "params": params or {},
                        }
                    )
                    + "\n"
                )
                self.proc.stdin.flush()
            except BaseException:
                self.pending.pop(id, None)
                raise
        try:
            reply = future.result(timeout=max(0.001, timeout))
        except TimeoutError:
            with self.lock:
                self.pending.pop(id, None)
                try:
                    self.proc.stdin.write(
                        canonical(
                            {
                                "jsonrpc": "2.0",
                                "method": "notifications/cancelled",
                                "params": {"requestId": id},
                            }
                        )
                        + "\n"
                    )
                    self.proc.stdin.flush()
                except (OSError, ValueError):
                    pass
            raise
        if "error" in reply:
            raise ValueError("MCP request failed")
        return reply["result"]

    def close(self):
        if self.closed:
            return
        self.closed = True
        try:
            self.proc.stdin.close()
            self.proc.wait(timeout=1)
        except (OSError, subprocess.TimeoutExpired):
            try:
                os.killpg(self.proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            self.proc.wait(timeout=3)
        # Kill any inherited owned descendants left after abnormal proxy exit.
        try:
            os.killpg(self.proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        self.reader.join(timeout=1)
        self.proc.stdout.close()


class McpEngine:
    """Only the Engine surface WeaveTool actually uses; fixed config, no overrides."""

    def __init__(self, fixture):
        self.fixture = fixture

    def check(self, source):
        result = self.fixture.client.request(
            "tools/call",
            {"name": "weave_check", "arguments": {"source": source}},
            timeout=self.fixture.remaining(),
        )
        return [result["content"][0]["text"]] if result.get("isError") else []

    def run(self, source):
        result = self.fixture.client.request(
            "tools/call",
            {"name": "weave_run", "arguments": {"source": source}},
            timeout=self.fixture.remaining(),
        )
        text = result["content"][0]["text"]
        if result.get("isError"):
            raise ValueError("MCP script execution failed")
        return SimpleNamespace(stdout=json.loads(text)["stdout"])


class RemoteFixture:
    def __init__(
        self,
        root,
        task,
        clock,
        deadline,
        width=4,
        *,
        path="weave_mcp",
        delay=0,
        binary=None,
    ):
        if path not in ("weave_tool", "weave_mcp"):
            raise ValueError("unknown shipped execution path")
        self.root = Path(root)
        self.data = self.root / "data"
        self.state = self.root / "state"
        self.data.mkdir()
        self.state.mkdir()
        self.task, self.clock, self.deadline, self.width, self.path = (
            task,
            clock,
            deadline,
            width,
            path,
        )
        self.lock = threading.Lock()
        self.events = []
        self._exceeded = False
        self.closed = False
        self.client = None
        self.stderr = None
        self.capacity = threading.BoundedSemaphore(width)
        self.tools = {t["name"]: t for t in contracts()}
        try:
            for name, text in task.files.items():
                target = self.data / name
                if (
                    not target.resolve().is_relative_to(self.data.resolve())
                    or ".git" in target.parts
                ):
                    raise ValueError("invalid fixture input")
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text(text)
            (self.state / "memory.json").write_text("{}")
            git(self.data, "init", "-q", "-b", "main")
            git(self.data, "add", ".")
            git(
                self.data,
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--no-gpg-sign",
                "-q",
                "-m",
                "fixture baseline",
            )
            binary = Path(binary or ROOT / "target/debug/weave-mcp")
            config = {
                "upstreams": [
                    {
                        "name": kind,
                        "command": sys.executable,
                        "args": [
                            "-m",
                            "bench.protocol_v3.fixture_server",
                            "--kind",
                            kind,
                            "--root",
                            str(self.data),
                            "--state",
                            str(self.state),
                            "--width",
                            str(width),
                            "--delay",
                            str(delay),
                        ],
                        "prefix": kind + "_",
                        # weave-mcp passes only an allowlisted environment by default.
                        "env": {
                            "PYTHONPATH": str(ROOT),
                            "PYTHONNOUSERSITE": "1",
                            "GIT_CONFIG_GLOBAL": os.devnull,
                            "XDG_CONFIG_HOME": str(self.root / "home"),
                        },
                        "stderr": {"file": str(self.root / "upstream-errors.log")},
                    }
                    for kind in ("fs", "git", "memory")
                ]
            }
            config_path = self.root / "proxy.json"
            config_path.write_text(canonical(config))
            home = self.root / "home"
            home.mkdir()
            env = {
                k: v
                for k, v in os.environ.items()
                if k in ("PATH", "LANG", "LC_ALL", "SYSTEMROOT")
            }
            env.update(
                {
                    "PYTHONPATH": str(ROOT),
                    "HOME": str(home),
                    "XDG_CONFIG_HOME": str(home),
                    "PYTHONNOUSERSITE": "1",
                    "GIT_CONFIG_GLOBAL": os.devnull,
                }
            )
            self.stderr = (self.root / "process-errors.log").open("w")
            self.client = OwnedMcpClient(
                [str(binary), "--config", str(config_path)], env, self.stderr
            )
            discovered = self.client.request("tools/list")["tools"]
            for name, tool in self.tools.items():
                actual = next(t for t in discovered if t["name"] == name)
                if (
                    actual["inputSchema"] != tool["inputSchema"]
                    or actual.get("outputSchema") != tool["outputSchema"]
                    or actual["description"] != tool["description"]
                    or actual["annotations"]["readOnlyHint"]
                    != (tool["effect"] == "read")
                ):
                    raise ValueError("discovered fixture contract drift")
            from weave import WeaveTool, declare

            functions = []

            def binding(name):
                def function(**kwargs):
                    event = self.call(name, kwargs)
                    if event["error"]:
                        raise ValueError(canonical(event["error"]))
                    return event["result"]

                function.__name__ = name
                function.__doc__ = self.tools[name]["description"]
                function.__signature__ = inspect.Signature(
                    [
                        inspect.Parameter(p, inspect.Parameter.KEYWORD_ONLY)
                        for p in self.tools[name]["inputSchema"]["properties"]
                    ]
                )
                return declare(effect=self.tools[name]["effect"])(function)

            for name in self.tools:
                functions.append(binding(name))
            self.weave_tool = WeaveTool(
                functions,
                engine=McpEngine(self) if path == "weave_mcp" else None,
                max_output_chars=1_000_000,
            )
            import weave._native
            import weave.agent_tool

            self.manifest_evidence = {
                "checkout_commit": subprocess.check_output(
                    ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
                ).strip(),
                "source_dirty": bool(
                    subprocess.check_output(
                        ["git", "status", "--porcelain"], cwd=ROOT, text=True
                    ).strip()
                ),
                "adapter_sources_hash": digest(
                    {
                        name: file_hash(Path(__file__).with_name(name))
                        for name in (
                            "core.py",
                            "responses.py",
                            "accounting.py",
                            "local_mcp.py",
                            "fixture_server.py",
                        )
                    }
                ),
                "weave_native_sha256": file_hash(weave._native.__file__),
                "weave_tool_sha256": file_hash(weave.agent_tool.__file__),
                "kind": "local_synthetic_mcp_fixture",
                "path": path,
                "fixture_server_sha256": file_hash(
                    Path(__file__).with_name("fixture_server.py")
                ),
                "proxy_binary_sha256": file_hash(binary),
                "contract_hash": digest(list(self.tools.values())),
                "initial_git_commit": git(self.data, "rev-parse", "HEAD"),
                "git_version": subprocess.check_output(
                    ["git", "--version"], text=True
                ).strip(),
                "output_schema_origin": "proxy discovery, verified equal to the pinned fixture contract",
                "global_backend_capacity": width,
            }
        except BaseException:
            self.close()
            raise

    @property
    def exceeded(self):
        return self._exceeded or any(
            e["event"] == "budget_refused" for e in self.logs()
        )

    @exceeded.setter
    def exceeded(self, value):
        self._exceeded = value

    def verify_pins(self, config):
        actual = {
            "fixture_server": self.manifest_evidence["fixture_server_sha256"],
            "weave_mcp": self.manifest_evidence["proxy_binary_sha256"],
            "weave_native": self.manifest_evidence["weave_native_sha256"],
            "weave_tool": self.manifest_evidence["weave_tool_sha256"],
        }
        if config.source_commit != self.manifest_evidence["checkout_commit"] or any(
            config.server_versions.get(k) != "sha256:" + v for k, v in actual.items()
        ):
            raise ValueError("fixture/runtime artifact pin mismatch")

    def remaining(self):
        return max(0.001, self.deadline - self.clock.now())

    def logs(self):
        path = self.state / "events.jsonl"
        if not path.exists():
            return []
        with path.open() as stream:
            fcntl.flock(stream, fcntl.LOCK_SH)
            return [json.loads(line) for line in stream if line.strip()]

    @property
    def writes(self):
        return [
            (e["name"] + ":" + e["arguments"]["path"], e["arguments"]["text"])
            for e in self.logs()
            if e["event"] == "end" and e["effect"] == "irreversible" and not e["error"]
        ]

    @property
    def write_attempts(self):
        return [
            {
                "path": e["name"] + ":" + e["arguments"].get("path", ""),
                "text": e["arguments"].get("text", ""),
            }
            for e in self.logs()
            if e["event"] == "write_attempt"
        ]

    def snapshot(self):
        files = {
            str(p.relative_to(self.data)): p.read_text()
            for p in sorted(self.data.rglob("*"))
            if p.is_file() and ".git" not in p.relative_to(self.data).parts
        }
        files["@git_head"] = git(self.data, "log", "-1", "--format=%s")
        files["@git_tracked"] = git(self.data, "ls-files")
        files["@memory"] = canonical(
            json.loads((self.state / "memory.json").read_text())
        )
        return files

    def call(self, name, args):
        with self.capacity:
            with self.lock:
                if len(self.events) >= 64:
                    self.exceeded = True
                    raise CallBudgetExceeded()
                event = {"name": name, "arguments": args, "result": None, "error": None}
                self.events.append(event)
            try:
                if self.clock.now() >= self.deadline:
                    raise TimeoutError()
                result = self.client.request(
                    "tools/call",
                    {"name": name, "arguments": args},
                    timeout=self.remaining(),
                )
                if result.get("isError"):
                    event["error"] = {
                        "kind": "ToolError",
                        "message": "tool request failed",
                    }
                else:
                    event["result"] = result["structuredContent"]
            except Exception:
                event["error"] = {"kind": "ToolError", "message": "tool request failed"}
            return event

    def quiesce(self):
        self.close()
        active = peak = 0
        for event in self.logs():
            if event["event"] == "begin":
                active += 1
                peak = max(peak, active)
            elif event["event"] == "end":
                active -= 1
        self.manifest_evidence["observed_peak_backend_calls"] = peak
        self.manifest_evidence["local_callbacks_still_unfinished_at_close"] = active

    def close(self):
        if self.closed:
            return
        self.closed = True
        if self.client:
            self.client.close()
        if self.stderr:
            self.stderr.close()


def execute_shipped(fixture, mode, decision, width):
    if mode == "native":
        return execute(fixture, mode, decision, width)
    before = len(fixture.logs())
    result = fixture.weave_tool.handle(
        {
            "type": "function_call",
            "name": "run_weave",
            "call_id": "local-execution",
            "arguments": canonical({"source": decision["script"]}),
        }
    )
    events = [
        {
            "name": e["name"],
            "arguments": e["arguments"],
            "result": e["result"],
            "error": e["error"],
        }
        for e in fixture.logs()[before:]
        if e["event"] == "end"
    ]
    # Underlying observations, not privileged script stdout, go to both modes.
    return normalized_feedback(
        fixture,
        events,
        "execution failed; inspect tool observations (no hidden grader values)"
        if result["output"].startswith("error:")
        else None,
    )
