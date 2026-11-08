"""A non-cooperative tool is killed when its deadline fires.

The pool is created inside ``main``. Workers spawn a fresh interpreter and
re-import this file, so nothing here may start a pool at import time.
"""

from __future__ import annotations

import time
from pathlib import Path

import weave

MARKER = Path("/tmp/weave-hard-deadline-marker")


@weave.declare(effect="pure", hard_deadline=True, timeout_ms=5000)
def ping() -> str:
    return "pong"


@weave.declare(effect="read", hard_deadline=True, timeout_ms=150)
def slow() -> str:
    for i in range(1, 40):
        MARKER.write_text(str(i))
        time.sleep(0.05)
    return "finished"


def main() -> None:
    MARKER.write_text("0")
    engine = weave.Engine(worker_pool=2)
    engine.install([ping, slow])
    ready = engine.run("print(ping())\n")
    if ready.stdout != "pong\n":
        raise SystemExit(f"warm call returned {ready.stdout!r}")
    started = time.perf_counter()
    try:
        engine.run("print(slow())\n")
    except Exception as exc:
        text = str(exc)
        if "timed out" not in text:
            raise SystemExit(f"expected a timeout, got {text}") from exc
    else:
        raise SystemExit("slow() finished instead of being killed")
    elapsed = time.perf_counter() - started
    if elapsed >= 1.0:
        raise SystemExit(f"deadline did not return promptly: {elapsed:.3f}s")
    at_return = int(MARKER.read_text() or "0")
    time.sleep(0.5)
    later = int(MARKER.read_text() or "0")
    if later > at_return + 1:
        raise SystemExit(f"worker kept running after the deadline: {at_return} -> {later}")
    print(f"hard_deadline_ok elapsed={elapsed:.3f}s ticks={at_return}")


if __name__ == "__main__":
    main()
