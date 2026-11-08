"""Shared measurement helpers. Numbers come only from a live run."""

from __future__ import annotations

import os
import platform
import socket
import statistics
import sys
import time
from dataclasses import dataclass, field


@dataclass
class Probe:
    events: list[tuple[float, float]] = field(default_factory=list)

    def wrap(self, fn):
        def inner(*args, **kwargs):
            started = time.perf_counter()
            try:
                return fn(*args, **kwargs)
            finally:
                self.events.append((started, time.perf_counter()))

        return inner

    def reset(self) -> None:
        self.events.clear()


def overlap_rounds(events: list[tuple[float, float]]) -> int:
    """Count non-overlapping waves from measured start/end timestamps.

    A call joins the current wave when it starts before that wave's latest
    finish. The next call that starts after the wave has finished opens a
    new wave. That is the observed round-trip count, not a planned one.
    """
    if not events:
        return 0
    ordered = sorted(events)
    rounds = 0
    wave_end = -1.0
    for start, end in ordered:
        if start >= wave_end:
            rounds += 1
            wave_end = end
        else:
            wave_end = max(wave_end, end)
    return rounds


def machine_info() -> dict:
    return {
        "python": sys.version.split()[0],
        "platform": platform.platform(),
        "machine": platform.machine(),
        "processor": platform.processor() or platform.machine(),
        "cpu_count": os.cpu_count(),
        "hostname": socket.gethostname(),
    }


def median(values: list[float]) -> float:
    return float(statistics.median(values))


def utc_stamp() -> str:
    return time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
