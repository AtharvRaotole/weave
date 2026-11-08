"""Optional real-model eval. Skips when no provider key is set.

The dataset, JSON tool-call baseline, Weave prompt pack, and error-repair
loop are always loaded. Tokens, pass rate, and retries are written only after
a provider responds.

    python bench/llm_eval.py

Keys and optional model overrides:

    OPENAI_API_KEY, OPENAI_MODELS (comma-separated, default gpt-5.4-mini)

Modes: `weave` (one Weave script), `json_tool_calls` (one ordered JSON list of
calls, no data flow between them), and, for OpenAI, `native_tools` (standard
function calling: one model round trip per turn, tools run one at a time).
Every tool call sleeps WEAVE_EVAL_TOOL_DELAY seconds (default 0.04) in every
mode. WEAVE_EVAL_REPEATS (default 3) repeats each case; latency is wall time
including model calls.
    ANTHROPIC_API_KEY, ANTHROPIC_MODEL (default claude-3-5-haiku-latest)
    GEMINI_API_KEY or GOOGLE_API_KEY, GEMINI_MODEL (default gemini-2.0-flash)
"""

from __future__ import annotations

import json
import os
import sys
import time
import statistics
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from bench.dataset import TASKS, ToolFailure, World, call_tool, render, tool_fn, tool_lines  # noqa: E402
from bench.harness import machine_info, utc_stamp  # noqa: E402
from bench.prompts import few_shot_messages, repair_message, system_message  # noqa: E402

MAX_ATTEMPTS = 3
# Every tool call sleeps this long in every mode, so latency includes tool time.
TOOL_DELAY_S = float(os.environ.get("WEAVE_EVAL_TOOL_DELAY", "0.04"))
REPEATS = int(os.environ.get("WEAVE_EVAL_REPEATS", "3"))
MAX_TOOL_TURNS = 8
MODES = tuple(os.environ.get("WEAVE_EVAL_MODES", "weave,json_tool_calls,native_tools").split(","))
# USD per 1M tokens (input, cached input, output), from
# https://developers.openai.com/api/docs/pricing, checked 2026-11-08.
PRICES = {
    "gpt-5.4-mini": (0.75, 0.075, 4.50),
    "gpt-6.1-sol": (2.00, 0.10, 10.00),
    # https://ai.google.dev/gemini-api/docs/pricing, paid tier, checked 2026-11-08.
    # Output includes thinking tokens.
    "gemini-2.5-flash": (0.30, 0.03, 2.50),
}
# The same reasoning effort is used in every mode. Chat Completions rejects
# function tools while gpt-6.1-sol reasons, so native tool calling uses the
# Responses API for every model.
MODEL_PARAMS = {
    "gpt-6.1-sol": {"reasoning_effort": "low"},
}
BUDGET_USD = float(os.environ.get("WEAVE_EVAL_BUDGET", "2.0"))
SPENT = {"usd": 0.0}
# Seconds spent waiting out HTTP 429s, so evals can subtract them from wall time.
WAITED = {"s": 0.0}


class BudgetExceeded(RuntimeError):
    pass


def _charge(model: str, usage: dict) -> None:
    price = PRICES.get(model)
    if not price:
        return
    cached = usage.get("cached_tokens") or 0
    fresh = (usage.get("input_tokens") or 0) - cached
    SPENT["usd"] += (fresh * price[0] + cached * price[1] + (usage.get("output_tokens") or 0) * price[2]) / 1e6
    if SPENT["usd"] > BUDGET_USD:
        raise BudgetExceeded(f"spend passed ${BUDGET_USD}")


def _slow(fn):
    def wrapped(*args):
        time.sleep(TOOL_DELAY_S)
        try:
            return fn(*args)
        except ToolFailure as exc:
            raise RuntimeError(str(exc)) from None

    wrapped.__name__ = fn.__name__
    return wrapped


def providers() -> list[dict]:
    found = []
    if os.environ.get("OPENAI_API_KEY"):
        models = os.environ.get("OPENAI_MODELS") or os.environ.get("OPENAI_MODEL", "gpt-5.4-mini")
        for model in models.split(","):
            found.append({"name": "openai", "model": model.strip()})
    if os.environ.get("ANTHROPIC_API_KEY"):
        found.append(
            {"name": "anthropic", "model": os.environ.get("ANTHROPIC_MODEL", "claude-3-5-haiku-latest")}
        )
    if os.environ.get("GEMINI_API_KEY") or os.environ.get("GOOGLE_API_KEY"):
        found.append({"name": "gemini", "model": os.environ.get("GEMINI_MODEL", "gemini-2.0-flash")})
    return found


def _post(url: str, payload: dict, headers: dict) -> dict:
    request = urllib.request.Request(
        url,
        data=json.dumps(payload).encode(),
        headers=headers,
        method="POST",
    )
    for attempt in range(8):
        try:
            with urllib.request.urlopen(request, timeout=90) as response:
                return json.loads(response.read().decode())
        except urllib.error.HTTPError as exc:
            if exc.code == 429:
                detail = exc.read().decode(errors="replace")
                if "per day" in detail or "PerDay" in detail:
                    raise BudgetExceeded("provider daily request quota exhausted (HTTP 429)") from None
            if exc.code == 429 and attempt < 7:
                # Rate limits are waited out; the wait is excluded from wall time.
                delay = float(exc.headers.get("Retry-After") or 10 * (attempt + 1))
                time.sleep(delay)
                WAITED["s"] += delay
                continue
            if exc.code < 500:
                detail = exc.read().decode(errors="replace")[:300]
                raise ValueError(f"HTTP {exc.code}: {detail}") from None
            if attempt >= 2:
                raise
        except (urllib.error.URLError, TimeoutError, ConnectionError, OSError):
            # Transport failures are retried; they say nothing about the model.
            if attempt >= 2:
                raise
        time.sleep(2 * (attempt + 1))
    raise RuntimeError("unreachable")


def _usage(input_tokens, output_tokens, cached_tokens=0) -> dict:
    return {"input_tokens": input_tokens, "output_tokens": output_tokens, "cached_tokens": cached_tokens}


def openai_chat(model: str, payload: dict) -> dict:
    payload = {"model": model, **MODEL_PARAMS.get(model, {}), **payload}
    if not model.startswith("gpt-4"):
        # GPT-5 and later reasoning models only accept the default temperature.
        payload.pop("temperature", None)
    body = _post(
        "https://api.openai.com/v1/chat/completions",
        payload,
        {
            "Authorization": f"Bearer {os.environ['OPENAI_API_KEY']}",
            "Content-Type": "application/json",
        },
    )
    usage = body.get("usage") or {}
    cached = (usage.get("prompt_tokens_details") or {}).get("cached_tokens") or 0
    body["_usage"] = _usage(usage.get("prompt_tokens"), usage.get("completion_tokens"), cached)
    _charge(model, body["_usage"])
    return body


def complete(provider: str, model: str, messages: list[dict]) -> tuple[str, dict]:
    if provider == "openai":
        body = openai_chat(model, {"temperature": 0, "messages": messages})
        return body["choices"][0]["message"]["content"] or "", body["_usage"]
    if provider == "anthropic":
        system = "\n\n".join(item["content"] for item in messages if item["role"] == "system")
        dialog = [
            {"role": item["role"], "content": item["content"]}
            for item in messages
            if item["role"] in {"user", "assistant"}
        ]
        body = _post(
            "https://api.anthropic.com/v1/messages",
            {
                "model": model,
                "max_tokens": 800,
                "temperature": 0,
                "system": system,
                "messages": dialog,
            },
            {
                "x-api-key": os.environ["ANTHROPIC_API_KEY"],
                "anthropic-version": "2023-06-01",
                "Content-Type": "application/json",
            },
        )
        text = "".join(
            part.get("text", "") for part in body.get("content", []) if part.get("type") == "text"
        )
        usage = body.get("usage") or {}
        return text, _usage(usage.get("input_tokens"), usage.get("output_tokens"))
    system = "\n\n".join(item["content"] for item in messages if item["role"] == "system")
    contents = []
    for item in messages:
        if item["role"] == "system":
            continue
        role = "user" if item["role"] == "user" else "model"
        contents.append({"role": role, "parts": [{"text": item["content"]}]})
    body = gemini_generate(model, {"systemInstruction": {"parts": [{"text": system}]}, "contents": contents})
    parts = body["candidates"][0]["content"].get("parts") or []
    text = "".join(part.get("text", "") for part in parts if not part.get("thought"))
    return text, body["_usage"]


def gemini_generate(model: str, payload: dict) -> dict:
    """One generateContent call. Output tokens include thinking tokens, as billed."""
    key = os.environ.get("GEMINI_API_KEY") or os.environ.get("GOOGLE_API_KEY")
    body = _post(
        f"https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent",
        payload,
        {"Content-Type": "application/json", "x-goog-api-key": key},
    )
    usage = body.get("usageMetadata") or {}
    out = (usage.get("candidatesTokenCount") or 0) + (usage.get("thoughtsTokenCount") or 0)
    body["_usage"] = _usage(usage.get("promptTokenCount"), out, usage.get("cachedContentTokenCount") or 0)
    _charge(model, body["_usage"])
    return body


def _strip_fence(text: str) -> str:
    stripped = text.strip()
    if stripped.startswith("```"):
        lines = stripped.splitlines()
        lines = lines[1:]
        if lines and lines[-1].startswith("```"):
            lines = lines[:-1]
        return "\n".join(lines).strip()
    return stripped


def _engine(task, world):
    from weave import Engine

    engine = Engine()
    for tool in task.tools:
        engine.tool(
            tool.name,
            _slow(tool_fn(world, tool.name)),
            effect=tool.effect,
            arity=len(tool.params),
        )
    return engine


def execute_weave(task, source: str, world) -> str:
    if not source.endswith("\n"):
        source += "\n"
    try:
        return _engine(task, world).run(source).stdout.strip()
    except Exception as exc:  # noqa: BLE001 - Weave runtime errors are graded as failures
        raise ValueError(str(exc)) from None


def check_weave(task, source: str) -> list[str]:
    if not source.endswith("\n"):
        source += "\n"
    return _engine(task, World()).check(source)


def execute_json_calls(text: str, world) -> str:
    calls = json.loads(_strip_fence(text))
    if not isinstance(calls, list) or not calls:
        raise ValueError("JSON tool calls must be a non-empty array")
    last = ""
    for call in calls:
        if not isinstance(call, dict) or "tool" not in call:
            raise ValueError("each call needs a tool name")
        time.sleep(TOOL_DELAY_S)
        try:
            last = render(call_tool(world, call["tool"], list(call.get("args") or [])))
        except ToolFailure as exc:
            raise ValueError(f"tool {call['tool']} failed: {exc}") from None
        except (AttributeError, TypeError) as exc:
            raise ValueError(f"bad call {call!r}: {exc}") from None
    return last


def user_prompt(task, mode: str) -> str:
    tools = tool_lines(task)
    if mode == "weave":
        return (
            f"Tools:\n{tools}\n"
            f"Task: {task.instruction}\n"
            "Write a Weave script only. Print the final answer."
        )
    return (
        f"Tools:\n{tools}\n"
        f"Task: {task.instruction}\n"
        "Reply with a JSON array only. Each item is {\"tool\": name, \"args\": [...]}. "
        "Execute calls in that order. The last call's return value is the answer."
    )


class WrongAnswer(Exception):
    pass


def wrong_answer(output: str) -> str:
    """Feedback for a run that executed but was graded wrong.

    It never contains the expected answer or the expected write log, so a
    retry has to come from the model, not from the grader."""
    return f"The run finished but the result is not correct. Printed output: {output!r}. Re-read the task and fix the logic."


def _graded(task, output: str, world) -> bool:
    if output != task.expected:
        return False
    return task.expected_writes is None or tuple(world.writes) == task.expected_writes


JSON_SYSTEM = (
    "You plan tool calls as one JSON array. Each item is {\"tool\": name, \"args\": [...]}. "
    "Calls run in order. Arguments must be literal values: there are no variables and no way to "
    "pass one call's result into another call. The last call's return value is the answer. "
    "Reply with the JSON array only."
)
JSON_SHOTS = (
    ("Tools:\nsearch(query), effect read. Search one query.\nTask: Search for cats and print the result.",
     '[{"tool": "search", "args": ["cats"]}]'),
)


def initial_messages(task, mode: str) -> list[dict]:
    if mode == "weave":
        return [system_message(), *few_shot_messages(), {"role": "user", "content": user_prompt(task, mode)}]
    shots = []
    for user, reply in JSON_SHOTS:
        shots += [{"role": "user", "content": user}, {"role": "assistant", "content": reply}]
    return [{"role": "system", "content": JSON_SYSTEM}, *shots, {"role": "user", "content": user_prompt(task, mode)}]


def grade_task(provider: dict, task, mode: str) -> dict:
    messages = initial_messages(task, mode)
    attempts = []
    passed = False
    source = ""
    started = time.perf_counter()
    model_s = 0.0
    for attempt in range(1, MAX_ATTEMPTS + 1):
        world = World()
        usage = None
        try:
            t0 = time.perf_counter()
            text, usage = complete(provider["name"], provider["model"], messages)
            model_s += time.perf_counter() - t0
            source = _strip_fence(text)
            if mode == "weave":
                errors = check_weave(task, source)
                if errors:
                    raise ValueError("\n".join(errors))
                output = execute_weave(task, source, world)
            else:
                output = execute_json_calls(text, world)
            if _graded(task, output, world):
                attempts.append({"attempt": attempt, "ok": True, "usage": usage, "error": None, "writes": list(world.writes)})
                passed = True
                break
            # Logged once, by the handler below.
            raise WrongAnswer(wrong_answer(output))
        except (
            WrongAnswer,
            urllib.error.URLError,
            urllib.error.HTTPError,
            KeyError,
            ValueError,
            TypeError,
            json.JSONDecodeError,
        ) as exc:
            attempts.append(
                {"attempt": attempt, "ok": False, "usage": usage, "error": str(exc), "writes": list(world.writes), "source": source}
            )
            messages.append({"role": "assistant", "content": source or str(exc)})
            if isinstance(exc, WrongAnswer):
                repair = f"{exc}\nReply with only the corrected {'Weave script' if mode == 'weave' else 'JSON array of tool calls'}."
            elif mode == "weave":
                repair = repair_message(source, [str(exc)])
            else:
                repair = (
                    f"That failed: {exc}\nReply with only the corrected JSON array of tool calls."
                )
            messages.append({"role": "user", "content": repair})
    input_tokens = sum((item["usage"] or {}).get("input_tokens") or 0 for item in attempts)
    output_tokens = sum((item["usage"] or {}).get("output_tokens") or 0 for item in attempts)
    return {
        "task": task.id,
        "provider": provider["name"],
        "model": provider["model"],
        "mode": mode,
        "passed": passed,
        "attempts": len(attempts),
        "retries": max(0, len(attempts) - 1),
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "model_turns": len(attempts),
        "wall_s": time.perf_counter() - started,
        "model_s": model_s,
        "attempt_log": attempts,
    }


def openai_responses(model: str, payload: dict) -> dict:
    extra = {}
    if "reasoning_effort" in MODEL_PARAMS.get(model, {}):
        extra["reasoning"] = {"effort": MODEL_PARAMS[model]["reasoning_effort"]}
    body = _post(
        "https://api.openai.com/v1/responses",
        {"model": model, **extra, **payload},
        {
            "Authorization": f"Bearer {os.environ['OPENAI_API_KEY']}",
            "Content-Type": "application/json",
        },
    )
    usage = body.get("usage") or {}
    cached = (usage.get("input_tokens_details") or {}).get("cached_tokens") or 0
    body["_usage"] = _usage(usage.get("input_tokens"), usage.get("output_tokens"), cached)
    _charge(model, body["_usage"])
    return body


def _response_tools(task) -> list[dict]:
    return [{"type": "function", **tool["function"]} for tool in _openai_tools(task)]


def _openai_tools(task) -> list[dict]:
    return [
        {
            "type": "function",
            "function": {
                "name": tool.name,
                "description": f"{tool.doc} Effect: {tool.effect}.",
                "parameters": {
                    "type": "object",
                    "properties": {p: {"type": "string"} for p in tool.params},
                    "required": list(tool.params),
                    "additionalProperties": False,
                },
            },
        }
        for tool in task.tools
    ]


def grade_native(provider: dict, task) -> dict:
    """Standard function calling: one model round trip per turn, tools run one
    after another in the order the model asked for them."""
    instructions = "Use the tools to complete the task. When done, reply with only the final answer string."
    items: list = [{"role": "user", "content": f"Task: {task.instruction}"}]
    started = time.perf_counter()
    model_s = 0.0
    tool_calls_made = 0
    turns = 0
    input_tokens = output_tokens = 0
    answer = None
    error = None
    world = World()
    try:
        while turns < MAX_TOOL_TURNS:
            turns += 1
            t0 = time.perf_counter()
            body = openai_responses(
                provider["model"],
                {"instructions": instructions, "input": items, "tools": _response_tools(task)},
            )
            model_s += time.perf_counter() - t0
            input_tokens += body["_usage"]["input_tokens"] or 0
            output_tokens += body["_usage"]["output_tokens"] or 0
            output = body.get("output") or []
            calls = [item for item in output if item.get("type") == "function_call"]
            if not calls:
                answer = "".join(
                    part.get("text", "")
                    for item in output
                    if item.get("type") == "message"
                    for part in item.get("content") or []
                    if part.get("type") == "output_text"
                ).strip()
                break
            items.extend(output)
            for call in calls:
                args = json.loads(call.get("arguments") or "{}")
                tool = next(t for t in task.tools if t.name == call["name"])
                time.sleep(TOOL_DELAY_S)
                try:
                    result = render(call_tool(world, tool.name, [args[p] for p in tool.params]))
                except ToolFailure as exc:
                    result = f"ERROR: {exc}"
                tool_calls_made += 1
                items.append({"type": "function_call_output", "call_id": call["call_id"], "output": result})
        else:
            error = f"no final answer after {MAX_TOOL_TURNS} turns"
    except (urllib.error.URLError, KeyError, ValueError, TypeError, StopIteration) as exc:
        error = str(exc)
    passed = answer is not None and _graded(task, answer, world)
    if answer is not None and not passed:
        error = f"output was {answer!r}, writes {world.writes!r}"
    return {
        "task": task.id,
        "provider": provider["name"],
        "model": provider["model"],
        "mode": "native_tools",
        "passed": passed,
        "attempts": 1,
        "retries": 0,
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "model_turns": turns,
        "tool_calls": tool_calls_made,
        "wall_s": time.perf_counter() - started,
        "model_s": model_s,
        "error": error,
        "writes": list(world.writes),
    }


def summarize(rows: list[dict]) -> list[dict]:
    out = []
    keys = sorted({(r["model"], r["task"], r["mode"]) for r in rows})
    for model, task, mode in keys:
        group = [r for r in rows if (r["model"], r["task"], r["mode"]) == (model, task, mode)]
        out.append(
            {
                "model": model,
                "task": task,
                "mode": mode,
                "runs": len(group),
                "passed": sum(r["passed"] for r in group),
                "median_wall_s": statistics.median(r["wall_s"] for r in group),
                "median_model_turns": statistics.median(r["model_turns"] for r in group),
                "input_tokens": sum(r["input_tokens"] for r in group),
                "output_tokens": sum(r["output_tokens"] for r in group),
            }
        )
    return out


def by_mode(rows: list[dict]) -> list[dict]:
    out = []
    for model, mode in sorted({(r["model"], r["mode"]) for r in rows}):
        group = [r for r in rows if (r["model"], r["mode"]) == (model, mode)]
        first = [r for r in group if r["passed"] and r["attempts"] == 1]
        out.append(
            {
                "model": model,
                "mode": mode,
                "runs": len(group),
                "passed": sum(r["passed"] for r in group),
                "passed_first_try": len(first),
                "median_wall_s": statistics.median(r["wall_s"] for r in group),
                "input_tokens": sum(r["input_tokens"] for r in group),
                "output_tokens": sum(r["output_tokens"] for r in group),
                "failed_tasks": sorted({r["task"] for r in group if not r["passed"]}),
            }
        )
    return out


def main() -> int:
    found = providers()
    out_dir = ROOT / "bench" / "results"
    out_dir.mkdir(parents=True, exist_ok=True)
    task_ids = [task.id for task in TASKS]
    if not found:
        payload = {
            "kind": "llm",
            "skipped": True,
            "measured": False,
            "reason": (
                "Set OPENAI_API_KEY, ANTHROPIC_API_KEY, and/or GEMINI_API_KEY "
                "(or GOOGLE_API_KEY), then run python bench/llm_eval.py."
            ),
            "tasks": task_ids,
            "modes": ["weave", "json_tool_calls"],
            "machine": machine_info(),
        }
    else:
        rows = []
        stopped = None
        only = os.environ.get("WEAVE_EVAL_TASKS")
        tasks = [t for t in TASKS if not only or t.id in only.split(",")]
        try:
            for repeat in range(REPEATS):
                for provider in found:
                    for task in tasks:
                        for mode in MODES:
                            if mode == "native_tools":
                                if provider["name"] != "openai":
                                    continue
                                row = grade_native(provider, task)
                            else:
                                row = grade_task(provider, task, mode)
                            row["repeat"] = repeat
                            row["category"] = task.category
                            rows.append(row)
                            print(f"{provider['model']:>14} {task.id:<22} {mode:<16} {'PASS' if row['passed'] else 'FAIL'} ${SPENT['usd']:.3f}", flush=True)
        except BudgetExceeded as exc:
            stopped = str(exc)
        passed = sum(1 for row in rows if row["passed"])
        payload = {
            "kind": "llm",
            "skipped": False,
            "measured": True,
            "machine": machine_info(),
            "tasks": task_ids,
            "modes": list(MODES),
            "tool_delay_s": TOOL_DELAY_S,
            "repeats": REPEATS,
            "cases": len(rows),
            "summary": summarize(rows),
            "by_mode": by_mode(rows),
            "prices_per_1m": PRICES,
            "model_params": MODEL_PARAMS,
            "estimated_cost_usd": round(SPENT["usd"], 4),
            "stopped_early": stopped,
            "passed": passed,
            "pass_rate": passed / len(rows) if rows else None,
            "rows": rows,
        }
    text = json.dumps(payload, indent=2) + "\n"
    path = out_dir / f"llm-{utc_stamp()}.json"
    path.write_text(text)
    (out_dir / "llm-latest.json").write_text(text)
    print(f"wrote {path.relative_to(ROOT)}")
    if payload["skipped"]:
        print(payload["reason"])
    else:
        print(f"pass_rate={payload['pass_rate']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
