"""Statistics used in the v4 report: Wilson intervals, bootstrap intervals, paired per-task deltas."""

from __future__ import annotations

import math
import random
import statistics


def wilson(k, n, z=1.96):
    if n == 0:
        return None
    p = k / n
    d = 1 + z * z / n
    c = (p + z * z / (2 * n)) / d
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return [max(0.0, c - h), min(1.0, c + h)]


def boot_mean(values, seed=0, reps=4000):
    if not values:
        return None
    rng = random.Random(seed)
    n = len(values)
    means = sorted(sum(rng.choice(values) for _ in range(n)) / n for _ in range(reps))
    return [means[int(0.025 * reps)], means[int(0.975 * reps) - 1]]


def paired(rows, metric, seed=0, reps=4000):
    """Per (task, repeat) pairs: mean native/weave ratio and the Weave-minus-native difference.

    Returns the mean difference with a cluster bootstrap over tasks (repeats of one task are not
    independent), the geometric-mean ratio weave/native, and per-task means."""
    by = {}
    for r in rows:
        by.setdefault((r["task"], r["repeat"]), {})[r["mode"]] = metric(r)
    pairs = {k: v for k, v in by.items() if "weave" in v and "native" in v}
    tasks = sorted({k[0] for k in pairs})
    per_task = {}
    for t in tasks:
        ds = [v["weave"] - v["native"] for k, v in pairs.items() if k[0] == t]
        rs = [v["weave"] / v["native"] for k, v in pairs.items() if k[0] == t and v["native"] > 0 and v["weave"] > 0]
        per_task[t] = {"diff": statistics.fmean(ds), "ratio_gm": _gm(rs), "n": len(ds),
                       "weave": statistics.fmean(v["weave"] for k, v in pairs.items() if k[0] == t),
                       "native": statistics.fmean(v["native"] for k, v in pairs.items() if k[0] == t)}
    if not tasks:
        return None
    rng = random.Random(seed)
    boots = []
    for _ in range(reps):
        sample = [rng.choice(tasks) for _ in tasks]
        boots.append(statistics.fmean(per_task[t]["diff"] for t in sample))
    boots.sort()
    ratios = [v["weave"] / v["native"] for v in pairs.values() if v["native"] > 0 and v["weave"] > 0]
    rboots = []
    for _ in range(reps):
        sample = [rng.choice(tasks) for _ in tasks]
        vals = [per_task[t]["ratio_gm"] for t in sample if per_task[t]["ratio_gm"]]
        if vals:
            rboots.append(_gm(vals))
    rboots.sort()
    return {"pairs": len(pairs), "tasks": len(tasks),
            "mean_diff": statistics.fmean(per_task[t]["diff"] for t in tasks),
            "mean_diff_ci95": [boots[int(0.025 * reps)], boots[int(0.975 * reps) - 1]],
            "ratio_gm": _gm(ratios),
            "ratio_gm_ci95": [rboots[int(0.025 * len(rboots))], rboots[int(0.975 * len(rboots)) - 1]] if rboots else None,
            "weave_better_tasks": sum(per_task[t]["diff"] < 0 for t in tasks),
            "per_task": per_task}


def _gm(xs):
    xs = [x for x in xs if x > 0]
    return math.exp(statistics.fmean(math.log(x) for x in xs)) if xs else None


def mcnemar_exact(rows):
    """Exact two-sided McNemar test on paired pass/fail outcomes."""
    by = {}
    for r in rows:
        by.setdefault((r["task"], r["repeat"]), {})[r["mode"]] = r["passed"]
    b = sum(1 for v in by.values() if v.get("weave") and v.get("native") is False)
    c = sum(1 for v in by.values() if v.get("native") and v.get("weave") is False)
    n = b + c
    if n == 0:
        return {"weave_only": 0, "native_only": 0, "p": 1.0}
    k = min(b, c)
    p = min(1.0, 2 * sum(math.comb(n, i) for i in range(k + 1)) / 2 ** n)
    return {"weave_only": b, "native_only": c, "p": p}


def describe(rows, seed=0):
    n = len(rows)
    if not n:
        return {"trials": 0}
    k = sum(r["passed"] for r in rows)
    pick = lambda f: [f(r) for r in rows]
    return {
        "trials": n, "passed": k, "pass_rate": k / n, "pass_wilson95": wilson(k, n),
        "statuses": {s: sum(r["status"] == s for r in rows) for s in sorted({r["status"] for r in rows})},
        "answer_wrong": sum(not r["answer_ok"] for r in rows), "state_wrong": sum(not r["state_ok"] for r in rows),
        "unexpected_write_trials": sum(r["unexpected_writes"] > 0 or r["duplicate_writes"] > 0 for r in rows),
        "mean_turns": statistics.fmean(pick(lambda r: r["turns"])), "mean_turns_ci95": boot_mean(pick(lambda r: r["turns"]), seed),
        "mean_wall_s": statistics.fmean(pick(lambda r: r["wall_s"])), "mean_wall_s_ci95": boot_mean(pick(lambda r: r["wall_s"]), seed + 1),
        "p50_wall_s": statistics.median(pick(lambda r: r["wall_s"])),
        "p95_wall_s": sorted(pick(lambda r: r["wall_s"]))[max(0, math.ceil(0.95 * n) - 1)],
        "mean_tokens": statistics.fmean(pick(lambda r: r["total_tokens"])),
        "mean_tokens_ci95": boot_mean(pick(lambda r: r["total_tokens"]), seed + 2),
        "mean_input_tokens": statistics.fmean(pick(lambda r: r["tokens"]["input"])),
        "mean_output_tokens": statistics.fmean(pick(lambda r: r["tokens"]["output"])),
        "sum_tokens": {k2: sum(r["tokens"][k2] for r in rows) for k2 in ("input", "output", "cached", "reasoning")},
        "unknown_usage_turns": sum(r["unknown_usage_turns"] for r in rows),
        "mean_backend_calls": statistics.fmean(pick(lambda r: r["backend_calls"])),
        "script_error_trials": sum(r["script_errors"] > 0 for r in rows),
        "retry_wait_s": sum(r["retry_wait_s"] for r in rows),
    }
