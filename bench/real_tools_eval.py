"""Real-tools eval: Weave vs native function calling against real MCP servers.

weave-mcp proxies two public reference servers:
  - @modelcontextprotocol/server-filesystem (npx), rooted at a fresh temp dir
  - mcp-server-fetch (uvx), which fetches live web pages

Both modes talk to the same weave-mcp process. `weave` sends one script to
`weave_run`; `native_tools` calls the proxied tools one turn at a time through
the OpenAI Responses API. Every run gets fresh files.

    OPENAI_API_KEY=... python bench/real_tools_eval.py

Optional: OPENAI_MODELS (default gpt-5.4-mini), WEAVE_EVAL_REPEATS (3),
WEAVE_EVAL_BUDGET (USD, default 1.0), WEAVE_MCP (path to the binary),
WEAVE_FETCH_ARGS (extra mcp-server-fetch flags, e.g. --allow-private-ips when
DNS resolves through a proxy), WEAVE_EVAL_MODES (weave,native_tools),
WEAVE_EVAL_VARIANT (comma-separated, default examples,structured; set to "none" for neither):
  examples    weave-mcp samples each read tool once at startup and shows the output
  structured  listing tools return a list of lines instead of one string
"""

from __future__ import annotations

import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from bench import llm_eval  # noqa: E402
from bench.harness import machine_info, utc_stamp  # noqa: E402
from bench.prompts import few_shot_messages, repair_message, system_message  # noqa: E402

MAX_ATTEMPTS = 3
MAX_TURNS = 10
VARIANT = frozenset(v for v in os.environ.get("WEAVE_EVAL_VARIANT", "examples,structured").split(",") if v)


@dataclass(frozen=True)
class RealTask:
    id: str
    category: str
    instruction: str
    expected: str
    # (relative path, expected content) checked after the run
    expected_file: tuple[str, str] | None = None


TASKS = (
    RealTask("sum_notes", "parallel",
             "Read the files notes/a.txt, notes/b.txt and notes/c.txt. Each holds one integer. Print the sum as a plain integer.",
             "50"),
    RealTask("count_urgent", "loop",
             "List the directory inbox. Read every file in it whose name ends in .txt and count how many contain the exact uppercase word URGENT. Print only the count.",
             "2"),
    RealTask("write_version", "write_order",
             "Read config.json and take its version field. Create the directory out, then write the file out/version.txt containing exactly the letter v followed by the version (no newline). Then read out/version.txt back and print its content.",
             "v1.4.2", ("out/version.txt", "v1.4.2")),
    RealTask("fallback_file", "tool_error",
             "Read missing.txt. If that fails, read fallback.txt instead. Print only the content you read.",
             "fallback-ok"),
    RealTask("example_heading", "fetch",
             "Fetch https://example.com and print only the text of the page's top-level heading.",
             "Example Domain"),
    RealTask("two_pages", "fetch",
             "Fetch https://example.com and https://www.rfc-editor.org/rfc/rfc2606.txt (max_length 800 for both). Print yes if the RFC text contains the name Eastlake and the example page contains the word documentation; otherwise print no.",
             "yes"),
)


def make_fixture(root: Path) -> None:
    (root / "notes").mkdir()
    for name, value in (("a", "17"), ("b", "25"), ("c", "8")):
        (root / "notes" / f"{name}.txt").write_text(value)
    (root / "inbox").mkdir()
    for name, text in (
        ("m1.txt", "URGENT: server down"),
        ("m2.txt", "lunch on friday?"),
        ("m3.txt", "please renew the cert"),
        ("m4.txt", "URGENT pay the invoice"),
        ("m5.md", "URGENT but not a txt file"),
    ):
        (root / "inbox" / name).write_text(text)
    (root / "config.json").write_text(json.dumps({"name": "weave", "version": "1.4.2"}))
    (root / "fallback.txt").write_text("fallback-ok")


class McpClient:
    def __init__(self, argv: list[str]):
        self.proc = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        self.next_id = 0
        self.request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "bench", "version": "0"}})

    def request(self, method: str, params: dict | None = None) -> dict:
        self.next_id += 1
        assert self.proc.stdin and self.proc.stdout
        self.proc.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params or {}}) + "\n")
        self.proc.stdin.flush()
        reply = json.loads(self.proc.stdout.readline())
        if "error" in reply:
            raise ValueError(reply["error"].get("message"))
        return reply["result"]

    def call(self, name: str, arguments: dict) -> tuple[str, bool]:
        result = self.request("tools/call", {"name": name, "arguments": arguments})
        text = "\n".join(part.get("text", "") for part in result.get("content", []))
        return text, bool(result.get("isError"))

    def close(self) -> None:
        self.proc.kill()
        self.proc.wait()


def binary() -> str:
    path = os.environ.get("WEAVE_MCP") or str(ROOT / "target" / "release" / "weave-mcp")
    if not Path(path).exists():
        subprocess.check_call(["cargo", "build", "--release", "-p", "weave-mcp"], cwd=ROOT)
    return path


def start(root: Path) -> McpClient:
    fetch_args = os.environ.get("WEAVE_FETCH_ARGS", "").split()
    fs = {"name": "fs", "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", str(root)], "prefix": "fs_"}
    web = {"name": "fetch", "command": "uvx", "args": ["mcp-server-fetch", *fetch_args], "prefix": "web_",
           "effects": {"fetch": "read"}}
    if "structured" in VARIANT:
        fs["results"] = {"list_directory": "lines", "search_files": "lines", "list_allowed_directories": "lines"}
    if "examples" in VARIANT:
        # One sample per read tool, on paths and a page that no task asks about.
        fs["examples"] = {
            "list_directory": {"path": str(root)},
            "search_files": {"path": str(root), "pattern": "*.json"},
            "read_text_file": {"path": str(root / "config.json")},
            "list_allowed_directories": {},
        }
        web["examples"] = {"fetch": {"url": "https://example.org", "max_length": 300}}
    config = {"upstreams": [fs, web]}
    path = root.parent / "weave-mcp.json"
    path.write_text(json.dumps(config))
    return McpClient([binary(), "--config", str(path)])


def tool_listing(tools: list[dict]) -> str:
    run = next(t for t in tools if t["name"] == "weave_run")
    return run["description"].split("Tools callable from the script:\n", 1)[1]


def graded(task: RealTask, answer: str, root: Path) -> bool:
    if answer.strip() != task.expected:
        return False
    if task.expected_file:
        target = root / task.expected_file[0]
        return target.exists() and target.read_text() == task.expected_file[1]
    return True


def run_weave(model: str, task: RealTask, client: McpClient, tools: list[dict], root: Path) -> dict:
    returns = "Tools (results are strings unless a tool says otherwise)" if "structured" in VARIANT else "Tools (all return strings)"
    prompt = (
        f"{returns}:\n{tool_listing(tools)}\n"
        f"All files are under {root}; pass absolute paths.\n"
        f"Task: {task.instruction}\nWrite a Weave script only. Print the final answer."
    )
    messages = [system_message(), *few_shot_messages(), {"role": "user", "content": prompt}]
    log, usage_in, usage_out, passed = [], 0, 0, False
    attempt = turns = 0
    started = time.perf_counter()
    while attempt < MAX_ATTEMPTS:
        text, usage = llm_eval.complete("openai", model, messages)
        turns += 1
        attempt += 1
        usage_in += usage["input_tokens"] or 0
        usage_out += usage["output_tokens"] or 0
        source = llm_eval._strip_fence(text)
        body, is_error = client.call("weave_run", {"source": source + ("\n" if not source.endswith("\n") else "")})
        if is_error:
            log.append({"attempt": attempt, "error": body[:500], "source": source})
            messages += [{"role": "assistant", "content": source}, {"role": "user", "content": repair_message(source, [body])}]
            continue
        stdout = json.loads(body)["stdout"].strip()
        if graded(task, stdout, root):
            log.append({"attempt": attempt, "ok": True})
            passed = True
            break
        feedback = llm_eval.wrong_answer(stdout)
        log.append({"attempt": attempt, "error": feedback, "source": source})
        messages += [{"role": "assistant", "content": source}, {"role": "user", "content": feedback + "\nReply with only the corrected Weave script."}]
    return {"passed": passed, "attempts": attempt, "model_turns": turns, "input_tokens": usage_in,
            "output_tokens": usage_out, "wall_s": time.perf_counter() - started, "attempt_log": log}


def run_native(model: str, task: RealTask, client: McpClient, tools: list[dict], root: Path) -> dict:
    fn_tools = []
    for tool in tools:
        if tool["name"].startswith("weave_"):
            continue
        schema = {k: v for k, v in tool["inputSchema"].items() if k != "$schema"}
        fn_tools.append({"type": "function", "name": tool["name"], "description": tool["description"], "parameters": schema, "strict": False})
    items: list = [{"role": "user", "content": f"All files are under {root}; pass absolute paths.\nTask: {task.instruction}"}]
    instructions = "Use the tools to complete the task. When done, reply with only the final answer."
    usage_in = usage_out = turns = calls = 0
    answer, error = None, None
    started = time.perf_counter()
    while turns < MAX_TURNS:
        turns += 1
        body = llm_eval.openai_responses(model, {"instructions": instructions, "input": items, "tools": fn_tools})
        usage_in += body["_usage"]["input_tokens"] or 0
        usage_out += body["_usage"]["output_tokens"] or 0
        output = body.get("output") or []
        fcalls = [item for item in output if item.get("type") == "function_call"]
        if not fcalls:
            answer = "".join(p.get("text", "") for item in output if item.get("type") == "message" for p in item.get("content") or [] if p.get("type") == "output_text").strip()
            break
        items.extend(output)
        for call in fcalls:
            text, is_error = client.call(call["name"], json.loads(call.get("arguments") or "{}"))
            calls += 1
            items.append({"type": "function_call_output", "call_id": call["call_id"], "output": ("ERROR: " if is_error else "") + text})
    else:
        error = f"no final answer after {MAX_TURNS} turns"
    passed = answer is not None and graded(task, answer, root)
    if answer is not None and not passed:
        error = f"answer {answer[:200]!r}"
    return {"passed": passed, "attempts": 1, "model_turns": turns, "tool_calls": calls, "input_tokens": usage_in,
            "output_tokens": usage_out, "wall_s": time.perf_counter() - started, "error": error}


def main() -> int:
    if not os.environ.get("OPENAI_API_KEY"):
        print("set OPENAI_API_KEY")
        return 0
    models = os.environ.get("OPENAI_MODELS", "gpt-5.4-mini").split(",")
    repeats = int(os.environ.get("WEAVE_EVAL_REPEATS", "3"))
    llm_eval.BUDGET_USD = float(os.environ.get("WEAVE_EVAL_BUDGET", "1.0"))
    only = os.environ.get("WEAVE_EVAL_TASKS")
    tasks = [t for t in TASKS if not only or t.id in only.split(",")]
    rows, stopped = [], None
    try:
        for repeat in range(repeats):
            for model in models:
                for task in tasks:
                    for mode in os.environ.get("WEAVE_EVAL_MODES", "weave,native_tools").split(","):
                        tmp = Path(tempfile.mkdtemp(prefix="weave-real-"))
                        root = tmp / "data"
                        root.mkdir()
                        make_fixture(root)
                        client = start(root)
                        try:
                            tools = client.request("tools/list")["tools"]
                            runner = run_weave if mode == "weave" else run_native
                            try:
                                row = runner(model, task, client, tools, root)
                            except (ValueError, KeyError, json.JSONDecodeError) as exc:
                                row = {"passed": False, "attempts": 1, "model_turns": 0, "input_tokens": 0,
                                       "output_tokens": 0, "wall_s": 0.0, "error": f"harness: {exc}"}
                        finally:
                            client.close()
                            shutil.rmtree(tmp, ignore_errors=True)
                        row.update({"model": model, "task": task.id, "category": task.category, "mode": mode, "repeat": repeat})
                        rows.append(row)
                        print(f"{model} {task.id:<16} {mode:<13} {'PASS' if row['passed'] else 'FAIL'} {row['wall_s']:.2f}s ${llm_eval.SPENT['usd']:.3f}", flush=True)
    except llm_eval.BudgetExceeded as exc:
        stopped = str(exc)
    summary = []
    for model, task, mode in sorted({(r["model"], r["task"], r["mode"]) for r in rows}):
        group = [r for r in rows if (r["model"], r["task"], r["mode"]) == (model, task, mode)]
        summary.append({"model": model, "task": task, "mode": mode, "runs": len(group),
                        "passed": sum(r["passed"] for r in group),
                        "median_wall_s": statistics.median(r["wall_s"] for r in group),
                        "median_model_turns": statistics.median(r["model_turns"] for r in group),
                        "input_tokens": sum(r["input_tokens"] for r in group),
                        "output_tokens": sum(r["output_tokens"] for r in group)})
    payload = {"kind": "real_tools", "measured": True, "machine": machine_info(), "models": models,
               "servers": ["@modelcontextprotocol/server-filesystem", "mcp-server-fetch"],
               "repeats": repeats, "variant": sorted(VARIANT), "summary": summary, "rows": rows, "prices_per_1m": llm_eval.PRICES,
               "estimated_cost_usd": round(llm_eval.SPENT["usd"], 4), "stopped_early": stopped}
    out = ROOT / "bench" / "results"
    text = json.dumps(payload, indent=2) + "\n"
    path = out / f"real-{utc_stamp()}.json"
    path.write_text(text)
    (out / "real-latest.json").write_text(text)
    print(f"wrote {path.relative_to(ROOT)} cost=${payload['estimated_cost_usd']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
