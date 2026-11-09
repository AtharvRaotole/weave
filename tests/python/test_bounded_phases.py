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

    # Run and replay must stay linear in the number of calls. A flat
    # dependency list and a full-trace scan per replayed call once made both
    # quadratic (6000 calls: ~6 s). Linear growth is ~4x for 4x the calls;
    # quadratic would be ~16x. Best of three to keep CI noise out.
    def loop_of(n):
        return f"total = 0\nfor i in range({n}):\n    total = total + len(echo(str(i)))\nprint(total)\n"
    def cost(n):
        source = loop_of(n)
        run_s = replay_s = float("inf")
        for _ in range(3):
            run, elapsed = timed(lambda: engine.run(source))
            run_s = min(run_s, elapsed)
            trace = run.trace_json()
            _, elapsed = timed(lambda: engine.replay(source, trace))
            replay_s = min(replay_s, elapsed)
        return run_s, replay_s
    small, large = cost(2000), cost(8000)
    for phase, a, b in (("run", small[0], large[0]), ("replay", small[1], large[1])):
        ratio = b / max(a, 1e-3)
        print(f"{phase}_8000_over_2000_calls={ratio:.1f}x ({a:.3f}s -> {b:.3f}s)")
        assert ratio < 9.0, (phase, ratio, "run/replay cost is no longer linear in calls")
    results["run_8000_calls_s"], results["replay_8000_calls_s"] = large

    for key, value in results.items():
        print(f"{key}={value:.3f}")
        assert value < BOUND_S, (key, value)
    print("non-preemptible phases stayed within budget-derived bounds")


if __name__ == "__main__":
    main()
