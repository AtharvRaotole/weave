"""Killable worker pool for Weave tools that cannot poll a cancel token.

Each worker is a fresh interpreter. A timeout or session cancel SIGKILLs the
worker that is running the call and starts a replacement, so the pool stays
the same size. The function, its arguments, and its return value must be
pickleable. Define tools at module scope and guard the script with
``if __name__ == "__main__"``. Closures and lambdas are not sendable.

Tradeoffs, compared with an in-process thread:

- Process termination is best effort, not a real-time deadline guarantee. ``finally`` blocks, locks, and open files inside the
  worker die with it. Side effects that already left the process (a request
  already sent, a write already flushed to another service) are not undone.
- Fork is not used. Spawn re-imports the main module, so pool startup costs
  one interpreter per slot, and a killed slot pays that cost again.
- Workers do not share the parent's memory. A tool that reads a global mutated
  after the pool started sees the module as imported in the child.
- Only one call runs on a worker at a time. Extra calls wait for a free slot.
"""

from __future__ import annotations

import multiprocessing as mp
import queue
import threading
import traceback


def _serve(conn) -> None:
    conn.send(("ready", None))
    while True:
        try:
            msg = conn.recv()
        except EOFError:
            return
        if msg is None:
            return
        fn, args, kwargs = msg
        try:
            conn.send(("ok", fn(*args, **(kwargs or {}))))
        except Exception:
            conn.send(("err", traceback.format_exc()))


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

    def dispatch(self):
        slot = None
        try:
            while not self._cancel.is_set():
                try:
                    slot = self.pool.idle.get(timeout=.01)
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
                conn.send_bytes(self.payload)
            try:
                result = conn.recv()
            except Exception as exc:
                result = ("err", f"worker failed: {exc}")
            with self._lock:
                if self._slot is not None:
                    self._slot = None
                    if proc.is_alive():
                        self.pool._recycle(proc, conn)
                    else:
                        self.pool._discard(proc, conn)
                    slot = None
                if not self._cancel.is_set():
                    self._result = result
        except Exception:
            with self._lock:
                owned, self._slot = self._slot, None
                if owned is not None:
                    self.pool._discard(*owned)
                self._result = ("err", traceback.format_exc())
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
    def __init__(self, size: int) -> None:
        if size < 1:
            raise ValueError("worker_pool size must be at least 1")
        self.size = size
        self.ctx = mp.get_context("spawn")
        self.idle: queue.Queue = queue.Queue()
        self._spawn_lock = threading.Lock()
        self._jobs_lock = threading.Lock()
        self._jobs = set()
        self._closed = False
        for _ in range(size):
            self._replace()

    def _spawn(self):
        parent, child = self.ctx.Pipe()
        proc = self.ctx.Process(target=_serve, args=(child,))
        proc.start()
        child.close()
        kind, _payload = parent.recv()
        if kind != "ready":
            proc.kill()
            proc.join(timeout=2)
            raise RuntimeError("worker did not start")
        return proc, parent

    def _replace(self) -> None:
        with self._spawn_lock:
            if self._closed:
                return
            slot = self._spawn()
        self._recycle(*slot)

    @staticmethod
    def _terminate(proc, conn):
        if proc.is_alive():
            proc.kill()
        proc.join(timeout=.1)
        conn.close()

    def _discard(self, proc, conn):
        self._terminate(proc, conn)
        # Interpreter startup must not block a runtime timer or cancellation.
        threading.Thread(target=self._replace, daemon=True).start()

    def _recycle(self, proc, conn):
        with self._jobs_lock:
            closed = self._closed
            if not closed:
                self.idle.put((proc, conn))
        if closed:
            self._terminate(proc, conn)

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
