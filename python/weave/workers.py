"""Killable worker pool for Weave tools that cannot poll a cancel token.

Each worker is a fresh interpreter. A timeout or session cancel SIGKILLs the
worker that is running the call and starts a replacement, so the pool stays
the same size. The function, its arguments, and its return value must be
pickleable. Define tools at module scope and guard the script with
``if __name__ == "__main__"``. Closures and lambdas are not sendable.

Tradeoffs, compared with an in-process thread:

- The deadline is real. ``finally`` blocks, locks, and open files inside the
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
    def __init__(self, pool: "Pool", proc: mp.Process, conn) -> None:
        self.pool = pool
        self.proc = proc
        self.conn = conn
        self._lock = threading.Lock()
        self._settled = False

    def wait(self):
        try:
            status, payload = self.conn.recv()
        except Exception as exc:
            status, payload = "err", f"worker failed: {exc}"
        self._release(replace=not self.proc.is_alive())
        return (status, payload)

    def kill(self) -> None:
        self._release(replace=True)

    def _release(self, replace: bool) -> None:
        with self._lock:
            if self._settled:
                return
            self._settled = True
        if replace:
            if self.proc.is_alive():
                self.proc.kill()
                self.proc.join(timeout=2)
            try:
                self.conn.close()
            except Exception:
                pass
            self.pool._replace()
        else:
            self.pool._recycle(self.proc, self.conn)


class Pool:
    def __init__(self, size: int) -> None:
        if size < 1:
            raise ValueError("worker_pool size must be at least 1")
        self.size = size
        self.ctx = mp.get_context("spawn")
        self.idle: queue.Queue = queue.Queue()
        self._spawn_lock = threading.Lock()
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
            slot = self._spawn()
        self.idle.put(slot)

    def _recycle(self, proc, conn) -> None:
        self.idle.put((proc, conn))

    def start(self, fn, args, kwargs):
        proc, conn = self.idle.get()
        if not proc.is_alive():
            try:
                conn.close()
            except Exception:
                pass
            proc, conn = self._spawn()
        conn.send((fn, tuple(args), dict(kwargs or {})))
        return Job(self, proc, conn)

    def close(self) -> None:
        while True:
            try:
                proc, conn = self.idle.get_nowait()
            except queue.Empty:
                break
            try:
                conn.send(None)
            except Exception:
                pass
            if proc.is_alive():
                proc.kill()
            proc.join(timeout=1)
