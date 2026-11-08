"""Offline adapter regressions. Run with the built extension installed."""
import asyncio
import json
import subprocess
import sys
import time
import weave

for source in (None, 42, [], {}, True):
    out = weave.WeaveTool([])._run_arguments(json.dumps({"source": source}))
    assert out.startswith("error:"), out
for args in ("null", "[]", "42", "{}", "invalid"):
    assert weave.WeaveTool([])._run_arguments(args).startswith("error:")
p = subprocess.run([sys.executable, "-m", "weave", "typo"], capture_output=True, text=True)
assert p.returncode != 0 and "unknown command" in p.stderr

async def heartbeat_test():
    @weave.declare(effect="read")
    def slow():
        time.sleep(0.15)
        return "ok"
    tool = weave.WeaveTool([slow]).agents_tool()
    ticks = []
    async def beat():
        for _ in range(45):
            ticks.append(time.monotonic())
            await asyncio.sleep(.005)
    async def invoke():
        await asyncio.sleep(.02)
        return await tool.on_invoke_tool(None, json.dumps({"source": "print(slow())"}))
    _, out = await asyncio.gather(beat(), invoke())
    gap = max(b-a for a,b in zip(ticks,ticks[1:]))
    assert gap < .1, gap
    assert "ok" in out
    print("adapter heartbeat max gap seconds", gap)

asyncio.run(heartbeat_test())
print("adapter regressions passed")
