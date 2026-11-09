"""Process-isolation guarantees for host tools. Offline; POSIX parts skip elsewhere.

Each test states the guarantee it proves. Run directly: python tests/python/test_isolation.py
"""
import os
from pathlib import Path
import sys
import tempfile
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))

import weave  # noqa: E402
from isolation_tools import (  # noqa: E402
    big_result, burn_cpu, echo, hog, open_many, secret_failure, sleeper, spawner, spinner, write_big,
)

POSIX = os.name == "posix"
LINUX = sys.platform.startswith("linux")
# Generous bound: kill + reply, never the 60s the tool wanted. CI runners are slow.
BOUND = 3.0


def alive(pid):
    if os.name == "nt":
        import ctypes
        kernel32 = ctypes.windll.kernel32
        handle = kernel32.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
        if not handle:
            return False
        code = ctypes.c_ulong()
        kernel32.GetExitCodeProcess(handle, ctypes.byref(code))
        kernel32.CloseHandle(handle)
        return code.value == 259  # STILL_ACTIVE
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    # A zombie still answers kill(0); check /proc state on Linux.
    stat = Path(f"/proc/{pid}/stat")
    if stat.exists():
        return stat.read_text().split(")")[-1].split()[0] != "Z"
    return True


def wait_dead(pid, seconds=3.0):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if not alive(pid):
            return True
        time.sleep(0.02)
    return not alive(pid)


def expect_error(engine, source, *needles):
    try:
        engine.run(source)
    except ValueError as exc:
        text = str(exc)
        for needle in needles:
            assert needle in text, (needle, text)
        return text
    raise AssertionError("expected the run to fail")


def test_tool_timeout_kills_runaway_sleep(tmp):
    """A sync tool sleeping 60s with timeout_ms=200 returns promptly and its worker is dead."""
    engine = weave.Engine(worker_pool=1, isolate_all=True)
    engine.tool("sleeper", sleeper, effect="read", timeout_ms=200)
    path = str(tmp / "sleep.pid")
    start = time.monotonic()
    expect_error(engine, f"sleeper({path!r})\n", "timed out after 200ms")
    elapsed = time.monotonic() - start
    assert elapsed < BOUND, elapsed
    pid = int(Path(path).read_text())
    assert wait_dead(pid), "worker still running after timeout"
    assert not Path(path + ".finished").exists()
    # The pool replaced the worker: the next call works.
    engine.tool("echo", echo, effect="pure")
    assert engine.run('print(echo("after"))\n').stdout == "after\n"
    return elapsed


def test_run_deadline_kills_cpu_spin(tmp):
    """A busy loop with no tool timeout is killed by the run's max_wall_time."""
    engine = weave.Engine(worker_pool=1, isolate_all=True)
    engine.tool("spinner", spinner, effect="read")
    engine.set_limits(max_wall_time_ms=300)
    path = str(tmp / "spin.pid")
    start = time.monotonic()
    expect_error(engine, f"spinner({path!r})\n")
    elapsed = time.monotonic() - start
    assert elapsed < BOUND, elapsed
    assert wait_dead(int(Path(path).read_text()))
    return elapsed


def test_session_cancel_kills_worker(tmp):
    engine = weave.Engine(worker_pool=1, isolate_all=True)
    engine.tool("sleeper", sleeper, effect="read")
    path = str(tmp / "cancel.pid")
    session = engine.session()
    session.push(f"x = sleeper({path!r})\n")
    for _ in range(300):
        if Path(path).exists():
            break
        time.sleep(0.01)
    session.cancel()
    try:
        session.finish()
    except ValueError:
        pass
    assert wait_dead(int(Path(path).read_text()))


def test_process_group_kill_reaches_grandchildren(tmp):
    """POSIX: subprocesses a tool started die with the worker."""
    if not POSIX:
        return "skipped (no process groups)"
    engine = weave.Engine(worker_pool=1, isolate_all=True)
    engine.tool("spawner", spawner, effect="read", timeout_ms=1500)
    path = tmp / "spawn.pids"
    expect_error(engine, f"spawner({str(path)!r})\n", "timed out")
    worker, child = map(int, path.read_text().split())
    assert wait_dead(worker), "worker survived"
    assert wait_dead(child), "grandchild survived the process-group kill"
    assert engine.isolation()["pool"]["kill"] == "SIGKILL process group"


def test_memory_limit(tmp):
    """Linux: RLIMIT_AS turns a 2 GiB allocation into a tool error; the pool recovers."""
    if not LINUX:
        try:
            weave.Engine(worker_pool=1, isolate_all=True, worker_limits={"memory_bytes": 1 << 30})
        except ValueError as exc:
            assert "only enforced on Linux" in str(exc) or "POSIX" in str(exc)
            return "fails closed on this platform"
        raise AssertionError("unsupported memory limit was accepted")
    limit = 768 * 1024 * 1024
    engine = weave.Engine(worker_pool=1, isolate_all=True, worker_limits=weave.WorkerLimits(memory_bytes=limit))
    engine.tool("hog", hog, effect="pure")
    assert engine.isolation()["pool"]["limits"]["memory_bytes"] == limit
    assert engine.run("print(hog(16))\n").stdout == str(16 * 1024 * 1024) + "\n"
    expect_error(engine, "hog(2048)\n", "MemoryError")
    assert engine.run("print(hog(8))\n").stdout == str(8 * 1024 * 1024) + "\n"


def test_cpu_limit(tmp):
    """POSIX: RLIMIT_CPU stops a worker that burns CPU even with no deadline."""
    if not POSIX:
        return "skipped"
    engine = weave.Engine(worker_pool=1, isolate_all=True, worker_limits={"cpu_seconds": 1})
    engine.tool("burn_cpu", burn_cpu, effect="read")
    path = str(tmp / "cpu.pid")
    start = time.monotonic()
    text = expect_error(engine, f"burn_cpu({path!r})\n", "worker process died")
    assert time.monotonic() - start < 10
    assert "SIGXCPU" in text or "SIGKILL" in text, text
    engine.tool("echo", echo, effect="pure")
    assert engine.run('print(echo(1))\n').stdout == "1\n"


def test_open_file_and_file_size_limits(tmp):
    if not POSIX:
        return "skipped"
    engine = weave.Engine(worker_pool=1, isolate_all=True,
                          worker_limits={"open_files": 64, "file_size_bytes": 1024 * 1024})
    engine.tool("open_many", open_many, effect="read")
    engine.tool("write_big", write_big, effect="irreversible")
    expect_error(engine, "open_many(500)\n", "OSError")
    target = str(tmp / "big.bin")
    expect_error(engine, f"write_big({target!r}, 4)\n")
    assert Path(target).stat().st_size <= 1024 * 1024


def test_result_size_cap(tmp):
    """A worker cannot make the host unpickle more than max_result_bytes."""
    engine = weave.Engine(worker_pool=1, isolate_all=True, max_result_bytes=1024 * 1024)
    engine.tool("big_result", big_result, effect="pure")
    engine.set_limits(max_value_size=64 * 1024 * 1024)
    expect_error(engine, "big_result(4)\n", "worker result limit")
    assert engine.isolation()["pool"]["max_result_bytes"] == 1024 * 1024


def test_isolate_all_rejects_unsendable_and_reports_modes(tmp):
    engine = weave.Engine(worker_pool=1, isolate_all=True)
    try:
        engine.tool("closure", lambda: 1, effect="pure")
    except ValueError as exc:
        assert "module scope" in str(exc), exc
    else:
        raise AssertionError("lambda accepted for worker isolation")
    engine.tool("local", lambda: 2, effect="pure", hard_deadline=False)
    engine.tool("echo", echo, effect="pure")
    info = engine.isolation()
    assert info["isolate_all"] is True
    assert info["isolated_tools"] == ["echo"] and info["in_process_tools"] == ["local"], info

    def cooperative(cancel):
        return 1
    try:
        engine.tool("coop", cooperative, effect="pure")
    except ValueError as exc:
        assert "cancel token" in str(exc)
    else:
        raise AssertionError("cancel-token tool silently isolated")


def test_options_require_pool_and_fail_closed(tmp):
    for kwargs in ({"isolate_all": True}, {"worker_limits": {"cpu_seconds": 1}}, {"max_result_bytes": 10}):
        try:
            weave.Engine(**kwargs)
        except ValueError:
            continue
        raise AssertionError(f"accepted {kwargs} without a pool")
    for bad in ({"memory_bytes": 0}, {"nope": 1}):
        try:
            weave.Engine(worker_pool=1, worker_limits=bad)
        except (ValueError, TypeError):
            continue
        raise AssertionError(f"accepted bad limits {bad}")


def test_worker_errors_are_redacted_by_default(tmp):
    """Host exception text (here a planted secret) does not reach the script unless debug=True."""
    secret = "PLANTED-SECRET-7f3a"
    engine = weave.Engine(worker_pool=1, isolate_all=True)
    engine.tool("secret_failure", secret_failure, effect="read")
    text = expect_error(engine, f"secret_failure({secret!r})\n", "RuntimeError", "redacted")
    assert secret not in text, text
    debug = weave.Engine(worker_pool=1, isolate_all=True, debug=True)
    debug.tool("secret_failure", secret_failure, effect="read")
    assert secret in expect_error(debug, f"secret_failure({secret!r})\n")


def test_weave_tool_worker_pool(tmp):
    """WeaveTool(worker_pool=N) isolates every function and kills runaways."""
    from isolation_tools import sleeper as slow_tool
    tool = weave.WeaveTool([weave.declare(effect="read", timeout_ms=200)(slow_tool),
                            weave.declare(effect="pure")(echo)], worker_pool=1, max_wall_time_ms=5000)
    info = tool.engine.isolation()
    assert sorted(info["isolated_tools"]) == ["echo", "sleeper"], info
    path = str(tmp / "agent.pid")
    start = time.monotonic()
    out = tool.run(f"sleeper({path!r})\n")
    assert out.startswith("error:") and "timed out" in out, out
    assert time.monotonic() - start < BOUND
    assert wait_dead(int(Path(path).read_text()))
    assert tool.run('print(echo("ok"))\n') == "ok\n"


def main():
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    failures = 0
    for test in tests:
        with tempfile.TemporaryDirectory() as tmp:
            try:
                note = test(Path(tmp))
                print(f"ok {test.__name__}" + (f" ({note})" if note else ""), flush=True)
            except Exception as exc:  # noqa: BLE001
                failures += 1
                print(f"FAIL {test.__name__}: {type(exc).__name__}: {exc}", flush=True)
    print(f"{len(tests) - failures}/{len(tests)} isolation tests passed")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
