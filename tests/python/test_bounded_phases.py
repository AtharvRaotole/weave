"""Phases that cancellation cannot interrupt are bounded by input budgets.

Parsing, static checking and replay run on the calling thread and do not poll
cancellation. They are bounded instead: source <= max_source_bytes (1 MiB by
default), nesting is capped, traces <= max_trace_bytes. This test measures the
worst inputs the default budgets admit and fails if any phase becomes slow or
crashes. It is a regression bound, not a real-time guarantee. Offline.
"""
import time

import weave

BOUND_S = 5.0


def timed(fn):
    start = time.monotonic()
    value = fn()
    return value, time.monotonic() - start


def echo(value):
    return value


def main():
    engine = weave.Engine()
    engine.tool("echo", echo, effect="pure", arity=1)
    results = {}

    line = 'x = echo("' + "a" * 60 + '")\n'
    big = line * ((1024 * 1024 - 1) // len(line))
    assert len(big.encode()) < 1024 * 1024
    problems, results["check_1MiB_source_s"] = timed(lambda: engine.check(big))
    assert problems == [], problems[:3]

    too_big = big + "y" * (1024 * 1024)
    def rejected():
        try:
            engine.run(too_big)
        except ValueError as exc:
            return str(exc)
        raise AssertionError("oversized source accepted")
    message, results["reject_2MiB_source_s"] = timed(rejected)
    assert "source" in message.lower(), message

    nested = "x = " + "(" * 100_000 + "1" + ")" * 100_000 + "\n"
    problems, results["check_deep_nesting_s"] = timed(lambda: engine.check(nested))
    assert problems, "pathological nesting must be rejected, not crash"

    loop = "total = 0\nfor i in range(3000):\n    total = total + len(echo(str(i)))\nprint(total)\n"
    run = engine.run(loop)
    trace = run.trace_json()
    replayed, results["replay_3000_calls_s"] = timed(lambda: engine.replay(loop, trace))
    assert replayed.stdout == run.stdout

    for key, value in results.items():
        print(f"{key}={value:.3f}")
        assert value < BOUND_S, (key, value)
    print("non-preemptible phases stayed within budget-derived bounds")


if __name__ == "__main__":
    main()
