"""Round 15 analysis, as pre-registered (docs/research/round15-preregistration.md, Addendum 15 of
docs/paper/preregistration.md). Written before any evidence trial.

    python bench/bfcl/report_r15.py --set lc100r15 RUN_DIR [RUN_DIR ...] [--set mtb60r9 RUN_DIR ...]
        [--live LIVE_RUN_DIR ...] [--out report.json]    (needs bfcl-eval for the BFCL sets)

Per set: only complete pairs count (a task and repeat with a row in both arms). Per task, each arm's pass rate is the
mean over its complete repeats. Primary: mean over tasks of (weave - native), with a paired bootstrap 95% CI
(10,000 resamples of tasks, seed 2026, percentile). Exact two-sided McNemar on repeat 0. Secondary, same bootstrap:
model calls, input+output tokens, list-price USD, wall seconds per task, and state-changing calls made after a failed
call in the same model step (a native assistant message or a Weave script).
"""

from __future__ import annotations

import argparse
import ast
import collections
import json
import math
import os
import random
import re

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
READS = set(json.load(open(os.path.join(ROOT, "bench/bfcl/effects.json")))["reads"])
CALL = re.compile(r"^(\w+)\((.*)\) -> (.*)$")


def failed_output(out: str) -> bool:
    if out.startswith("Error during execution") or out.startswith("error:"):
        return True
    for parse in (json.loads, ast.literal_eval):
        try:
            v = parse(out)
            return isinstance(v, dict) and "error" in v
        except Exception:  # noqa: BLE001
            continue
    return bool(re.match(r"^\{['\"]error['\"]", out))


def writes_after_failure(row: dict, result, entries: dict) -> int:
    """State-changing calls made after a failed call in the same model step, for either arm. A step is one model
    response: a native assistant message's tool calls, or the calls one Weave script executed (paused or refused
    calls never executed and are not in the record). The recorded calls are executed again, in order, on fresh BFCL
    backends (deterministic; no model calls) to get each call's output."""
    from bfcl_eval.eval_checker.multi_turn_eval.multi_turn_utils import execute_multi_turn_func_call

    entry = entries[row["id"]]
    long_context = "long_context" in row["id"]
    name = f"r15report_{row['arm']}_{row['repeat']}_{row['id']}_{random.random()}"
    n = 0
    for turn in result or []:
        for step in turn:
            if not isinstance(step, list):
                continue
            failed = False
            for call in step:
                (fn, args), = call.items()
                kwargs = json.loads(args) if isinstance(args, str) else args
                text = f"{fn}({','.join(f'{k}={v!r}' for k, v in kwargs.items())})"
                [out], _ = execute_multi_turn_func_call([text], entry["initial_config"], entry["involved_classes"],
                                                        name, entry["id"], long_context=long_context,
                                                        is_evaL_run=False)
                if failed and fn not in READS:
                    n += 1
                failed = failed or failed_output(str(out))
    return n


def load(dirs, reps_offset=True):
    """Rows keyed (task, arm, repeat). Invocations of the same set get distinct repeat numbers in the given order."""
    rows, traces = {}, {}
    for k, d in enumerate(dirs):
        for line in open(os.path.join(d, "rows.jsonl")):
            r = json.loads(line)
            rows[(r["task_index"], r["arm"], k)] = r
        tp = os.path.join(d, "traces.jsonl")
        if os.path.exists(tp):
            for line in open(tp):
                t = json.loads(line)
                traces[(int(t["id"].rsplit("_", 1)[1]), t["arm"], k)] = t
    return rows, traces


def boot(values, seed=2026, reps=10_000):
    rng = random.Random(seed)
    n = len(values)
    means = sorted(sum(values[rng.randrange(n)] for _ in range(n)) / n for _ in range(reps))
    return [means[int(0.025 * reps)], means[int(0.975 * reps) - 1]]


def mcnemar(b, c):
    n = b + c
    if n == 0:
        return 1.0
    k = min(b, c)
    p = sum(math.comb(n, i) for i in range(k + 1)) / 2 ** n
    return min(1.0, 2 * p)


def analyse(dirs):
    from bfcl_eval.utils import load_dataset_entry

    rows, traces = load(dirs)
    category = json.load(open(os.path.join(dirs[0], "run.json")))["manifest"]["category"]
    entries = {e["id"]: e for e in load_dataset_entry(category)}
    tasks = sorted({t for t, _, _ in rows})
    per = collections.defaultdict(lambda: collections.defaultdict(list))
    rep0 = []
    for t in tasks:
        for k in range(len(dirs)):
            w, n = rows.get((t, "weave", k)), rows.get((t, "native", k))
            if not (w and n):
                continue
            for arm, r in (("weave", w), ("native", n)):
                tr = traces.get((t, arm, k), {})
                per[t][arm].append({"pass": float(bool(r["passed"])), "calls": r["agent_calls"],
                                    "tokens": r["tokens"]["input"] + r["tokens"]["output"], "usd": r["usd"],
                                    "wall": r["wall_s"], "after_fail": writes_after_failure(r, tr.get("result"), entries)})
            if k == 0:
                rep0.append((bool(w["passed"]), bool(n["passed"])))
    paired = [t for t in per if per[t]["weave"]]
    out = {"tasks_with_pairs": len(paired), "trial_pairs": sum(len(per[t]["weave"]) for t in paired)}
    for metric in ("pass", "calls", "tokens", "usd", "wall", "after_fail"):
        w = [sum(x[metric] for x in per[t]["weave"]) / len(per[t]["weave"]) for t in paired]
        n = [sum(x[metric] for x in per[t]["native"]) / len(per[t]["native"]) for t in paired]
        d = [a - b for a, b in zip(w, n)]
        out[metric] = {"weave_mean": sum(w) / len(w), "native_mean": sum(n) / len(n), "diff": sum(d) / len(d),
                       "diff_ci95": boot(d), "weave_total": sum(sum(x[metric] for x in per[t]["weave"]) for t in paired),
                       "native_total": sum(sum(x[metric] for x in per[t]["native"]) for t in paired)}
    b = sum(w and not n for w, n in rep0)
    c = sum(n and not w for w, n in rep0)
    out["repeat0"] = {"pairs": len(rep0), "weave_pass": sum(w for w, _ in rep0), "native_pass": sum(n for _, n in rep0),
                      "weave_only": b, "native_only": c, "mcnemar_p": mcnemar(b, c)}
    out["per_task"] = {t: {a: [x["pass"] for x in per[t][a]] for a in ("weave", "native")} for t in paired}
    return out


def analyse_live(dirs):
    """live-v2: complete (task, repeat) pairs; per task means; same bootstrap. Unsafe = unexpected write attempts plus
    write attempts after a failed call."""
    rows = {}
    for k, d in enumerate(dirs):
        for line in open(os.path.join(d, "rows.jsonl")):
            r = json.loads(line)
            rows[(r["task"], r["arm"], (k, r["repeat"]))] = r
    per = collections.defaultdict(lambda: collections.defaultdict(list))
    for (t, arm, rep), r in rows.items():
        other = rows.get((t, "native" if arm == "weave" else "weave", rep))
        if other is None:
            continue
        per[t][arm].append({"pass": float(r["passed"]), "calls": r["agent_calls"],
                            "tokens": r["tokens"]["input"] + r["tokens"]["output"], "usd": r["usd"],
                            "wall": r["wall_s"], "unsafe": r["unexpected_write_attempts"]
                            + r["write_attempts_after_failed_call"], "provider_error": float(r["status"] == "error")})
    tasks = sorted(per)
    out = {"tasks_with_pairs": len(tasks), "trial_pairs": sum(len(per[t]["weave"]) for t in tasks)}
    for metric in ("pass", "calls", "tokens", "usd", "wall", "unsafe", "provider_error"):
        w = [sum(x[metric] for x in per[t]["weave"]) / len(per[t]["weave"]) for t in tasks]
        n = [sum(x[metric] for x in per[t]["native"]) / len(per[t]["native"]) for t in tasks]
        d = [a - b for a, b in zip(w, n)]
        out[metric] = {"weave_mean": sum(w) / len(w), "native_mean": sum(n) / len(n), "diff": sum(d) / len(d),
                       "diff_ci95": boot(d), "weave_total": sum(sum(x[metric] for x in per[t]["weave"]) for t in tasks),
                       "native_total": sum(sum(x[metric] for x in per[t]["native"]) for t in tasks)}
    fam = collections.defaultdict(lambda: collections.Counter())
    for (t, arm, _), r in rows.items():
        fam[r["family"]][arm + "_pass"] += r["passed"]
        fam[r["family"]][arm + "_trials"] += 1
    out["by_family"] = {f: dict(c) for f, c in fam.items()}
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--set", nargs="+", action="append", default=[], metavar=("NAME", "RUN_DIR"))
    ap.add_argument("--live", nargs="+", default=[])
    ap.add_argument("--out")
    args = ap.parse_args()
    report = {}
    for name, *dirs in args.set:
        report[name] = analyse(dirs)
        r = report[name]
        print(f"{name}: {r['tasks_with_pairs']} tasks, {r['trial_pairs']} pairs; pass weave {r['pass']['weave_mean']:.3f} "
              f"native {r['pass']['native_mean']:.3f} diff {r['pass']['diff']:+.3f} CI {r['pass']['diff_ci95'][0]:+.3f}.."
              f"{r['pass']['diff_ci95'][1]:+.3f}; rep0 {r['repeat0']}")
    if args.live:
        report["live_v2"] = r = analyse_live(args.live)
        print(f"live_v2: {r['tasks_with_pairs']} tasks, {r['trial_pairs']} pairs; pass diff {r['pass']['diff']:+.3f} "
              f"CI {r['pass']['diff_ci95']}; unsafe weave {r['unsafe']['weave_total']} native {r['unsafe']['native_total']}")
    if args.out:
        json.dump(report, open(args.out, "w"), indent=1)


if __name__ == "__main__":
    main()
