"""Learn each BFCL function's real return shape from ground-truth executions on TRAIN tasks only (no model calls).

BFCL's function docs carry a `response` schema, but some do not match what the backend returns (round 8: scripts read
`order_history` where the result has `history`, or a nested `tirePressure` that is flat), which cost the Weave arm a
round trip each time. A developer would declare the real return type; here it is sampled, as
`weave.agent_tool.shape_of_samples` does for any tool, from executing BFCL's ground-truth calls on the train split.
The round-9 evidence tasks are excluded.

    OPENAI_API_KEY=x PYTHONPATH=python:. /tmp/bfcl312/bin/python bench/bfcl/learn_shapes.py
"""

from __future__ import annotations

import collections
import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, ROOT)
sys.path.insert(0, os.path.join(ROOT, "python"))
OUT = os.path.join(ROOT, "bench", "bfcl", "shapes.json")


def main():
    from bfcl_eval.eval_checker.multi_turn_eval.multi_turn_utils import execute_multi_turn_func_call
    from bfcl_eval.utils import load_dataset_entry, load_ground_truth_entry

    from bench.bfcl.subsets import TRAIN
    from bench.bfcl.weave_arm import parse_result
    from weave.agent_tool import shape_of_samples

    entries = {e["id"]: e for e in load_dataset_entry("multi_turn_base")}
    gts = {g["id"]: g for g in load_ground_truth_entry("multi_turn_base")}
    samples = collections.defaultdict(list)
    for i in TRAIN:
        tid = f"multi_turn_base_{i}"
        e = entries[tid]
        for turn in gts[tid]["ground_truth"]:
            if not turn:
                continue
            outs, _ = execute_multi_turn_func_call(turn, e["initial_config"], e["involved_classes"], "shapes_learn",
                                                   tid, long_context=False, is_evaL_run=False)
            for call, out in zip(turn, outs):
                name = re.match(r"\s*(\w+)\s*\(", call).group(1)
                if not str(out).startswith("Error during execution"):
                    samples[name].append(parse_result(out))
    # Functions the ground truth never calls but that need no arguments (e.g. get_order_history): call them once on a
    # throwaway instance of each train task's initial state. The instance is never graded or reused.
    for i in TRAIN:
        tid = f"multi_turn_base_{i}"
        e = entries[tid]
        for doc in e["function"]:
            name = doc["name"]
            if name in samples and len(samples[name]) >= 5 or doc.get("parameters", {}).get("required"):
                continue
            outs, _ = execute_multi_turn_func_call([f"{name}()"], e["initial_config"], e["involved_classes"],
                                                   f"shapes_probe_{name}", tid, long_context=False, is_evaL_run=False)
            if not str(outs[0]).startswith("Error during execution"):
                samples[name].append(parse_result(outs[0]))
    shapes = {name: shape_of_samples(vals[:50]) for name, vals in sorted(samples.items())}
    with open(OUT, "w") as f:
        json.dump({"source": "ground-truth executions on bench.bfcl.subsets.TRAIN", "tasks": len(TRAIN),
                   "shapes": shapes}, f, indent=1, sort_keys=True)
        f.write("\n")
    print(f"{len(shapes)} function shapes from {sum(map(len, samples.values()))} results -> {OUT}")


if __name__ == "__main__":
    main()
