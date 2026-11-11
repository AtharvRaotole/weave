"""One trial: a model drives either native tool calling or Weave's run_weave tool.

Fairness contract (see docs/paper/methodology): identical system prompt, task
text, tool descriptions (both include the return shape), backend, latency,
concurrency cap and generation settings. Native mode gets real parallel tool
calls (reads in one turn run concurrently; a write is a barrier). Weave mode
is the product as shipped: ``weave.WeaveTool`` with its own listing, printed
output and error hints. The trial ends when the model replies without a tool
call; that reply is graded together with the backend's actual write log.
"""

from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor
import inspect
import json
import re
import time

from .world import TOOLS, World, ToolFailure, READ

SYSTEM = ("You are an operations agent with access to tools. Use them to complete the user's task; tool results are "
          "authoritative. When the task is complete, reply with only the final answer in the requested format and "
          "no other text.")

MAX_TURNS = 16


def schema_type(schema):
    from weave.agent_tool import schema_type as render

    return render(schema)


def native_tools():
    out = []
    for name, (effect, params, returns, desc) in TOOLS.items():
        out.append({"type": "function", "function": {
            "name": name,
            "description": f"{desc} Effect: {effect}. Returns: {schema_type(returns)}",
            "parameters": {"type": "object", "properties": {p: {"type": t} for p, t in params.items()},
                           "required": list(params), "additionalProperties": False},
        }})
    return out


def weave_tool(world, *, pipeline_loops=True):
    from weave import ToolError, WeaveTool, declare

    functions = []
    for name, (effect, params, returns, desc) in TOOLS.items():
        def make(name=name, order=tuple(params)):
            def fn(*args, **kwargs):
                if len(args) > len(order):
                    raise ToolError(f"{name} takes {len(order)} arguments")
                kwargs.update(zip(order, args))
                try:
                    return world.call(name, kwargs)
                except ToolFailure as exc:
                    raise ToolError(str(exc)) from None
            return fn
        fn = make()
        fn.__name__ = name
        fn.__doc__ = f"{desc}"
        fn.__signature__ = inspect.Signature([inspect.Parameter(p, inspect.Parameter.POSITIONAL_OR_KEYWORD,
                                                                annotation="str") for p in params])
        fn.__weave_returns__ = returns
        functions.append(declare(effect=effect)(fn))
    tool = WeaveTool(functions, max_wall_time_ms=60_000, max_output_chars=20_000)
    if not pipeline_loops:
        tool.engine.set_limits(pipeline_loops=False)
    return tool


def normalize(text):
    t = (text or "").strip()
    t = re.sub(r"^```[a-z]*\s*|\s*```$", "", t).strip()
    t = t.strip("`'\" ").rstrip(".").strip()
    t = re.sub(r"\s*,\s*", ",", t)
    return t.casefold()


def run_native_calls(world, calls):
    """Reads in one turn run concurrently; a write waits for earlier calls and blocks later ones."""
    results = [None] * len(calls)

    def one(i):
        call = calls[i]
        fn = call.get("function", {})
        try:
            args = json.loads(fn.get("arguments") or "{}")
            if not isinstance(args, dict):
                raise ValueError
        except ValueError:
            results[i] = json.dumps({"error": "arguments are not a JSON object"})
            return
        try:
            results[i] = json.dumps(world.call(fn.get("name"), args))
        except ToolFailure as exc:
            results[i] = json.dumps({"error": str(exc)})

    with ThreadPoolExecutor(max_workers=16) as pool:
        group = []
        for i, call in enumerate(calls):
            name = call.get("function", {}).get("name")
            if TOOLS.get(name, (READ,))[0] == READ:
                group.append(pool.submit(one, i))
            else:
                for f in group:
                    f.result()
                group = []
                one(i)
        for f in group:
            f.result()
    return results


def trial(task, mode, client, *, latency_ms, repeat=0, concurrency=8, pipeline_loops=True, seed=0):
    world = World(task.data, latency_ms=latency_ms, concurrency=concurrency, seed=seed)
    messages = [{"role": "system", "content": SYSTEM}, {"role": "user", "content": task.instruction}]
    if mode == "native":
        tools = native_tools()
    else:
        wt = weave_tool(world, pipeline_loops=pipeline_loops)
        tools = [wt.chat_tool()]
    usage = {"input": 0, "output": 0, "cached": 0, "reasoning": 0}
    unknown_usage = 0
    turns = scripts = script_errors = tool_messages = 0
    llm_s = exec_s = retry_wait = 0.0
    attempts_n = 0
    status, answer = "turn_limit", None
    script_log = []
    started = time.monotonic()
    try:
        while turns < MAX_TURNS:
            turns += 1
            t0 = time.monotonic()
            msg, u, attempts = client.complete(messages, tools)
            llm_s += time.monotonic() - t0
            attempts_n += len(attempts)
            retry_wait += sum(a.get("wait_s") or 0 for a in attempts)
            if u is None:
                unknown_usage += 1
            else:
                for k in usage:
                    usage[k] += u[k]
            calls = msg.get("tool_calls") or []
            assistant = {"role": "assistant", "content": msg.get("content")}
            if calls:
                assistant["tool_calls"] = calls
            messages.append(assistant)
            if not calls:
                answer = msg.get("content") or ""
                status = "answered"
                break
            t1 = time.monotonic()
            if mode == "native":
                outputs = run_native_calls(world, calls)
            else:
                outputs = []
                for call in calls:
                    fn = call.get("function", {})
                    if fn.get("name") != wt.name:
                        outputs.append(f"error: the only tool is {wt.name}")
                        script_errors += 1
                        continue
                    scripts += 1
                    out = wt._run_arguments(fn.get("arguments") or "{}")
                    if out.startswith("error:"):
                        script_errors += 1
                    script_log.append({"turn": turns, "source": _source(fn.get("arguments")), "output": out[:2000]})
                    outputs.append(out)
            exec_s += time.monotonic() - t1
            for call, out in zip(calls, outputs):
                tool_messages += 1
                messages.append({"role": "tool", "tool_call_id": call.get("id"), "content": out})
    except Exception as exc:  # provider failure keeps the row (and its cost) in every denominator
        status = "provider_error:" + getattr(exc, "kind", type(exc).__name__)
    wall = time.monotonic() - started
    answer_ok = answer is not None and normalize(answer) == normalize(task.answer)
    expected = sorted(task.writes)
    actual = sorted(world.writes)
    state_ok = actual == expected and (not task.ordered or list(world.writes) == list(task.writes))
    passed = status == "answered" and answer_ok and state_ok
    return {
        "task": task.id, "family": task.family, "split": task.split, "size": task.size, "mode": mode,
        "repeat": repeat, "latency_ms": latency_ms, "pipeline_loops": pipeline_loops,
        "provider": client.provider, "model": client.model,
        "status": status, "passed": passed, "answer_ok": answer_ok, "state_ok": state_ok,
        "answer": (answer or "")[:300], "expected_answer_hash": task.fingerprint(),
        "unexpected_writes": len([w for w in actual if w not in expected]),
        "missing_writes": len([w for w in expected if w not in actual]),
        "duplicate_writes": len(actual) - len(set(actual)),
        "turns": turns, "tool_messages": tool_messages, "backend_calls": len(world.calls),
        "failed_backend_calls": sum(not c.get("ok", False) for c in world.calls),
        "min_tool_calls": task.min_tool_calls, "min_dependent_rounds": task.min_dependent_rounds,
        "scripts": scripts, "script_errors": script_errors,
        "tokens": usage, "total_tokens": usage["input"] + usage["output"], "unknown_usage_turns": unknown_usage,
        "provider_attempts": attempts_n, "retry_wait_s": retry_wait,
        "llm_s": llm_s, "exec_s": exec_s, "wall_s": wall, "peak_backend_concurrency": world.peak,
        "scripts_log": script_log,
    }


def _source(arguments):
    try:
        return json.loads(arguments or "{}").get("source", "")[:4000]
    except (ValueError, AttributeError):
        return (arguments or "")[:4000]
