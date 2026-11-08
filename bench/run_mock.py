"""Run the mock-latency suite and write bench/results.

Usage (from the repo root, with the weave module installed):

    python bench/run_mock.py
    BENCH_LATENCY_MS=80 BENCH_REPEATS=5 python bench/run_mock.py
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from bench.harness import machine_info, median, utc_stamp  # noqa: E402
from bench.runners import run_dag, run_gather, run_monty, run_sequential, run_weave  # noqa: E402
from bench.tasks import build_tasks  # noqa: E402

RUNNERS = (
    ("sequential", run_sequential),
    ("asyncio_gather", run_gather),
    ("dag", run_dag),
    ("weave", run_weave),
    ("monty", run_monty),
)


def _sample_record(sample) -> dict:
    return {
        "wall_s": sample.wall_s,
        "rounds": sample.rounds,
        "tool_calls": sample.tool_calls,
        "correct": sample.correct,
        "output": sample.output,
    }


def measure_task(task, repeats: int) -> dict:
    runners = {}
    for name, fn in RUNNERS:
        first = fn(task)
        if first is None:
            runners[name] = {
                "skipped": True,
                "reason": "pydantic-monty is not installed",
            }
            continue
        samples = [_sample_record(first)]
        if not first.correct:
            runners[name] = {
                "skipped": False,
                "samples": samples,
                "correct": False,
            }
            continue
        for _ in range(repeats - 1):
            samples.append(_sample_record(fn(task)))
        walls = [item["wall_s"] for item in samples]
        runners[name] = {
            "skipped": False,
            "samples": samples,
            "median_wall_s": median(walls),
            "rounds": samples[walls.index(median(walls)) if median(walls) in walls else 0]["rounds"],
            "tool_calls": samples[0]["tool_calls"],
            "correct": all(item["correct"] for item in samples),
        }
        # Pick rounds from the sample whose wall is closest to the median.
        closest = min(samples, key=lambda item: abs(item["wall_s"] - runners[name]["median_wall_s"]))
        runners[name]["rounds"] = closest["rounds"]
    return {
        "name": task.name,
        "summary": task.summary,
        "expected": task.expected,
        "runners": runners,
    }


def main() -> int:
    base_s = float(os.environ.get("BENCH_LATENCY_MS", "40")) / 1000.0
    repeats = int(os.environ.get("BENCH_REPEATS", "3"))
    tasks = build_tasks(base_s)
    payload = {
        "kind": "mock",
        "latency_s": base_s,
        "repeats": repeats,
        "machine": machine_info(),
        "notes": {
            "rounds": "Observed waves from tool start/end timestamps. A call joins the current wave when it starts before that wave finishes.",
            "asyncio_gather": "Hand-written batches. trip_plan waits for weather before flights and hotels.",
            "dag": "LLMCompiler-style: each wave is every node whose dependencies are already done.",
            "monty": "Pydantic Monty feed_run on a warm worker, if pydantic-monty imports.",
            "weave": "One Weave script per task. Tools are tagged read.",
        },
        "tasks": [measure_task(task, repeats) for task in tasks],
    }
    out_dir = ROOT / "bench" / "results"
    out_dir.mkdir(parents=True, exist_ok=True)
    stamp = utc_stamp()
    path = out_dir / f"mock-{stamp}.json"
    text = json.dumps(payload, indent=2) + "\n"
    path.write_text(text)
    latest = out_dir / "mock-latest.json"
    latest.write_text(text)
    print(f"wrote {path.relative_to(ROOT)}")
    failed = False
    for task in payload["tasks"]:
        print(task["name"])
        for name, row in task["runners"].items():
            if row.get("skipped"):
                print(f"  {name}: skipped ({row['reason']})")
                continue
            if "median_wall_s" not in row or not row["correct"]:
                failed = True
                print(f"  {name}: incorrect output={row['samples'][0]['output']!r}")
                continue
            print(
                f"  {name}: median_wall_s={row['median_wall_s']:.6f} "
                f"rounds={row['rounds']} tool_calls={row['tool_calls']} correct={row['correct']}"
            )
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
