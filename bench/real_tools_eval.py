"""Real-tools eval: Weave vs native function calling against real MCP servers.

weave-mcp proxies four reference servers (filesystem, fetch, git and memory):
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
WEAVE_EVAL_VARIANT (comma-separated, default structured; examples is opt-in sampling; set to "none" for neither):
  examples    weave-mcp samples each read tool once at startup and shows the output
  structured  listing tools return a list of lines instead of one string
"""

from __future__ import annotations

import json
import threading
from concurrent.futures import ThreadPoolExecutor, Future
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from typing import Callable
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from bench import llm_eval  # noqa: E402
from bench.harness import machine_info, utc_stamp  # noqa: E402
from bench.prompts import few_shot_messages, repair_message, system_message  # noqa: E402

MAX_ATTEMPTS = 3
MAX_TURNS = 10
VARIANT = frozenset(v for v in os.environ.get("WEAVE_EVAL_VARIANT", "structured").split(",") if v)


@dataclass(frozen=True)
class RealTask:
    id: str
    category: str
    instruction: str
    expected: str
    # (relative path, expected content) checked after the run
    expected_file: tuple[str, str] | None = None
    # extra check on real state after the run (git repo, memory file, ...)
    check: Callable[[Path], bool] | None = None


def git_out(root: Path, *args: str) -> str:
    return subprocess.run(["git", "-C", str(root / "repo"), *args], capture_output=True, text=True).stdout.strip()


def memory_records(root: Path) -> list[dict]:
    path = root.parent / "memory.jsonl"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def entity(root: Path, name: str) -> dict | None:
    return next((r for r in memory_records(root) if r.get("type") == "entity" and r.get("name") == name), None)


def check_commit(root: Path) -> bool:
    return git_out(root, "log", "-1", "--format=%s") == "Add todo" and "TODO.md" in git_out(root, "show", "--name-only", "--format=", "HEAD")


def check_observation(root: Path) -> bool:
    ada = entity(root, "Ada")
    return bool(ada) and "likes tea" in ada["observations"] and len(ada["observations"]) == 3


def check_grace(root: Path) -> bool:
    grace = entity(root, "Grace")
    relation = any(r.get("type") == "relation" and r.get("from") == "Grace" and r.get("to") == "Ada" and r.get("relationType") == "knows"
                   for r in memory_records(root))
    return bool(grace) and grace.get("entityType") == "person" and "works at Navy" in grace["observations"] and relation


def check_project(root: Path) -> bool:
    project = entity(root, "weave")
    return bool(project) and project.get("entityType") == "project" and "version 1.4.2" in project["observations"]


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
    RealTask("append_log", "write_order",
             "Append the line done (followed by a newline) to log.txt without changing its existing content, then read log.txt back and print the number of lines it has.",
             "2", ("log.txt", "start\ndone\n")),
    RealTask("largest_note", "loop",
             "The directory notes holds .txt files that each contain one integer. Print only the file name (for example x.txt) of the file with the largest integer.",
             "b.txt"),
    RealTask("git_last_message", "git",
             "The git repository is the directory repo. Print only the message of its most recent commit.",
             "Add parser"),
    RealTask("git_count", "git",
             "The git repository is the directory repo. Print only the number of commits in its history.",
             "3"),
    RealTask("git_modified", "git",
             "The git repository is the directory repo. One tracked file has been modified but not staged. Print only that file's name.",
             "README.md"),
    RealTask("git_commit", "git_write",
             "The git repository is the directory repo. Stage the untracked file TODO.md and commit it with the message Add todo. Then print only the message of the newest commit.",
             "Add todo", check=check_commit),
    RealTask("mem_lookup", "memory",
             "The knowledge graph (memory tools) has an entity named Ada. One of her observations says which city she lives in. Print only the city.",
             "London"),
    RealTask("mem_observe", "memory_write",
             "In the knowledge graph (memory tools), add the observation likes tea to the entity Ada. Then print only the number of observations Ada has.",
             "3", check=check_observation),
    RealTask("mem_relate", "memory_write",
             "In the knowledge graph (memory tools), create an entity Grace of type person with the observation works at Navy, and a relation from Grace to Ada of type knows. Print done when finished.",
             "done", check=check_grace),
    RealTask("config_to_memory", "cross_server",
             "Read config.json, take its version field, and create a knowledge graph (memory tools) entity named weave of type project with the observation version X, where X is that version. Print only the version.",
             "1.4.2", check=check_project),
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
    (root / "log.txt").write_text("start\n")
    repo = root / "repo"
    repo.mkdir()
    env = {**os.environ, "GIT_AUTHOR_NAME": "Eval", "GIT_AUTHOR_EMAIL": "eval@example.com",
           "GIT_COMMITTER_NAME": "Eval", "GIT_COMMITTER_EMAIL": "eval@example.com"}
    git = lambda *args: subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True, env=env)  # noqa: E731
    git("init", "-q", "-b", "main")
    for name, text, message in (("README.md", "# demo\n", "Initial commit"), ("lexer.py", "TOKENS = []\n", "Add lexer"),
                                ("parser.py", "def parse(): pass\n", "Add parser")):
        (repo / name).write_text(text)
        git("add", name)
        git("commit", "-q", "-m", message)
    (repo / "README.md").write_text("# demo\nmore\n")
    (repo / "TODO.md").write_text("- ship\n")
    records = [
        {"type": "entity", "name": "Ada", "entityType": "person", "observations": ["lives in London", "writes programs"]},
        {"type": "entity", "name": "Charles", "entityType": "person", "observations": ["lives in Cambridge"]},
        {"type": "relation", "from": "Ada", "to": "Charles", "relationType": "knows"},
    ]
    (root.parent / "memory.jsonl").write_text("".join(json.dumps(r) + "\n" for r in records))


def make_samples(tmp: Path) -> Path:
    """A scratch git repo outside the task data, used only for example outputs."""
    sample = tmp / "sample-repo"
    sample.mkdir()
    env = {**os.environ, "GIT_AUTHOR_NAME": "Sam", "GIT_AUTHOR_EMAIL": "s@example.com",
           "GIT_COMMITTER_NAME": "Sam", "GIT_COMMITTER_EMAIL": "s@example.com"}
    (sample / "notes.txt").write_text("x\n")
    for args in (["init", "-q", "-b", "main"], ["add", "notes.txt"], ["commit", "-q", "-m", "Sample commit"]):
        subprocess.run(["git", "-C", str(sample), *args], check=True, capture_output=True, env=env)
    (sample / "notes.txt").write_text("y\n")
    return sample


class McpClient:
    def __init__(self, argv: list[str]):
        self.proc = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        self.next_id = 0
        self.lock = threading.Lock()
        self.pending = {}
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()
        self.request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "bench", "version": "0"}})

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
                    future.set_exception(RuntimeError("MCP process closed"))
                self.pending.clear()

    def request(self, method: str, params: dict | None = None) -> dict:
        future = Future()
        with self.lock:
            self.next_id += 1
            self.pending[self.next_id] = future
            self.proc.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params or {}}) + "\n")
            self.proc.stdin.flush()
        reply = future.result(timeout=90)
        if "error" in reply:
            raise ValueError(reply["error"].get("message"))
        return reply["result"]

    def call(self, name: str, arguments: dict) -> tuple[str, bool]:
        result = self.request("tools/call", {"name": name, "arguments": arguments})
        text = json.dumps(result["structuredContent"]) if "structuredContent" in result else "\n".join(part.get("text", "") for part in result.get("content", []))
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
    fs = {"name": "fs", "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", str(root)], "prefix": "fs_", "results": {"read_text_file": "text"}}
    web = {"name": "fetch", "command": "uvx", "args": ["mcp-server-fetch", *fetch_args], "prefix": "web_",
           "effects": {"fetch": "read"}}
    if "structured" in VARIANT:
        fs["results"].update({"list_directory": "lines", "search_files": "lines", "list_allowed_directories": "lines"})
    if "examples" in VARIANT:
        # One sample per read tool, on paths and a page that no task asks about.
        fs["examples"] = {
            "list_directory": {"path": str(root)},
            "search_files": {"path": str(root), "pattern": "*.json"},
            "read_text_file": {"path": str(root / "config.json")},
            "list_allowed_directories": {},
        }
        web["examples"] = {"fetch": {"url": "https://example.org", "max_length": 300}}
    git = {"name": "git", "command": "uvx", "args": ["mcp-server-git"], "prefix": ""}
    memory = {"name": "memory", "command": "env",
              "args": [f"MEMORY_FILE_PATH={root.parent / 'memory.jsonl'}", "npx", "-y", "@modelcontextprotocol/server-memory"],
              "prefix": "mem_"}
    if "structured" in VARIANT:
        memory["results"] = {"read_graph": "json", "search_nodes": "json", "open_nodes": "json"}
    if "examples" in VARIANT:
        sample = make_samples(root.parent)
        git["examples"] = {"git_log": {"repo_path": str(sample), "max_count": 1}, "git_status": {"repo_path": str(sample)}}
        memory["examples"] = {"search_nodes": {"query": "no-such-entity"}}
    config = {"upstreams": [fs, web, git, memory]}
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
        if not (target.exists() and target.read_text() == task.expected_file[1]):
            return False
    return task.check(root) if task.check else True


def final_answer(model: str, task: RealTask, output: str) -> tuple[str, dict]:
    """Same final model-answer endpoint as native, including its token/latency cost."""
    instructions = "Use the tools to complete the task. When done, reply with only the final answer."
    prompt = f"Task: {task.instruction}\nWorkflow tool output:\n{output}\nReply with only the final answer."
    if model.startswith("gemini"):
        body = llm_eval.gemini_generate(model, {"systemInstruction": {"parts": [{"text": instructions}]},
            "contents": [{"role": "user", "parts": [{"text": prompt}]}]})
        parts = body["candidates"][0]["content"].get("parts") or []
        text = "".join(p.get("text", "") for p in parts if not p.get("thought"))
    else:
        body = llm_eval.openai_responses(model, {"instructions": instructions, "input": [{"role": "user", "content": prompt}]})
        text = "".join(p.get("text", "") for m in body.get("output", []) if m.get("type") == "message"
                       for p in m.get("content", []) if p.get("type") == "output_text")
    return text.strip(), body["_usage"]


def native_calls(client, tools, calls):
    """Bounded concurrent reads within a turn; preserve order around writes.

    Tool annotations are trusted fixture policy, not a security boundary.
    Returns in request order even if responses arrive out of order.
    """
    read_names = {t["name"] for t in tools if t.get("annotations", {}).get("readOnlyHint") is True}
    width = max(1, int(os.environ.get("WEAVE_NATIVE_PARALLEL", "4")))
    results = []
    group = []
    with ThreadPoolExecutor(max_workers=width) as pool:
        def drain():
            results.extend(f.result() for f in group)
            group.clear()
        for name, args in calls:
            if name in read_names:
                group.append(pool.submit(client.call, name, args))
            else:
                drain()
                results.append(client.call(name, args))
        drain()
    return results


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
        text, usage = llm_eval.complete("gemini" if model.startswith("gemini") else "openai", model, messages)
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
        answer, final_usage = final_answer(model, task, stdout)
        turns += 1
        usage_in += final_usage["input_tokens"] or 0
        usage_out += final_usage["output_tokens"] or 0
        if graded(task, answer, root):
            log.append({"attempt": attempt, "ok": True})
            passed = True
            break
        feedback = llm_eval.wrong_answer(answer)
        log.append({"attempt": attempt, "error": feedback, "source": source})
        messages += [{"role": "assistant", "content": source}, {"role": "user", "content": feedback + "\nReply with only the corrected Weave script."}]
    return {"passed": passed, "attempts": attempt, "model_turns": turns, "input_tokens": usage_in,
            "output_tokens": usage_out, "wall_s": time.perf_counter() - started, "attempt_log": log}


def run_native(model: str, task: RealTask, client: McpClient, tools: list[dict], root: Path) -> dict:
    if model.startswith("gemini"):
        return run_native_gemini(model, task, client, tools, root)
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
        results = native_calls(client, tools, [(c["name"], json.loads(c.get("arguments") or "{}")) for c in fcalls])
        for call, (text, is_error) in zip(fcalls, results):
            calls += 1
            items.append({"type": "function_call_output", "call_id": call["call_id"], "output": ("ERROR: " if is_error else "") + text})
    else:
        error = f"no final answer after {MAX_TURNS} turns"
    passed = answer is not None and graded(task, answer, root)
    if answer is not None and not passed:
        error = f"answer {answer[:200]!r}"
    return {"passed": passed, "attempts": 1, "model_turns": turns, "tool_calls": calls, "input_tokens": usage_in,
            "output_tokens": usage_out, "wall_s": time.perf_counter() - started, "error": error}


def run_native_gemini(model: str, task: RealTask, client: McpClient, tools: list[dict], root: Path) -> dict:
    """Native Gemini function calling (generateContent), same loop as the OpenAI one."""
    decls = []
    for tool in tools:
        if tool["name"].startswith("weave_"):
            continue
        schema = {k: v for k, v in tool["inputSchema"].items() if k != "$schema"}
        decls.append({"name": tool["name"], "description": tool["description"], "parametersJsonSchema": schema})
    contents: list = [{"role": "user", "parts": [{"text": f"All files are under {root}; pass absolute paths.\nTask: {task.instruction}"}]}]
    system = {"parts": [{"text": "Use the tools to complete the task. When done, reply with only the final answer."}]}
    usage_in = usage_out = turns = calls = 0
    answer, error = None, None
    started = time.perf_counter()
    while turns < MAX_TURNS:
        turns += 1
        body = llm_eval.gemini_generate(model, {"systemInstruction": system, "contents": contents,
                                                "tools": [{"functionDeclarations": decls}]})
        usage_in += body["_usage"]["input_tokens"] or 0
        usage_out += body["_usage"]["output_tokens"] or 0
        content = body["candidates"][0].get("content") or {"role": "model", "parts": []}
        parts = content.get("parts") or []
        fcalls = [p["functionCall"] for p in parts if "functionCall" in p]
        if not fcalls:
            answer = "".join(p.get("text", "") for p in parts if not p.get("thought")).strip()
            break
        contents.append(content)
        responses = []
        results = native_calls(client, tools, [(c["name"], c.get("args") or {}) for c in fcalls])
        for call, (text, is_error) in zip(fcalls, results):
            calls += 1
            responses.append({"functionResponse": {"name": call["name"], "response": {"result": ("ERROR: " if is_error else "") + text}}})
        contents.append({"role": "user", "parts": responses})
    else:
        error = f"no final answer after {MAX_TURNS} turns"
    passed = answer is not None and graded(task, answer, root)
    if answer is not None and not passed:
        error = f"answer {answer[:200]!r}"
    return {"passed": passed, "attempts": 1, "model_turns": turns, "tool_calls": calls, "input_tokens": usage_in,
            "output_tokens": usage_out, "wall_s": time.perf_counter() - started, "error": error}


def main() -> int:
    print("Historical v2 live driver retired: controlled v3 mechanics are offline-only; corrected live runs remain pending.")
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
