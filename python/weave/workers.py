"""Killable worker processes for Weave host tools.

Each worker is a fresh ``spawn`` interpreter. A tool timeout, run deadline,
session cancel or dropped session kills the worker that is running the call
and starts a replacement, so the pool keeps its size. The function, its
arguments and its return value must be pickleable: define tools at module
scope and guard the entry script with ``if __name__ == "__main__"``.

What isolation does:

- On POSIX each worker leads its own process group (``os.setsid``). A kill
  sends ``SIGKILL`` to the whole group, so subprocesses a tool started die too,
  unless they deliberately left the group (``setsid``/daemonizing).
- Optional OS limits (``WorkerLimits``) are applied with ``setrlimit`` inside
  each worker before any tool code runs. A limit that cannot be enforced on this
  platform makes pool startup fail; it is never silently ignored.
- Results larger than ``max_result_bytes`` are rejected and that worker is
  replaced, so one tool cannot make the host unpickle an unbounded payload.

What it does not do:

- It is not a security sandbox or VM. A worker has the host user's file
  system, network and credentials, and its environment is inherited.
- Killing a process does not undo effects that already left it: an HTTP
  request already sent, a file already written, a remote job already started.
- On Windows only the worker process itself is terminated (no process-group or
  Job Object kill), and ``WorkerLimits`` are unsupported.
- Termination is prompt but not real-time: the kill is delivered when the
  deadline fires, and replacement interpreters start in the background.
"""

from __future__ import annotations

from dataclasses import asdict, dataclass
import multiprocessing as mp
import os
import queue
import signal
import sys
import threading
import traceback
from typing import Optional

DEFAULT_MAX_RESULT_BYTES = 16 * 1024 * 1024


@dataclass(frozen=True)
class WorkerLimits:
    """Per-worker OS resource limits (``setrlimit``). ``None`` leaves a limit unset.

    - ``memory_bytes``: address-space cap (``RLIMIT_AS``). Linux only; it counts
      virtual memory, so leave headroom (a bare interpreter maps ~50-100 MB).
    - ``cpu_seconds``: CPU-time cap (``RLIMIT_CPU``). The kernel signals the
      worker when it is exceeded; the call fails and the worker is replaced.
    - ``open_files``: file descriptor cap (``RLIMIT_NOFILE``).
    - ``file_size_bytes``: largest file the worker may write (``RLIMIT_FSIZE``).
    """

    memory_bytes: Optional[int] = None
    cpu_seconds: Optional[int] = None
    open_files: Optional[int] = None
    file_size_bytes: Optional[int] = None

    @classmethod
    def coerce(cls, value) -> "WorkerLimits":
        if value is None:
            return cls()
        if isinstance(value, cls):
            return value
        if isinstance(value, dict):
            unknown = set(value) - set(cls.__dataclass_fields__)
            if unknown:
                raise ValueError(f"unknown worker limit(s): {', '.join(sorted(unknown))}")
            return cls(**value)
        raise TypeError("worker_limits must be a WorkerLimits, a dict, or None")

    def requested(self) -> dict:
        return {k: v for k, v in asdict(self).items() if v is not None}


_RLIMITS = {
    "memory_bytes": "RLIMIT_AS",
    "cpu_seconds": "RLIMIT_CPU",
    "open_files": "RLIMIT_NOFILE",
    "file_size_bytes": "RLIMIT_FSIZE",
}


def check_limits_supported(limits: WorkerLimits) -> None:
    """Fail closed when a requested limit cannot be enforced here."""
    requested = limits.requested()
    for key, value in requested.items():
        if not isinstance(value, int) or isinstance(value, bool) or value < 1:
            raise ValueError(f"worker limit {key} must be a positive integer")
    if not requested:
        return
    if os.name != "posix":
        raise ValueError("worker_limits are only enforced on POSIX systems; remove them on this platform")
    if "memory_bytes" in requested and not sys.platform.startswith("linux"):
        raise ValueError("worker_limits.memory_bytes (RLIMIT_AS) is only enforced on Linux")


def _apply_limits(requested: dict) -> dict:
    import resource

    applied = {}
    for key, value in requested.items():
        which = getattr(resource, _RLIMITS[key])
        _soft, hard = resource.getrlimit(which)
        if hard != resource.RLIM_INFINITY and value > hard:
            raise ValueError(f"{key}={value} exceeds the hard limit {hard}")
        # CPU: hard limit one second above soft, so SIGXCPU fires first and the
        # kernel SIGKILLs a worker that ignores it.
        new_hard = value + 1 if key == "cpu_seconds" and (hard == resource.RLIM_INFINITY or value + 1 <= hard) else value
        resource.setrlimit(which, (value, new_hard))
        applied[key] = resource.getrlimit(which)[0]
    return applied


def _serve(conn, requested: dict, max_result_bytes: int, debug: bool) -> None:
    applied: dict = {}
    try:
        if os.name == "posix":
            os.setsid()
        applied = _apply_limits(requested) if requested else {}
    except BaseException as exc:  # noqa: BLE001 - reported to the parent, which fails closed
        conn.send(("failed", f"{type(exc).__name__}: {exc}"))
        return
    conn.send(("ready", {"limits": applied, "process_group": os.name == "posix"}))
    from weave.errors import describe_exception

    while True:
        try:
            msg = conn.recv_bytes()
        except (EOFError, OSError):
            return
        try:
            fn, args, kwargs = mp.reduction.ForkingPickler.loads(msg)
            value = fn(*args, **(kwargs or {}))
            payload = mp.reduction.ForkingPickler.dumps(("ok", value))
            if len(payload) > max_result_bytes:
                payload = mp.reduction.ForkingPickler.dumps(
                    ("err", f"tool result exceeds the {max_result_bytes}-byte worker result limit")
                )
        except BaseException as exc:  # noqa: BLE001 - every failure returns to the host
            if isinstance(exc, (KeyboardInterrupt, SystemExit)):
                return
            payload = mp.reduction.ForkingPickler.dumps(("err", describe_exception(exc, debug=debug)))
        try:
            conn.send_bytes(payload)
        except (OSError, ValueError):
            return


def _signal_name(code: Optional[int]) -> str:
    if code is None or code >= 0:
        return f"exit code {code}"
    try:
        return signal.Signals(-code).name
    except ValueError:
        return f"signal {-code}"


class Job:
    """Own a queued or active invocation until completion or cancellation.

    start() never waits for capacity on a runtime thread. The dispatcher owns
    the slot, and serializes dispatch vs kill under the same lock.
    """

    def __init__(self, pool, payload):
        self.pool = pool
        self.payload = payload
        self._lock = threading.Lock()
        self._done = threading.Event()
        self._cancel = threading.Event()
        self._slot = None
        self._result = ("err", "execution was cancelled")
        self.pid = None

    def dispatch(self):
        slot = None
        try:
            while not self._cancel.is_set():
                try:
                    slot = self.pool.idle.get(timeout=0.01)
                    break
                except queue.Empty:
                    continue
            if slot is None:
                return
            proc, conn = slot
            with self._lock:
                if self._cancel.is_set() or self.pool._closed:
                    self.pool._recycle(proc, conn)
                    slot = None
                    return
                self._slot = slot
                self.pid = proc.pid
                conn.send_bytes(self.payload)
            healthy = True
            try:
                raw = conn.recv_bytes(self.pool.max_result_bytes + 4096)
                result = mp.reduction.ForkingPickler.loads(raw)
            except EOFError:
                proc.join(timeout=0.5)
                healthy = False
                result = ("err", f"worker process died ({_signal_name(proc.exitcode)}); it was replaced")
            except OSError:
                healthy = False
                result = ("err", f"tool result exceeds the {self.pool.max_result_bytes}-byte worker result limit")
            except Exception as exc:  # noqa: BLE001
                healthy = False
                result = ("err", f"worker failed: {type(exc).__name__}")
            with self._lock:
                if self._slot is not None:
                    self._slot = None
                    if healthy and proc.is_alive():
                        self.pool._recycle(proc, conn)
                    else:
                        self.pool._discard(proc, conn)
                    slot = None
                if not self._cancel.is_set():
                    self._result = result
        except Exception as exc:  # noqa: BLE001
            with self._lock:
                owned, self._slot = self._slot, None
                if owned is not None:
                    self.pool._discard(*owned)
                self._result = ("err", f"worker dispatch failed: {type(exc).__name__}")
        finally:
            self._done.set()
            with self.pool._jobs_lock:
                self.pool._jobs.discard(self)

    def wait(self):
        self._done.wait()
        return self._result

    def kill(self):
        with self._lock:
            self._cancel.set()
            slot, self._slot = self._slot, None
            if slot is not None:
                self.pool._discard(*slot)
            self._done.set()


class Pool:
    def __init__(
        self,
        size: int,
        *,
        limits=None,
        max_result_bytes: int = DEFAULT_MAX_RESULT_BYTES,
        debug: bool = False,
    ) -> None:
        if size < 1:
            raise ValueError("worker_pool size must be at least 1")
        if max_result_bytes < 1:
            raise ValueError("max_result_bytes must be positive")
        self.size = size
        self.limits = WorkerLimits.coerce(limits)
        check_limits_supported(self.limits)
        self.max_result_bytes = max_result_bytes
        self.debug = debug
        self.applied_limits: dict = {}
        self.process_group_kill = os.name == "posix"
        self.ctx = mp.get_context("spawn")
        self.idle: queue.Queue = queue.Queue()
        self._spawn_lock = threading.Lock()
        self._jobs_lock = threading.Lock()
        self._jobs = set()
        self._closed = False
        try:
            for _ in range(size):
                self._replace()
        except BaseException:
            self.close()
            raise

    def describe(self) -> dict:
        """What this pool enforces, for logs and health checks."""
        return {
            "workers": self.size,
            "start_method": "spawn",
            "kill": "SIGKILL process group" if self.process_group_kill else "terminate worker process only",
            "limits": dict(self.applied_limits),
            "max_result_bytes": self.max_result_bytes,
        }

    def _spawn(self):
        parent, child = self.ctx.Pipe()
        proc = self.ctx.Process(
            target=_serve,
            args=(child, self.limits.requested(), self.max_result_bytes, self.debug),
        )
        proc.start()
        child.close()
        try:
            if not parent.poll(60):
                raise RuntimeError("worker did not start within 60s")
            kind, payload = parent.recv()
        except BaseException:
            self._terminate(proc, parent)
            raise
        if kind != "ready":
            self._terminate(proc, parent)
            raise RuntimeError(f"worker could not apply its limits: {payload}")
        self.applied_limits = payload["limits"]
        return proc, parent

    def _replace(self) -> None:
        with self._spawn_lock:
            if self._closed:
                return
            slot = self._spawn()
        self._recycle(*slot)

    def _terminate(self, proc, conn):
        if self.process_group_kill and proc.pid:
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
        if proc.is_alive():
            proc.kill()
        proc.join(timeout=1)
        conn.close()

    def _discard(self, proc, conn):
        self._terminate(proc, conn)
        # Interpreter startup must not block a runtime timer or cancellation.
        threading.Thread(target=self._replace_quietly, daemon=True).start()

    def _replace_quietly(self):
        try:
            self._replace()
        except Exception:  # noqa: BLE001 - next start() reports capacity loss
            traceback.print_exc(limit=0)

    def _recycle(self, proc, conn):
        with self._jobs_lock:
            closed = self._closed
            if not closed:
                self.idle.put((proc, conn))
        if closed:
            self._terminate(proc, conn)

    def check_sendable(self, fn) -> None:
        """Raise now if ``fn`` cannot be sent to a worker (closures, lambdas)."""
        try:
            mp.reduction.ForkingPickler.dumps(fn)
        except Exception as exc:
            name = getattr(fn, "__qualname__", repr(fn))
            raise ValueError(
                f"tool `{name}` cannot run in a worker process ({type(exc).__name__}); "
                "define it at module scope, or register it with hard_deadline=False to run in-process"
            ) from None

    def start(self, fn, args, kwargs):
        # Fail before acquiring a slot, including for unpickleable functions.
        payload = mp.reduction.ForkingPickler.dumps((fn, tuple(args), dict(kwargs or {})))
        job = Job(self, payload)
        with self._jobs_lock:
            if self._closed:
                raise RuntimeError("worker pool is closed")
            self._jobs.add(job)
        threading.Thread(target=job.dispatch, daemon=True).start()
        return job

    def close(self) -> None:
        with self._jobs_lock:
            self._closed = True
            jobs = list(self._jobs)
        for job in jobs:
            job.kill()
        while True:
            try:
                proc, conn = self.idle.get_nowait()
            except queue.Empty:
                break
            self._terminate(proc, conn)
