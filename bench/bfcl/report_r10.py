"""Round 10 (Addendum 10): the write gate on BFCL multi_turn_miss_param / miss_func, committed rows only.

    python bench/bfcl/report_r10.py     -> table on stdout, bench/results/v13/summary.json
"""
from __future__ import annotations

import glob
import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, ROOT)
from bench.bfcl.report import stats  # noqa: E402

OUT = os.path.join(ROOT, "bench", "results", "v13")


def main():
    groups = {}
    for d in sorted(glob.glob(os.path.join(OUT, "*-evidence-*"))):
        meta = json.load(open(os.path.join(d, "run.json")))
        cat = meta["manifest"]["category"].replace("multi_turn_", "")
        model = "GPT-4.1" if "4.1" in meta["model"] else "GPT-5.4 mini"
        rows = [dict(json.loads(l), category=cat) for l in open(os.path.join(d, "rows.jsonl"))]
        groups[f"{model}, {cat}"] = [dict(r, task=r["task_index"], mode=r["arm"]) for r in rows]
        groups.setdefault(f"{model}, pooled", []).extend(
            dict(r, task=(cat, r["task_index"]), mode=r["arm"]) for r in rows)
    summary, lines = {}, [
        "| model, category | pairs | pass W / N | Δ pass rate [95% CI] | W-only / N-only (p) | Δ model calls [CI] | "
        "Δ wall s [CI] | token ratio [CI] | $ ratio [CI] | $ W / N | gate pauses |",
        "|---|---|---|---|---|---|---|---|---|---|---|"]
    for name, rows in groups.items():
        if name.endswith("pooled") and sum(1 for k in groups if k.startswith(name.split(",")[0])) <= 2:
            continue  # one category only: pooled equals it
        s = stats(rows)
        pauses = sum((r.get("weave") or {}).get("gate_pauses", 0) for r in rows if r["mode"] == "weave")
        s["gate_pauses"] = pauses
        summary[name] = s
        f = lambda x: f"[{x[0]:+.3f}, {x[1]:+.3f}]"  # noqa: E731
        g = lambda x: f"[{x[0]:.2f}, {x[1]:.2f}]"  # noqa: E731
        m = s["mcnemar"]
        lines.append(
            f"| {name} | {s['pairs']} | {s['weave']['passed']} / {s['native']['passed']} | "
            f"{s['pass_diff']['mean_diff']:+.3f} {f(s['pass_diff']['mean_diff_ci95'])} | "
            f"{m['weave_only']} / {m['native_only']} ({m['p']:.2f}) | {s['calls']['mean_diff']:+.2f} "
            f"{f(s['calls']['mean_diff_ci95'])} | {s['wall']['mean_diff']:+.2f} {f(s['wall']['mean_diff_ci95'])} | "
            f"{s['tokens']['ratio_gm']:.2f} {g(s['tokens']['ratio_gm_ci95'])} | {s['usd']['ratio_gm']:.2f} "
            f"{g(s['usd']['ratio_gm_ci95'])} | {s['weave']['usd']:.3f} / {s['native']['usd']:.3f} | {pauses} |")
    json.dump(summary, open(os.path.join(OUT, "summary.json"), "w"), indent=1, default=str)
    print("\n".join(lines))
    for name, s in summary.items():
        print(name, "errors", s["error_types"], "scripts", s["weave_scripts"], "script errors", s["weave_script_errors"])


if __name__ == "__main__":
    main()
