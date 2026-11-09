"""Module-level tools for the isolation tests (workers import them by name)."""
import os
from pathlib import Path
import subprocess
import sys
import time


def record_pid(path):
    Path(path).write_text(str(os.getpid()))


def sleeper(path):
    record_pid(path)
    time.sleep(60)
    Path(path + ".finished").write_text("ran to completion")
    return "late"


def spinner(path):
    record_pid(path)
    while True:
        pass


def spawner(path):
    child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
    Path(path).write_text(f"{os.getpid()} {child.pid}")
    time.sleep(60)


def hog(megabytes):
    blob = bytearray(megabytes * 1024 * 1024)
    return len(blob)


def burn_cpu(path):
    record_pid(path)
    deadline = time.time() + 30
    while time.time() < deadline:
        pass
    return "survived"


def open_many(count):
    handles = [open(os.devnull) for _ in range(count)]
    return len(handles)


def write_big(path, megabytes):
    with open(path, "wb") as handle:
        handle.write(b"x" * megabytes * 1024 * 1024)
    return "wrote"


def big_result(megabytes):
    return "x" * (megabytes * 1024 * 1024)


def echo(value):
    return value


def secret_failure(token):
    raise RuntimeError(f"database password is {token}")
