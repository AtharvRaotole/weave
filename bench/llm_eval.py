"""Optional real-model eval. Skips when no provider key is set.

The dataset, JSON tool-call baseline, Weave prompt pack, and error-repair
loop are always loaded. Tokens, pass rate, and retries are written only after
a provider responds.

    python bench/llm_eval.py

Keys and optional model overrides:

    OPENAI_API_KEY, OPENAI_MODEL (default gpt-4o-mini)
    ANTHROPIC_API_KEY, ANTHROPIC_MODEL (default claude-3-5-haiku-latest)
    GEMINI_API_KEY or GOOGLE_API_KEY, GEMINI_MODEL (default gemini-2.0-flash)
"""

from __future__ import annotations

import json
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from bench.dataset import TASKS, call_tool, tool_lines  # noqa: E402
from bench.harness import machine_info, utc_stamp  # noqa: E402
from bench.prompts import few_shot_messages, repair_message, system_message  # noqa: E402

MAX_ATTEMPTS = 3


def providers() -> list[dict]:
    found = []
    if os.environ.get("OPENAI_API_KEY"):
        found.append({"name": "openai", "model": os.environ.get("OPENAI_MODEL", "gpt-4o-mini")})
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
    with urllib.request.urlopen(request, timeout=90) as response:
        return json.loads(response.read().decode())


def _usage(input_tokens, output_tokens) -> dict:
    return {"input_tokens": input_tokens, "output_tokens": output_tokens}


def complete(provider: str, model: str, messages: list[dict]) -> tuple[str, dict]:
    if provider == "openai":
        body = _post(
            "https://api.openai.com/v1/chat/completions",
            {"model": model, "temperature": 0, "messages": messages},
            {
                "Authorization": f"Bearer {os.environ['OPENAI_API_KEY']}",
                "Content-Type": "application/json",
            },
        )
        text = body["choices"][0]["message"]["content"]
        usage = body.get("usage") or {}
        return text, _usage(usage.get("prompt_tokens"), usage.get("completion_tokens"))
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
    key = os.environ.get("GEMINI_API_KEY") or os.environ.get("GOOGLE_API_KEY")
    system = "\n\n".join(item["content"] for item in messages if item["role"] == "system")
    contents = []
    for item in messages:
        if item["role"] == "system":
            continue
        role = "user" if item["role"] == "user" else "model"
        contents.append({"role": role, "parts": [{"text": item["content"]}]})
    body = _post(
        f"https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key={key}",
        {"systemInstruction": {"parts": [{"text": system}]}, "contents": contents},
        {"Content-Type": "application/json"},
    )
    parts = body["candidates"][0]["content"]["parts"]
    text = "".join(part.get("text", "") for part in parts)
    usage = body.get("usageMetadata") or {}
    return text, _usage(usage.get("promptTokenCount"), usage.get("candidatesTokenCount"))


def _strip_fence(text: str) -> str:
    stripped = text.strip()
    if stripped.startswith("```"):
        lines = stripped.splitlines()
        lines = lines[1:]
        if lines and lines[-1].startswith("```"):
            lines = lines[:-1]
        return "\n".join(lines).strip()
    return stripped


def _engine(task):
    from weave import Engine

    from bench.dataset import IMPLEMENTATIONS

    engine = Engine()
    for tool in task.tools:
        engine.tool(
            tool.name,
            IMPLEMENTATIONS[tool.name],
            effect=tool.effect,
            arity=len(tool.params),
        )
    return engine


def execute_weave(task, source: str) -> str:
    if not source.endswith("\n"):
        source += "\n"
    return _engine(task).run(source).stdout.strip()


def check_weave(task, source: str) -> list[str]:
    if not source.endswith("\n"):
        source += "\n"
    return _engine(task).check(source)


def execute_json_calls(text: str) -> str:
    calls = json.loads(_strip_fence(text))
    if not isinstance(calls, list) or not calls:
        raise ValueError("JSON tool calls must be a non-empty array")
    last = ""
    for call in calls:
        if not isinstance(call, dict) or "tool" not in call:
            raise ValueError("each call needs a tool name")
        last = call_tool(call["tool"], list(call.get("args") or []))
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


def grade_task(provider: dict, task, mode: str) -> dict:
    messages = [system_message(), *few_shot_messages(), {"role": "user", "content": user_prompt(task, mode)}]
    attempts = []
    passed = False
    source = ""
    for attempt in range(1, MAX_ATTEMPTS + 1):
        try:
            text, usage = complete(provider["name"], provider["model"], messages)
            source = _strip_fence(text)
            if mode == "weave":
                errors = check_weave(task, source)
                if errors:
                    raise ValueError("\n".join(errors))
                output = execute_weave(task, source)
            else:
                output = execute_json_calls(text)
            ok = output == task.expected
            attempts.append({"attempt": attempt, "ok": ok, "usage": usage, "error": None})
            if ok:
                passed = True
                break
            raise ValueError(f"output was {output!r}, expected {task.expected!r}")
        except (
            urllib.error.URLError,
            urllib.error.HTTPError,
            KeyError,
            ValueError,
            TypeError,
            json.JSONDecodeError,
        ) as exc:
            attempts.append(
                {"attempt": attempt, "ok": False, "usage": locals().get("usage"), "error": str(exc)}
            )
            messages.append({"role": "assistant", "content": source or str(exc)})
            messages.append({"role": "user", "content": repair_message(source, [str(exc)])})
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
        "attempt_log": attempts,
    }


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
        for provider in found:
            for task in TASKS:
                for mode in ("weave", "json_tool_calls"):
                    rows.append(grade_task(provider, task, mode))
        passed = sum(1 for row in rows if row["passed"])
        payload = {
            "kind": "llm",
            "skipped": False,
            "measured": True,
            "machine": machine_info(),
            "tasks": task_ids,
            "modes": ["weave", "json_tool_calls"],
            "cases": len(rows),
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
