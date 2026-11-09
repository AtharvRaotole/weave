"""Cancel a running Weave script from asyncio and confirm the tool was killed.

    python examples/cancel_run.py

`worker_pool=N` runs every tool in a killable worker process. Cancelling the
task that awaits `WeaveTool.run_async` cancels the script session, which kills
the in-flight worker (hard on POSIX process groups; see docs/isolation.md).
Without a worker pool, cancellation is cooperative only.
"""

import asyncio
import os
import tempfile
import time
from pathlib import Path

import weave

MARKER = Path(tempfile.gettempdir()) / "weave-cancel-example-marker"


@weave.declare(effect="read", arity=0, timeout_ms=30_000)
def slow_report():
    """A tool that would run for ~10 seconds, writing progress to a marker."""
    for i in range(1, 200):
        MARKER.write_text(str(i))
        time.sleep(0.05)
    return "finished"


async def main():
    MARKER.write_text("0")
    tool = weave.WeaveTool([slow_report], worker_pool=1)
    task = asyncio.create_task(tool.run_async("print(slow_report())\n"))
    await asyncio.sleep(0.8)  # let the worker start writing progress
    task.cancel()
    try:
        await task
    except asyncio.CancelledError:
        pass
    at_cancel = int(MARKER.read_text() or "0")
    await asyncio.sleep(0.5)
    after = int(MARKER.read_text() or "0")
    if os.name == "posix" and after > at_cancel + 1:
        raise SystemExit(f"worker kept running after cancel: {at_cancel} -> {after}")
    print(f"cancelled; tool progress stopped at step {after} of 199")


if __name__ == "__main__":
    asyncio.run(main())
