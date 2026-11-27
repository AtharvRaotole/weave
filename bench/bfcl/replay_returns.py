"""Replay every recorded BFCL Weave-arm script through the static check, with and without declared return types.

No model calls and no tool calls: each script from `bench/results/v1*/**/traces.jsonl` (rounds 11 to 14) is only
passed to `Engine.check`, once with every BFCL function declared without a return type and once with
`returns=` set to the shape in `bench/bfcl/shapes.json` (learned from train-split ground-truth executions by
`learn_shapes.py`). A new diagnostic is compared with what the recorded run actually did:

- caught: the recorded run failed with a key or type lookup error on a line the check now reports;
- reported_but_ran_without_error: the recorded run finished without any error (a false alarm);
- reported_other_failure: the recorded run failed for another reason first;
- missed_*: the recorded run failed on a lookup the check does not report (`soft_error` when the result was
  the backend's `{"error": ...}` dict instead of the declared shape).

    PYTHONPATH=python python bench/bfcl/replay_returns.py [--out bench/results/returns-replay.json]
"""

from __future__ import annotations

import argparse
import collections
import glob
import json
import os
import re

import weave

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
LOOKUP = re.compile(r"error: L(\d+):C\d+: ((?:key .* is missing|cannot look up )[^\n]*)")
FAILED = re.compile(r"error: L(\d+):C\d+: ([^\n]*)")


def recorded_scripts() -> list:
    out = []
    for path in sorted(glob.glob(os.path.join(ROOT, "bench/results/v1*/**/traces.jsonl"), recursive=True)):
        run = os.path.basename(os.path.dirname(path))
        if "bfcl" not in run:
            continue
        for line in open(path):
            trace = json.loads(line)
            if trace.get("arm") != "weave":
                continue
            pending = {}
            for m in trace["messages"]:
                for call in m.get("tool_calls") or []:
                    if call["function"]["name"] == "run_weave":
                        try:
                            args = json.loads(call["function"]["arguments"])
                        except ValueError:
                            continue
                        pending[call["id"]] = args.get("source")
                if m.get("role") == "tool" and m.get("tool_call_id") in pending:
                    source = pending.pop(m["tool_call_id"])
                    if isinstance(source, str):
                        out.append({"run": run, "task": trace.get("id"), "script": source,
                                    "response": m.get("content") or ""})
    return out


def engine(shapes: dict, typed: bool) -> weave.Engine:
    e = weave.Engine()
    for name, shape in shapes.items():
        def fn(*args, **kwargs):
            raise AssertionError("check() never calls a tool")
        fn.__name__ = name
        e.tool(name, fn, effect="reversible", check_args=False, returns=shape if typed else None)
    return e


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=os.path.join(ROOT, "bench/results/returns-replay.json"))
    args = ap.parse_args()
    shapes = json.load(open(os.path.join(ROOT, "bench/bfcl/shapes.json")))["shapes"]
    plain, typed = engine(shapes, False), engine(shapes, True)
    counts = collections.Counter()
    cases = []
    for r in recorded_scripts():
        counts["scripts"] += 1
        before = set(plain.check(r["script"]))
        new = [d for d in typed.check(r["script"]) if d not in before]
        lookup = LOOKUP.search(r["response"])
        failed = FAILED.search(r["response"])
        if new:
            lines = {int(re.match(r"L(\d+):", d).group(1)) for d in new}
            if lookup and int(lookup.group(1)) in lines:
                kind = "caught"
            elif failed is None:
                kind = "reported_but_ran_without_error"
            else:
                kind = "reported_other_failure"
        elif lookup:
            kind = "missed_soft_error" if lookup.group(2).endswith("has keys 'error'") else "missed_other"
        else:
            continue
        counts[kind] += 1
        if kind != "missed_soft_error":
            cases.append({"kind": kind, "run": r["run"], "task": r["task"], "reported": new,
                          "recorded_error": failed.group(0) if failed else None, "script": r["script"]})
    caught = [c for c in cases if c["kind"] == "caught"]
    counts["caught_distinct_scripts"] = len({c["script"] for c in caught})
    counts["caught_after_an_earlier_call"] = sum(
        any(re.search(rf"\b{re.escape(n)}\s*\(", "\n".join(c["script"].splitlines()[:int(c["reported"][0][1:].split(":")[0]) - 1]))
            for n in shapes)
        for c in caught)
    summary = dict(sorted(counts.items()))
    json.dump({"summary": summary, "cases": cases}, open(args.out, "w"), indent=1)
    print(json.dumps(summary, indent=1))


if __name__ == "__main__":
    main()
