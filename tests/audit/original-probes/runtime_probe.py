"""Bounded audit probes. Run with the audited checkout's Python environment."""
import asyncio
import json
import threading
import time

from weave import Engine, WeaveTool


def capture(fn):
    try:
        return {"returned": fn()}
    except Exception as exc:
        return {"error": type(exc).__name__, "message": str(exc)}


out = {}
engine = Engine()
engine.set_limits(max_value_size=64)
out["literal_limit"] = capture(lambda: len(engine.run('x = "' + 'a' * 4096 + '"\n').bindings["x"]))
engine.tool("large", lambda: "b" * 4096, effect="read")
out["tool_result_limit"] = capture(lambda: len(engine.run('x = large()\n').bindings["x"]))
out["output_budget"] = capture(lambda: len(engine.run('for i in range(20):\n    print("a" * 32)\n').stdout))

tool = WeaveTool([])
out["malformed_source"] = {str(value): capture(lambda value=value: tool.handle({"id": "test", "function": {"arguments": json.dumps({"source": value})}})) for value in (None, 42, [], {})}

replays = []
for attempt in range(12):
    lock = threading.Lock()
    counter = [0]
    def read():
        with lock:
            counter[0] += 1
            n = counter[0]
            observed_at = time.monotonic_ns()
        time.sleep(0.015 if n == 1 else 0.001)
        return observed_at
    engine = Engine()
    engine.tool("read", read, effect="read")
    source = "a = read()\nb = read()\nprint(a, b)\n"
    result = engine.run(source)
    replayed = engine.replay(source, result.trace_json())
    replays.append({"original": result.stdout.strip(), "replay": replayed.stdout.strip(), "calls_after_replay": counter[0]})
out["duplicate_read_replay"] = {"mismatches": sum(r["original"] != r["replay"] for r in replays), "runs": replays}

effects = []
for attempt in range(12):
    writes = []
    engine = Engine()
    engine.tool("write", lambda: writes.append("written"), effect="irreversible")
    result = capture(lambda: engine.run('try:\n    x = 1 / 0\n    write()\nexcept:\n    print("caught")\n').stdout)
    time.sleep(0.01)
    effects.append({"result": result, "writes": writes})
out["write_after_exception"] = effects

engine = Engine()
source = 'x = 0\ntry:\n    x = 1\n    y = fail()\nexcept ToolError:\n    print(x)\nprint(x)\n'
engine.tool("fail", lambda: (_ for _ in ()).throw(ValueError("expected failure")), effect="read")
out["try_environment"] = capture(lambda: engine.run(source).stdout)

engine = Engine()
engine.tool("echo", lambda x: x, effect="read", arity=1)
out["keyword_arity"] = capture(lambda: engine.run('print(echo(x="ok"))\n').stdout)
out["unicode_find"] = Engine().run('print("éx".find("x"))\nprint("éx"["éx".find("x"):])\n').stdout

async def async_tool():
    await asyncio.sleep(0)
    return "ok"
engine = Engine()
engine.tool("async_tool", async_tool, effect="read")
out["async_python_tool"] = capture(lambda: engine.run('print(async_tool())\n').stdout)

print(json.dumps(out, indent=2))
