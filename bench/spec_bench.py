"""Speculative pre-execution on the mock suite.

The predictor follows the tool-cache idea in Nichols et al., arXiv 2512.15834
(the engine-side, phase-2 design): guess pure/read calls, run them before the
script exists, and key the results by tool name plus canonical arguments.
Data-dependent calls are not guessed. One extra unused call per task is
predicted so wasted work shows up in the measurement.

Generation is a fixed sleep. The baseline waits for the whole sleep, then
runs Weave. Speculation returns the prediction after a shorter sleep, starts
those calls, then waits until the same generation instant before running.

Numbers are written only from this process. Nothing here is filled in by hand.
"""

from __future__ import annotations

import json
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from bench.harness import Probe, machine_info, median, utc_stamp  # noqa: E402
from bench.tasks import Task, build_tasks, tool_functions  # noqa: E402

GEN_S = 0.080
PRED_S = 0.010
BASE_S = 0.040
REPEATS = 3

# One prediction per task that no program call uses.
WASTE = {
    "research_brief": ("search", ["nope"]),
    "trip_plan": ("geo", ["LON"]),
    "support_ticket": ("lookup_user", ["other"]),
}


class LiteralPredictor:
    """Predict calls whose arguments are already literals, plus one waste."""

    def __init__(self, task: Task) -> None:
        self.task = task

    def predict(self, hint: str) -> list:
        if hint != self.task.name:
            raise RuntimeError(f"predictor hint {hint!r} != {self.task.name}")
        calls = []
        for node in self.task.nodes:
            if node.deps:
                continue
            calls.append((node.tool, list(node.arg_specs), {}))
        tool, args = WASTE[self.task.name]
        calls.append((tool, args, {}))
        return calls


def make_engine(task: Task):
    from weave import Engine

    probe = Probe()
    engine = Engine()
    raw = tool_functions(task)
    arity = {name: count for name, count, _delay in task.tools}
    for name, fn in raw.items():
        engine.tool(name, probe.wrap(fn), effect="read", arity=arity[name])
    return engine, probe


def run_after_generate(task: Task) -> dict:
    engine, probe = make_engine(task)
    started = time.perf_counter()
    time.sleep(GEN_S)
    result = engine.run(task.weave_source)
    return _record(task, result, probe, time.perf_counter() - started)


def run_speculative(task: Task) -> dict:
    engine, probe = make_engine(task)
    started = time.perf_counter()
    time.sleep(PRED_S)
    cache = engine.speculate_with(LiteralPredictor(task), task.name)
    remaining = GEN_S - (time.perf_counter() - started)
    if remaining > 0:
        time.sleep(remaining)
    result = engine.run(task.weave_source, speculation=cache)
    return _record(task, result, probe, time.perf_counter() - started)


def _record(task: Task, result, probe: Probe, wall_s: float) -> dict:
    output = result.stdout.strip()
    hits = result.spec_hits
    misses = result.spec_misses
    denom = hits + misses
    return {
        "wall_s": wall_s,
        "correct": output == task.expected,
        "output": output,
        "host_calls": len(probe.events),
        "program_calls": result.trace_json().count('"tool"'),
        "spec_started": result.spec_started,
        "spec_hits": hits,
        "spec_misses": misses,
        "spec_wasted": result.spec_wasted,
        "spec_rejected": result.spec_rejected,
        "hit_rate": (hits / denom) if denom else 0.0,
    }


def _median_block(samples: list[dict]) -> dict:
    walls = [item["wall_s"] for item in samples]
    return {
        "samples": samples,
        "median_wall_s": median(walls),
        "correct": all(item["correct"] for item in samples),
        "spec_started": samples[0]["spec_started"],
        "spec_hits": samples[0]["spec_hits"],
        "spec_misses": samples[0]["spec_misses"],
        "spec_wasted": samples[0]["spec_wasted"],
        "spec_rejected": samples[0]["spec_rejected"],
        "hit_rate": samples[0]["hit_rate"],
        "host_calls": samples[0]["host_calls"],
    }


def main() -> None:
    from weave import Engine

    # Exercise the explicit call list once. This sample is not part of the table.
    engine = Engine()

    def echo(query: str) -> str:
        return query

    engine.tool("search", echo, effect="pure", arity=1)
    cache = engine.speculate([("search", ["q"]), ("search", ["other"])])
    probed = engine.run('print(search("q"))\n', speculation=cache)
    if probed.stdout != "q\n" or probed.spec_hits != 1 or probed.spec_wasted != 1:
        raise SystemExit(
            f"speculate() smoke failed hits={probed.spec_hits} wasted={probed.spec_wasted} out={probed.stdout!r}"
        )

    tasks = build_tasks(BASE_S)
    measured = []
    for task in tasks:
        after = [run_after_generate(task) for _ in range(REPEATS)]
        spec = [run_speculative(task) for _ in range(REPEATS)]
        if not all(item["correct"] for item in after + spec):
            raise SystemExit(f"{task.name} produced a wrong output: {after} {spec}")
        measured.append(
            {
                "name": task.name,
                "expected": task.expected,
                "run_after_generate": _median_block(after),
                "speculative": _median_block(spec),
            }
        )
    payload = {
        "kind": "speculation",
        "measured": True,
        "reference": "arXiv:2512.15834 tool cache, pure/read calls only",
        "generation_s": GEN_S,
        "predictor_s": PRED_S,
        "base_tool_s": BASE_S,
        "repeats": REPEATS,
        "machine": machine_info(),
        "notes": {
            "run_after_generate": "Sleep for the full generation, then Engine.run with no cache.",
            "speculative": "Sleep for the predictor, start predicted pure/read calls, then Engine.run at the same generation instant.",
            "hit_rate": "hits / (hits + misses) over pure and read calls the program actually made.",
            "wasted": "Predictions the program never called. One unused argument is predicted per task.",
        },
        "tasks": measured,
    }
    out_dir = ROOT / "bench" / "results"
    out_dir.mkdir(parents=True, exist_ok=True)
    stamp = utc_stamp()
    path = out_dir / f"spec-{stamp}.json"
    text = json.dumps(payload, indent=2) + "\n"
    path.write_text(text)
    (out_dir / "spec-latest.json").write_text(text)
    for task in measured:
        after = task["run_after_generate"]["median_wall_s"]
        spec = task["speculative"]["median_wall_s"]
        block = task["speculative"]
        print(
            f"{task['name']}: after={after:.6f}s spec={spec:.6f}s "
            f"hits={block['spec_hits']} misses={block['spec_misses']} "
            f"wasted={block['spec_wasted']} hit_rate={block['hit_rate']:.3f}"
        )
    print(path)


if __name__ == "__main__":
    main()
