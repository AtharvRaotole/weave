"""Optional real-model eval. Skips when no provider key is set.

Writes tokens, pass rate, and retries only from API responses. It never fills
in a score when a provider was not called.

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

from bench.harness import machine_info, utc_stamp  # noqa: E402

MAX_ATTEMPTS = 3

WEAVE_PROMPT = """Write a Weave script only, no markdown. Tools:
search(query: str) -> str
summarize(a: str, b: str, c: str, d: str) -> str
Call search for q1, q2, q3, and q4, then summarize those four results and print it.
"""

JSON_PROMPT = """Reply with a JSON array only, no markdown. Each item is {"tool": name, "args": [...]}.
Tools: search(query), summarize(a, b, c, d).
search q1, q2, q3, q4 first. Then summarize those four results in that order.
The array is executed strictly in order.
"""


def providers() -> list[dict]:
    found = []
    if os.environ.get("OPENAI_API_KEY"):
        found.append(
            {
                "name": "openai",
                "model": os.environ.get("OPENAI_MODEL", "gpt-4o-mini"),
            }
        )
    if os.environ.get("ANTHROPIC_API_KEY"):
        found.append(
            {
                "name": "anthropic",
                "model": os.environ.get("ANTHROPIC_MODEL", "claude-3-5-haiku-latest"),
            }
        )
    if os.environ.get("GEMINI_API_KEY") or os.environ.get("GOOGLE_API_KEY"):
        found.append(
            {
                "name": "gemini",
                "model": os.environ.get("GEMINI_MODEL", "gemini-2.0-flash"),
            }
        )
    return found


def _post(url: str, payload: dict, headers: dict) -> dict:
    request = urllib.request.Request(
        url,
        data=json.dumps(payload).encode(),
        headers=headers,
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=60) as response:
        return json.loads(response.read().decode())


def complete(provider: str, model: str, prompt: str) -> tuple[str, dict]:
    if provider == "openai":
        body = _post(
            "https://api.openai.com/v1/chat/completions",
            {
                "model": model,
                "temperature": 0,
                "messages": [{"role": "user", "content": prompt}],
            },
            {
                "Authorization": f"Bearer {os.environ['OPENAI_API_KEY']}",
                "Content-Type": "application/json",
            },
        )
        text = body["choices"][0]["message"]["content"]
        usage = body.get("usage") or {}
        return text, {
            "input_tokens": usage.get("prompt_tokens"),
            "output_tokens": usage.get("completion_tokens"),
        }
    if provider == "anthropic":
        body = _post(
            "https://api.anthropic.com/v1/messages",
            {
                "model": model,
                "max_tokens": 800,
                "temperature": 0,
                "messages": [{"role": "user", "content": prompt}],
            },
            {
                "x-api-key": os.environ["ANTHROPIC_API_KEY"],
                "anthropic-version": "2023-06-01",
                "Content-Type": "application/json",
            },
        )
        text = "".join(part.get("text", "") for part in body.get("content", []) if part.get("type") == "text")
        usage = body.get("usage") or {}
        return text, {
            "input_tokens": usage.get("input_tokens"),
            "output_tokens": usage.get("output_tokens"),
        }
    key = os.environ.get("GEMINI_API_KEY") or os.environ.get("GOOGLE_API_KEY")
    body = _post(
        f"https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key={key}",
        {"contents": [{"parts": [{"text": prompt}]}]},
        {"Content-Type": "application/json"},
    )
    parts = body["candidates"][0]["content"]["parts"]
    text = "".join(part.get("text", "") for part in parts)
    usage = body.get("usageMetadata") or {}
    return text, {
        "input_tokens": usage.get("promptTokenCount"),
        "output_tokens": usage.get("candidatesTokenCount"),
    }


def _strip_fence(text: str) -> str:
    stripped = text.strip()
    if stripped.startswith("```"):
        lines = stripped.splitlines()
        lines = lines[1:]
        if lines and lines[-1].startswith("```"):
            lines = lines[:-1]
        return "\n".join(lines).strip()
    return stripped


def execute_weave(source: str) -> str:
    from weave import Engine

    engine = Engine()

    def search(query: str) -> str:
        return f"hit:{query}"

    def summarize(a: str, b: str, c: str, d: str) -> str:
        return "|".join((a, b, c, d))

    engine.tool("search", search, effect="pure", arity=1)
    engine.tool("summarize", summarize, effect="pure", arity=4)
    if not source.endswith("\n"):
        source += "\n"
    return engine.run(source).stdout.strip()


def execute_json_calls(text: str) -> str:
    calls = json.loads(_strip_fence(text))
    results = []
    for call in calls:
        name = call["tool"]
        args = call.get("args") or []
        if name == "search":
            results.append(f"hit:{args[0]}")
        elif name == "summarize":
            results.append("|".join(args))
        else:
            raise ValueError(f"unknown tool {name}")
    return results[-1] if results else ""


def grade(provider: dict, mode: str) -> dict:
    prompt = WEAVE_PROMPT if mode == "weave" else JSON_PROMPT
    attempts = []
    passed = False
    for attempt in range(1, MAX_ATTEMPTS + 1):
        try:
            text, usage = complete(provider["name"], provider["model"], prompt)
            output = execute_weave(_strip_fence(text)) if mode == "weave" else execute_json_calls(text)
            ok = output == "hit:q1|hit:q2|hit:q3|hit:q4"
            attempts.append({"attempt": attempt, "ok": ok, "usage": usage, "error": None})
            if ok:
                passed = True
                break
        except (urllib.error.URLError, urllib.error.HTTPError, KeyError, ValueError, json.JSONDecodeError) as exc:
            attempts.append({"attempt": attempt, "ok": False, "usage": None, "error": str(exc)})
    retries = max(0, len(attempts) - 1)
    input_tokens = sum((item["usage"] or {}).get("input_tokens") or 0 for item in attempts)
    output_tokens = sum((item["usage"] or {}).get("output_tokens") or 0 for item in attempts)
    return {
        "provider": provider["name"],
        "model": provider["model"],
        "mode": mode,
        "passed": passed,
        "attempts": len(attempts),
        "retries": retries,
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "attempt_log": attempts,
    }


def main() -> int:
    found = providers()
    out_dir = ROOT / "bench" / "results"
    out_dir.mkdir(parents=True, exist_ok=True)
    if not found:
        payload = {
            "kind": "llm",
            "skipped": True,
            "measured": False,
            "reason": "Set OPENAI_API_KEY, ANTHROPIC_API_KEY, and/or GEMINI_API_KEY (or GOOGLE_API_KEY), then run python bench/llm_eval.py.",
            "machine": machine_info(),
        }
    else:
        rows = []
        for provider in found:
            for mode in ("weave", "json_tool_calls"):
                rows.append(grade(provider, mode))
        passed = sum(1 for row in rows if row["passed"])
        payload = {
            "kind": "llm",
            "skipped": False,
            "measured": True,
            "machine": machine_info(),
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
