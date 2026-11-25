"""The interpreter exits cleanly while a timed-out or cancelled tool thread is
still finishing. Before the exit hook, CPython could abort with
"PyGILState_Release: thread state must be current" (exit code 134 or 139).
Offline. Run directly."""
import subprocess
import sys
import textwrap
import time

import weave

COOPERATIVE = textwrap.dedent("""
    import time, weave

    def hang(x, cancel):
        while not cancel.is_cancelled():
            time.sleep(0.01)
        time.sleep(0.05)  # still inside Python when the run has returned
        return x

    e = weave.Engine()
    e.tool("hang", hang, effect="read", timeout_ms=50)
    src = "out = []\\nfor i in range(4):\\n    try:\\n        out.append(hang(i))\\n    except ToolError:\\n        out.append(None)\\nprint(out)"
    print(e.run(src).stdout, end="")
""")

PLAIN = textwrap.dedent("""
    import time, weave

    def slow(x):
        for _ in range(20):
            time.sleep(0.01)  # re-enters Python every 10 ms
        return x

    e = weave.Engine()
    e.tool("slow", slow, effect="read", timeout_ms=20)
    try:
        e.run("print(slow(1))")
    except Exception as exc:
        print("timed out" in str(exc))
""")


def run(code):
    return subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, timeout=60)


def test_cooperative_timeout_then_exit_is_clean():
    for _ in range(15):
        proc = run(COOPERATIVE)
        assert proc.returncode == 0, (proc.returncode, proc.stderr[-500:])
        assert proc.stdout == "[None, None, None, None]\n", proc.stdout


def test_plain_timeout_then_exit_is_clean():
    for _ in range(10):
        proc = run(PLAIN)
        assert proc.returncode == 0, (proc.returncode, proc.stderr[-500:])
        assert proc.stdout == "True\n", proc.stdout


def test_a_stuck_tool_delays_exit_by_at_most_the_wait():
    code = textwrap.dedent("""
        import time, weave
        def stuck(x):
            time.sleep(30)
            return x
        e = weave.Engine()
        e.tool("stuck", stuck, effect="read", timeout_ms=20)
        try:
            e.run("print(stuck(1))")
        except Exception:
            pass
    """)
    started = time.monotonic()
    proc = run(code)
    elapsed = time.monotonic() - started
    assert proc.returncode == 0, (proc.returncode, proc.stderr[-500:])
    assert elapsed < 10, elapsed


def test_the_wait_returns_at_once_when_nothing_runs():
    started = time.monotonic()
    assert weave._native._wait_for_tool_threads(5.0) == 0
    assert time.monotonic() - started < 0.5


if __name__ == "__main__":
    failed = 0
    for name, fn in list(globals().items()):
        if name.startswith("test_") and callable(fn):
            try:
                fn()
                print("ok", name)
            except Exception as exc:  # noqa: BLE001
                failed += 1
                print("FAIL", name, repr(exc))
    if failed:
        sys.exit(1)
    print("exit-after-timeout tests passed")
