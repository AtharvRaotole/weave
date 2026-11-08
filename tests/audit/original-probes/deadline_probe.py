"""Finite, local worker tests; driver also imposes a process-group deadline."""
import json
import sys
import time
from pathlib import Path

from weave import Engine

HERE = Path(__file__).resolve().parent


def slow(n):
    time.sleep(0.8)
    (HERE / f"deadline-marker-{n}").write_text("completed")
    return n


def main():
    mode = sys.argv[1]
    if mode == "pickle":
        from weave.workers import Pool
        pool = Pool(1)
        try:
            try:
                pool.start(lambda: None, [], {})
            except Exception as exc:
                print(json.dumps({"mode": mode, "error": type(exc).__name__, "idle_slots_after_rejected_call": pool.idle.qsize(), "expected_slots": 1}), flush=True)
        finally:
            pool.close()
        return
    engine = Engine(worker_pool=1)
    engine.set_limits(max_wall_time_ms=100)
    engine.tool("slow", slow, effect="read", hard_deadline=True, timeout_ms=50)
    started = time.monotonic()
    try:
        result = engine.run("a = slow(1)\nb = slow(2)\nc = slow(3)\nprint(a, b, c)\n")
        status = result.stdout
    except Exception as exc:
        status = str(exc)
    at_return = time.monotonic() - started
    print(json.dumps({"mode": mode, "elapsed_s": at_return, "status": status}), flush=True)
    time.sleep(1.1)
    print(json.dumps({"markers": sorted(p.name for p in HERE.glob('deadline-marker-*'))}), flush=True)


if __name__ == "__main__":
    main()
