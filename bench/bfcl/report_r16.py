"""Round 16 analysis, as pre-registered (docs/research/round16-preregistration.md, Addendum 16 of
docs/paper/preregistration.md). Written before any round 16 model call.

    python bench/bfcl/report_r16.py --run REPEAT RUN_DIR [--run REPEAT RUN_DIR ...] [--out report.json]

Each RUN_DIR is one invocation of bench/bfcl/run.py (one category, one repeat). A task is (category, id). Only
complete pairs count (a task and repeat with a row in both arms). Per task, an arm's pass rate is the mean over its
complete repeats. Primary: mean over ALL fresh tasks (all categories pooled) of (weave - native), paired bootstrap
95% CI (10,000 resamples of tasks, seed 2026, percentile), exact two-sided McNemar on repeat 0 (and repeat 1, reported).
Also reported: each category on its own (same method), model calls, tokens, list-price USD and wall seconds per task.
No row is dropped for any reason other than a missing partner row.
"""
import argparse
import collections
import json
import math
import os
import random


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
    return min(1.0, 2 * sum(math.comb(n, i) for i in range(k + 1)) / 2 ** n)


def load(runs):
    rows = {}
    for rep, d in runs:
        for line in open(os.path.join(d, "rows.jsonl")):
            r = json.loads(line)
            cat = r["id"].rsplit("_", 1)[0]
            rows[((cat, r["task_index"]), r["arm"], rep)] = r
    return rows


def analyse(rows, keep=lambda task: True):
    tasks = sorted({t for t, _, _ in rows if keep(t)})
    per = collections.defaultdict(lambda: collections.defaultdict(list))
    byrep = collections.defaultdict(list)
    for t in tasks:
        for k in sorted({k for _, _, k in rows}):
            w, n = rows.get((t, "weave", k)), rows.get((t, "native", k))
            if not (w and n):
                continue
            for arm, r in (("weave", w), ("native", n)):
                per[t][arm].append({"pass": float(bool(r["passed"])), "calls": r["agent_calls"],
                                    "tokens": r["tokens"]["input"] + r["tokens"]["output"], "usd": r["usd"],
                                    "wall": r["wall_s"]})
            byrep[k].append((bool(w["passed"]), bool(n["passed"])))
    paired = [t for t in tasks if per[t]["weave"]]
    out = {"tasks_with_pairs": len(paired), "trial_pairs": sum(len(per[t]["weave"]) for t in paired)}
    if not paired:
        return out
    for metric in ("pass", "calls", "tokens", "usd", "wall"):
        w = [sum(x[metric] for x in per[t]["weave"]) / len(per[t]["weave"]) for t in paired]
        n = [sum(x[metric] for x in per[t]["native"]) / len(per[t]["native"]) for t in paired]
        d = [a - b for a, b in zip(w, n)]
        out[metric] = {"weave_mean": sum(w) / len(w), "native_mean": sum(n) / len(n), "diff": sum(d) / len(d),
                       "diff_ci95": boot(d),
                       "weave_total": sum(sum(x[metric] for x in per[t]["weave"]) for t in paired),
                       "native_total": sum(sum(x[metric] for x in per[t]["native"]) for t in paired)}
    for k, prs in sorted(byrep.items()):
        b = sum(w and not n for w, n in prs)
        c = sum(n and not w for w, n in prs)
        out[f"repeat{k}"] = {"pairs": len(prs), "weave_pass": sum(w for w, _ in prs),
                             "native_pass": sum(n for _, n in prs), "weave_only": b, "native_only": c,
                             "mcnemar_p": mcnemar(b, c)}
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--run", nargs=2, action="append", required=True, metavar=("REPEAT", "RUN_DIR"))
    ap.add_argument("--out")
    args = ap.parse_args()
    rows = load([(int(r), d) for r, d in args.run])
    report = {"primary_all_fresh": analyse(rows)}
    for cat in sorted({t[0] for t, _, _ in rows}):
        report[cat] = analyse(rows, lambda t, cat=cat: t[0] == cat)
    for name, r in report.items():
        if "pass" not in r:
            continue
        p = r["pass"]
        print(f"{name}: {r['tasks_with_pairs']} tasks, {r['trial_pairs']} pairs; weave {p['weave_mean']:.3f} native "
              f"{p['native_mean']:.3f} diff {p['diff']:+.3f} CI {p['diff_ci95'][0]:+.3f}..{p['diff_ci95'][1]:+.3f}; "
              f"rep0 {r.get('repeat0')}")
    if args.out:
        json.dump(report, open(args.out, "w"), indent=1)


if __name__ == "__main__":
    main()
