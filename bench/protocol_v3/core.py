"""Versioned controlled comparison; deliberately no live/network implementation.

Both modes use the same public tool contract and complete at a final provider
answer validated against private answer AND actual state/write-log oracles.
"""

from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor
from dataclasses import asdict, dataclass, field
import hashlib
import json
import keyword
from pathlib import Path
import re
import tempfile
import threading
import time
from typing import Protocol


def canonical(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def digest(value):
    return hashlib.sha256(canonical(value).encode()).hexdigest()


class Clock:
    def now(self):
        return time.monotonic()

    def sleep(self, seconds):
        time.sleep(seconds)


@dataclass(frozen=True)
class Config:
    provider: str
    model_revision: str
    api_version: str
    source_commit: str
    server_versions: dict
    generation: dict
    # Adapters must resolve aliases, report the returned revision, and disable
    # hidden SDK retries. These fields are declarations, not remote attestation.
    resolved_model_revision: str
    native_parallel: int = 4
    max_turns: int = 10
    max_answers: int = 3
    provider_retries: int = 2
    timeout_s: float = 30
    seed: int = 73
    cache_policy: str = "disabled"

    def __post_init__(self):
        if not re.fullmatch(r"[0-9a-f]{40}", self.source_commit):
            raise ValueError("full source commit required")
        if not self.provider or not self.api_version or not self.model_revision:
            raise ValueError("provider/model/API pins required")
        if self.model_revision != self.resolved_model_revision or any(
            x in self.model_revision.lower() for x in ("latest", "alias", "unresolved")
        ):
            raise ValueError(
                "resolved immutable model revision required (declared, not attested)"
            )
        if not self.server_versions or any(
            not v or v in ("latest", "*", "unpinned")
            for v in self.server_versions.values()
        ):
            raise ValueError("server pins required")
        if not {
            "temperature",
            "max_output_tokens",
            "reasoning",
        } <= self.generation.keys() or set(self.generation) - {
            "temperature",
            "max_output_tokens",
            "reasoning",
            "top_p",
        }:
            raise ValueError(
                "generation config incomplete or contains unsupported/private fields"
            )
        if (
            not 1 <= self.native_parallel <= 16
            or min(self.max_turns, self.max_answers) < 1
            or self.provider_retries < 0
            or self.timeout_s <= 0
        ):
            raise ValueError("invalid limits")
        if self.cache_policy != "disabled":
            raise ValueError("controlled protocol requires caching disabled")

    def report(self):
        # Whitelist dataclass fields only; never dump environment, credentials or headers.
        return asdict(self)


@dataclass(frozen=True)
class Task:
    id: str
    split: str
    family: str
    instruction: str
    files: dict[str, str]
    answer: str
    final_files: dict[str, str]
    expected_writes: tuple[tuple[str, str], ...] = ()
    aliases: dict = field(
        default_factory=lambda: {
            "read_file": "read_file",
            "write_file": "write_file",
            "list_files": "list_files",
        }
    )

    def public(self):
        return {"task_id": self.id, "instruction": self.instruction}

    def grade(self, answer, files, writes):
        return (
            answer.strip() == self.answer
            and files == self.final_files
            and writes == list(self.expected_writes)
        )


def manifest(task):
    aliases = task.aliases
    if (
        set(aliases) != {"read_file", "write_file", "list_files"}
        or len(set(aliases.values())) != 3
    ):
        raise ValueError("tool alias collision or incomplete mapping")
    if any(
        not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name)
        or keyword.iskeyword(name)
        or name in ("print", "len", "range")
        for name in aliases.values()
    ):
        raise ValueError("invalid tool alias")
    schemas = {
        "read_file": {"path": {"type": "string"}},
        "write_file": {"path": {"type": "string"}, "text": {"type": "string"}},
        "list_files": {},
    }
    return [
        {
            "name": task.aliases[n],
            "description": {
                "read_file": "Read a UTF-8 file.",
                "write_file": "Replace a UTF-8 file (irreversible, never auto-retried).",
                "list_files": "List all file paths.",
            }[n],
            "effect": "irreversible" if n == "write_file" else "read",
            "inputSchema": {
                "type": "object",
                "properties": props,
                "required": list(props),
                "additionalProperties": False,
            },
            "outputSchema": {
                "read_file": {"type": "string"},
                "write_file": {"type": "null"},
                "list_files": {"type": "array", "items": {"type": "string"}},
            }[n],
        }
        for n, props in schemas.items()
    ]


class Usage:
    @staticmethod
    def validate(usage):
        # Unknown is not zero. Cached tokens are a subset of input, not additive.
        if usage is None:
            return None
        required = ("input_tokens", "output_tokens", "cached_input_tokens")
        if set(usage) != set(required) or any(
            type(usage[k]) is not int or usage[k] < 0 for k in required
        ):
            raise ValueError("invalid usage")
        if usage["cached_input_tokens"] > usage["input_tokens"]:
            raise ValueError("invalid cache accounting")
        return usage


@dataclass
class Reply:
    decision: dict
    usage: dict | None
    model_revision: str


class ProviderFailure(Exception):
    def __init__(self, kind, *, usage=None, retry_after_s=0):
        super().__init__(kind)
        self.kind, self.usage, self.retry_after_s = kind, usage, retry_after_s


class Provider(Protocol):
    offline: bool

    def request(self, request: dict, deadline: float) -> Reply: ...


class CallBudgetExceeded(Exception):
    pass


class Fixture:
    def __init__(self, root, task, clock, deadline, width=4):
        self.root, self.task, self.clock, self.deadline = root, task, clock, deadline
        self.events, self.writes, self.lock = [], [], threading.Lock()
        self.capacity = threading.BoundedSemaphore(width)
        self.exceeded = False
        self.tools = {t["name"]: t for t in manifest(task)}
        self.reverse = {v: k for k, v in task.aliases.items()}
        for path, text in task.files.items():
            self.path(path).parent.mkdir(parents=True, exist_ok=True)
            self.path(path).write_text(text)

    def path(self, value):
        if (
            not isinstance(value, str)
            or not value
            or Path(value).is_absolute()
            or ".." in Path(value).parts
        ):
            raise ValueError("path must be fixture-relative without traversal")
        path = self.root / value
        if not path.resolve().is_relative_to(self.root.resolve()):
            raise ValueError("path escapes fixture")
        return path

    def snapshot(self):
        return {
            str(p.relative_to(self.root)): p.read_text()
            for p in sorted(self.root.rglob("*"))
            if p.is_file()
        }

    def call(self, name, args):
        with self.capacity:
            return self._call(name, args)

    def _call(self, name, args):
        with self.lock:
            if len(self.events) >= 64:
                self.exceeded = True
                raise CallBudgetExceeded("64-call budget exceeded")
            event = {"name": name, "arguments": args, "result": None, "error": None}
            self.events.append(event)
        try:
            if self.clock.now() >= self.deadline:
                raise TimeoutError("deadline exceeded before tool dispatch")
            tool = self.tools[name]
            props = tool["inputSchema"]["properties"]
            if (
                not isinstance(args, dict)
                or set(args) != set(props)
                or any(not isinstance(v, str) for v in args.values())
            ):
                raise ValueError("arguments do not match canonical schema")
            operation = self.reverse[name]
            if operation == "read_file":
                value = self.path(args["path"]).read_text()
            elif operation == "list_files":
                value = sorted(self.snapshot())
            else:
                path = self.path(args["path"])
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(args["text"])
                self.writes.append((args["path"], args["text"]))
                value = None
            event["result"] = value
        except Exception as exc:
            # Identical backend errors; no temp-root paths or host traceback in context.
            event["error"] = {
                "kind": type(exc).__name__,
                "message": "tool request failed",
            }
        return event


def execute(fixture, mode, decision, width):
    before = len(fixture.events)
    error = None
    if mode == "native":
        calls = decision.get("calls")
        if not isinstance(calls, list) or len(calls) > 64:
            raise ValueError("native calls must be a list of at most 64")
        # Preserve barriers around writes; only independent literal read calls
        # from the same provider turn may overlap. Result order is request order.
        group = []
        with ThreadPoolExecutor(max_workers=width) as pool:

            def drain():
                for future in group:
                    future.result()
                group.clear()

            for call in calls:
                if set(call) != {"name", "arguments"}:
                    raise ValueError("malformed native call")
                if fixture.tools.get(call["name"], {}).get("effect") == "read":
                    group.append(
                        pool.submit(fixture.call, call["name"], call["arguments"])
                    )
                else:
                    drain()
                    fixture.call(call["name"], call["arguments"])
            drain()
    else:
        import weave

        engine = weave.Engine()
        engine.set_limits(
            max_wall_time_ms=max(
                1, int(1000 * (fixture.deadline - fixture.clock.now()))
            )
        )

        def registered(name):
            def function(*, cancel=None, **kwargs):
                event = fixture.call(name, kwargs)
                if event["error"]:
                    raise ValueError(canonical(event["error"]))
                return event["result"]

            return function

        for tool in fixture.tools.values():
            engine.tool(tool["name"], registered(tool["name"]), effect=tool["effect"])
        try:
            engine.run(decision["script"])
        except Exception:
            error = (
                "execution failed; inspect tool observations (no hidden grader values)"
            )
    # Native futures can start out of order. Sort this turn by declared invocation
    # position for stable feedback; scripts use recorded dispatch order.
    events = fixture.events[before:]
    if mode == "native":
        remaining = list(events)
        ordered = []
        for call in decision["calls"]:
            i = next(
                i
                for i, e in enumerate(remaining)
                if e["name"] == call["name"] and e["arguments"] == call["arguments"]
            )
            ordered.append(remaining.pop(i))
        events = ordered
    # Canonicalize independent read segments in BOTH modes, preserving write
    # barriers and duplicate observations. No mode-specific result compression.
    stable, reads = [], []
    for event in events:
        if fixture.tools.get(event["name"], {}).get("effect") == "read":
            reads.append(event)
        else:
            stable.extend(sorted(reads, key=canonical))
            reads.clear()
            stable.append(event)
    stable.extend(sorted(reads, key=canonical))
    return {"observations": stable, "execution_error": error}


COMMON = "Complete the task using the canonical tools, then submit a final answer. Tool calls use keyword arguments. The grader never supplies expected answers or expected writes. No demonstrations."


def run(task: Task, mode: str, provider: Provider, config: Config, *, clock=None):
    if mode not in ("weave", "native"):
        raise ValueError("unknown mode")
    if not provider.offline:
        raise ValueError("live adapters are not enabled: corrected live runs pending")
    clock = clock or Clock()
    started = clock.now()
    deadline = started + config.timeout_s
    history, requests, attempts, feedback = [], [], [], []
    answers = turns = execution_errors = 0
    first_pass = first_answer_pass = False
    status = "turn_limit"
    unknown = 0
    counts = {"input_tokens": 0, "output_tokens": 0, "cached_input_tokens": 0}
    base = {"system": COMMON, "task": task.public(), "tools": manifest(task)}
    with tempfile.TemporaryDirectory(prefix="weave-protocol-v3-") as tmp:
        fixture = None
        initial_hash = digest(task.files)
        try:
            fixture = Fixture(Path(tmp), task, clock, deadline, config.native_parallel)
            initial_hash = digest(fixture.snapshot())
            while turns < config.max_turns:
                if clock.now() >= deadline:
                    status = "timeout"
                    break
                request = {
                    **base,
                    "mode": mode,
                    "interface": "script or answer"
                    if mode == "weave"
                    else "calls or answer",
                    "history": json.loads(canonical(history)),
                    "generation": config.generation,
                }
                requests.append(json.loads(canonical(request)))
                turns += 1
                reply = None
                for retry in range(config.provider_retries + 1):
                    if clock.now() >= deadline:
                        status = "timeout"
                        break
                    t0 = clock.now()
                    attempt = {
                        "turn": turns,
                        "retry": retry,
                        "request_hash": digest(request),
                        "wait_s": 0,
                        "usage": None,
                    }
                    attempts.append(attempt)
                    try:
                        reply = provider.request(
                            json.loads(canonical(request)), deadline
                        )
                        attempt["usage"] = Usage.validate(reply.usage)
                        attempt["outcome"] = "ok"
                        if reply.model_revision != config.model_revision:
                            raise ProviderFailure(
                                "model_revision_mismatch", usage=reply.usage
                            )
                    except ProviderFailure as exc:
                        attempt["usage"] = Usage.validate(exc.usage)
                        attempt["outcome"] = exc.kind
                        if (
                            exc.kind in ("rate_limit", "transient")
                            and retry < config.provider_retries
                        ):
                            wait = min(
                                max(0, exc.retry_after_s),
                                max(0, deadline - clock.now()),
                            )
                            attempt["wait_s"] = wait
                            clock.sleep(wait)
                        else:
                            status = {
                                "timeout": "timeout",
                                "budget": "budget_exhausted",
                            }.get(exc.kind, "provider_error")
                        reply = None
                    except TimeoutError:
                        attempt["outcome"] = "timeout"
                        status = "timeout"
                        reply = None
                    except Exception:
                        attempt["outcome"] = "adapter_error"
                        status = "provider_error"
                        reply = None
                    finally:
                        attempt["elapsed_s"] = clock.now() - t0
                        if attempt["usage"] is None:
                            unknown += 1
                        else:
                            for key in counts:
                                counts[key] += attempt["usage"][key]
                    if reply is not None or status in (
                        "timeout",
                        "provider_error",
                        "budget_exhausted",
                    ):
                        break
                if reply is None:
                    break
                if clock.now() >= deadline:
                    status = "timeout"
                    break
                decision = reply.decision
                if not isinstance(decision, dict):
                    status = "malformed_response"
                    break
                if set(decision) == {"answer"} and isinstance(decision["answer"], str):
                    answers += 1
                    passed = task.grade(
                        decision["answer"], fixture.snapshot(), fixture.writes
                    )
                    if answers == 1:
                        first_answer_pass = passed
                        first_pass = passed and execution_errors == 0
                    if passed:
                        status = "passed"
                        break
                    if answers >= config.max_answers:
                        status = "wrong_answer_or_state"
                        break
                    observation = {
                        "validation": "final answer or actual state invalid; expected values withheld"
                    }
                elif set(decision) == ({"script"} if mode == "weave" else {"calls"}):
                    invalid = (
                        mode == "weave" and not isinstance(decision["script"], str)
                    ) or (
                        mode == "native"
                        and (
                            not isinstance(decision["calls"], list)
                            or len(decision["calls"]) > 64
                            or any(
                                not isinstance(c, dict)
                                or set(c) != {"name", "arguments"}
                                or not isinstance(c["name"], str)
                                or not isinstance(c["arguments"], dict)
                                for c in decision["calls"]
                            )
                        )
                    )
                    observation = (
                        {
                            "observations": [],
                            "execution_error": "execution failed; inspect tool observations (no hidden grader values)",
                        }
                        if invalid
                        else execute(fixture, mode, decision, config.native_parallel)
                    )
                    if fixture.exceeded:
                        status = "resource_limit"
                        break
                    execution_errors += int(observation["execution_error"] is not None)
                else:
                    status = "malformed_response"
                    break
                feedback.append(observation)
                history.extend([{"decision": decision}, {"feedback": observation}])
            if clock.now() >= deadline:
                status = "timeout"
                first_pass = first_answer_pass = False
        except CallBudgetExceeded:
            status = "resource_limit"
        except TimeoutError:
            status = "timeout"
        except Exception:
            status = "harness_error" if fixture is not None else "fixture_error"
        try:
            final_files, writes = (
                (fixture.snapshot(), fixture.writes) if fixture else ({}, [])
            )
        except Exception:
            final_files, writes, status, first_pass = {}, [], "fixture_error", False
        return {
            "methodology_version": 3,
            "kind": "offline_mechanics",
            "measured_model_performance": False,
            "endpoint": "provider_final_answer_and_actual_state",
            "task": task.id,
            "split": task.split,
            "family": task.family,
            "mode": mode,
            "status": status,
            "passed": status == "passed",
            "first_pass": first_pass,
            "first_answer_pass": first_answer_pass,
            "execution_errors": execution_errors,
            "wall_s": clock.now() - started,
            "retry_wait_s": sum(a["wait_s"] for a in attempts),
            "rate_limit_wait_s": sum(
                a["wait_s"] for a in attempts if a.get("outcome") == "rate_limit"
            ),
            "model_turns": turns,
            "answer_submissions": answers,
            "provider_attempts": attempts,
            "known_tokens": counts,
            "unknown_usage_attempts": unknown,
            "token_totals_complete": unknown == 0,
            "config": config.report(),
            "context_hash": digest(base),
            "schema_hash": digest(manifest(task)),
            "initial_state_hash": initial_hash,
            "final_state_hash": digest(final_files),
            "writes": writes,
            "state_matches": final_files == task.final_files,
            "write_log_matches": writes == list(task.expected_writes),
            "unexpected_write": writes != list(task.expected_writes)[: len(writes)],
            "feedback": feedback,
            "requests": requests,
            "fixture_root": tmp,
        }


def summarize(rows):
    """Keep all failures in denominators; failed latency never becomes zero."""
    import statistics

    wall = sorted(r["wall_s"] for r in rows)
    return {
        "kind": "offline_mechanics",
        "measured_model_performance": False,
        "runs": len(rows),
        "passed": sum(r["passed"] for r in rows),
        "first_pass": sum(r["first_pass"] for r in rows),
        "state_mismatch_runs": sum(not r["state_matches"] for r in rows),
        "write_log_mismatch_runs": sum(not r["write_log_matches"] for r in rows),
        "unexpected_write_runs": sum(r["unexpected_write"] for r in rows),
        "failures": {
            s: sum(r["status"] == s for r in rows)
            for s in sorted({r["status"] for r in rows if not r["passed"]})
        },
        "p50_all_wall_s": statistics.median(wall) if wall else None,
        "p95_all_wall_s": wall[max(0, __import__("math").ceil(0.95 * len(wall)) - 1)]
        if wall
        else None,
        "known_tokens": {
            k: sum(r["known_tokens"][k] for r in rows)
            for k in ("input_tokens", "output_tokens", "cached_input_tokens")
        },
        "unknown_usage_attempts": sum(r["unknown_usage_attempts"] for r in rows),
        "token_totals_complete": all(r["token_totals_complete"] for r in rows),
    }
