"""Learn which BFCL functions only read, by differential execution of TRAIN ground-truth calls (no model calls).

BFCL's backends are stateful objects and its docs do not say which functions change state, so the adapter declares
every function a reversible write (program order). The write gate (`WeaveTool(write_gate=...)`) needs to tell reads
from writes. Each ground-truth call of the 140 train tasks is executed one at a time on a throwaway instance, and the
backend's state is compared before and after the call. A function is a read when it never changed state in any
sample; a function never sampled stays a write (the safe side). Evaluation tasks are not used.

    OPENAI_API_KEY=x PYTHONPATH=python:. /tmp/bfcl312/bin/python bench/bfcl/learn_effects.py
"""

from __future__ import annotations

import collections
import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, ROOT)
OUT = os.path.join(ROOT, "bench", "bfcl", "effects.json")


def state(obj, depth=0, seen=None):
    """A comparable snapshot of a backend object (its attributes, recursively)."""
    seen = seen if seen is not None else set()
    if depth > 12:
        return "<deep>"
    if isinstance(obj, (str, int, float, bool, type(None))):
        return obj
    if id(obj) in seen:
        return "<cycle>"
    seen = seen | {id(obj)}
    if isinstance(obj, dict):
        return {str(k): state(v, depth + 1, seen) for k, v in obj.items()}
    if isinstance(obj, (list, tuple)):
        return [state(v, depth + 1, seen) for v in obj]
    if isinstance(obj, set):
        return sorted(repr(v) for v in obj)
    if hasattr(obj, "__dict__"):
        return {"<cls>": type(obj).__name__, **{k: state(v, depth + 1, seen) for k, v in vars(obj).items()
                                                if not k.startswith("__")}}
    return repr(obj)


def main():
    from bfcl_eval.eval_checker.multi_turn_eval.multi_turn_utils import execute_multi_turn_func_call
    from bfcl_eval.utils import load_dataset_entry, load_ground_truth_entry

    from bench.bfcl.subsets import TRAIN

    entries = {e["id"]: e for e in load_dataset_entry("multi_turn_base")}
    gts = {g["id"]: g for g in load_ground_truth_entry("multi_turn_base")}
    changed = collections.Counter()
    samples = collections.Counter()
    for i in TRAIN:
        tid = f"multi_turn_base_{i}"
        e = entries[tid]
        name = f"effects_learn_{i}"
        _, inst = execute_multi_turn_func_call([], e["initial_config"], e["involved_classes"], name, tid)
        for turn in gts[tid]["ground_truth"]:
            for call in turn:
                fn = re.match(r"\s*(\w+)\s*\(", call).group(1)
                before = json.dumps(state(inst), sort_keys=True, default=str)
                outs, inst = execute_multi_turn_func_call([call], e["initial_config"], e["involved_classes"], name, tid)
                after = json.dumps(state(inst), sort_keys=True, default=str)
                samples[fn] += 1
                changed[fn] += before != after
    reads = sorted(f for f in samples if changed[f] == 0)
    writes = sorted(f for f in samples if changed[f])
    with open(OUT, "w") as f:
        json.dump({"source": "differential execution of ground-truth calls on bench.bfcl.subsets.TRAIN",
                   "reads": reads, "writes": writes, "samples": dict(sorted(samples.items()))}, f, indent=1)
        f.write("\n")
    print(f"{len(reads)} reads, {len(writes)} writes ({sum(samples.values())} calls) -> {OUT}")
    print("reads:", reads)
    print("writes:", writes)


if __name__ == "__main__":
    main()
