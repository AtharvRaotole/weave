import asyncio
import json
import time
import weave

@weave.declare(effect="read")
def slow():
    time.sleep(0.15)
    return "ok"

async def main():
    tool = weave.WeaveTool([slow]).agents_tool()
    ticks = []
    async def heartbeat():
        start = time.perf_counter()
        while time.perf_counter() - start < 0.20:
            ticks.append(time.perf_counter())
            await asyncio.sleep(0.005)
    await asyncio.gather(heartbeat(), tool.on_invoke_tool(None, json.dumps({"source": "print(slow())"})))
    print(json.dumps({"max_heartbeat_gap_ms": max(b-a for a,b in zip(ticks,ticks[1:])) * 1000, "expected_tick_ms": 5}))

asyncio.run(main())
