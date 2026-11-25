"""What a fan-out does when one call fails and another hangs. Offline, no API key.

Twelve status checks run in one script. One service raises an error, one never
answers. The script catches the error per item, the hung call is cut off by
its timeout, and the other ten results still come back, in about the time of
the timeout rather than the sum of every call.

    python examples/failing_branch.py
"""

from __future__ import annotations

import time

import weave

SERVICES = [f"svc{i:02d}" for i in range(12)]
CALL_S = 0.2  # every healthy service answers in 200 ms
TIMEOUT_MS = 500


def status(name: str, cancel) -> str:
    """Return a service's status."""
    if name == "svc03":
        raise weave.ToolError(f"{name} is down")
    if name == "svc07":
        # Never answers on its own. The timeout fires the cancel token.
        while not cancel.is_cancelled():
            time.sleep(0.01)
        return "too late"
    time.sleep(CALL_S)
    return "ok"


SCRIPT = f"""
services = {SERVICES!r}

def check(name):
    try:
        return status(name)
    except ToolError as e:
        return "error: " + str(e)

results = {{s: check(s) for s in services}}
bad = {{s: r for s, r in results.items() if r != "ok"}}
print(len(results) - len(bad), "ok")
for s in sorted(bad):
    print(s, bad[s])
"""


def main() -> None:
    engine = weave.Engine()
    engine.tool("status", status, effect="read", timeout_ms=TIMEOUT_MS)
    started = time.perf_counter()
    run = engine.run(SCRIPT)
    elapsed = time.perf_counter() - started
    print(run.stdout, end="")
    lines = run.stdout.splitlines()
    assert lines[0] == "10 ok", run.stdout
    assert lines[1] == "svc03 error: tool `status` failed after 1 attempt(s): svc03 is down", run.stdout
    assert lines[2].startswith("svc07 error: tool `status` failed after 1 attempt(s): ") and "timed out" in lines[2], run.stdout
    one_by_one = 10 * CALL_S + TIMEOUT_MS / 1000
    print(f"wall {elapsed:.2f} s; the same calls one after another add up to {one_by_one:.1f} s")
    # Everything overlaps, so the run takes about as long as the timeout.
    assert elapsed < 0.5 * one_by_one, elapsed
    print("failing_branch ok")


if __name__ == "__main__":
    main()
