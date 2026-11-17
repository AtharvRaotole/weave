"""Addendum 8 report: BFCL multi_turn_base, Weave arm vs BFCL's native FC loop, per model and pooled.

Reads committed rows only (bench/results/v11/*-evidence-*), prints markdown tables, writes summary.json and the
chart assets/benchmark-v11-bfcl.{png,svg}. Statistics are the pre-registered ones: exact McNemar, paired pass-rate
difference with a 95% cluster-bootstrap CI, Weave-minus-native calls and wall time, geometric-mean ratios of tokens
and list-price dollars. Trials skipped for budget are dropped from paired statistics (their pair partner too).

    python bench/bfcl/report.py
"""

from __future__ import annotations

import collections
import glob
import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, ROOT)
from bench.v4.stats import mcnemar_exact, paired, wilson  # noqa: E402

OUT = os.path.join(ROOT, "bench", "results", "v11")
CHART = "benchmark-v11-bfcl"
TITLE = "BFCL v4 multi_turn_base, 40 pre-registered tasks (GPT-4.1: 38 paired, 2 skipped for budget); 95% cluster-bootstrap CIs"
if "--round9" in sys.argv:
    OUT = os.path.join(ROOT, "bench", "results", "v12")
    CHART = "benchmark-v12-bfcl"
    TITLE = "Round 9: BFCL v4 multi_turn_base, 60 fresh pre-registered tasks; 95% cluster-bootstrap CIs"


def load():
    runs = {}
    for d in sorted(glob.glob(os.path.join(OUT, "*-evidence-*"))):
        meta = json.load(open(os.path.join(d, "run.json")))
        rows = [json.loads(l) for l in open(os.path.join(d, "rows.jsonl"))]
        runs[meta["model"]] = {"dir": os.path.basename(d), "meta": meta, "rows": rows}
    return runs


def as_pairs(rows, pooled_key=False):
    out = []
    for r in rows:
        out.append(dict(r, task=(r["model"], r["task_index"]) if pooled_key else r["task_index"], mode=r["arm"]))
    return out


def tokens(r):
    return r["tokens"]["input"] + r["tokens"]["output"]


def stats(rows):
    keyed = {}
    for r in rows:
        keyed.setdefault(r["task"], {})[r["mode"]] = r
    both = [r for v in keyed.values() if len(v) == 2 for r in v.values()]
    res = {"pairs": len(both) // 2, "mcnemar": mcnemar_exact(both)}
    for arm in ("weave", "native"):
        k = sum(r["passed"] for r in both if r["mode"] == arm)
        res[arm] = {"passed": k, "wilson95": wilson(k, res["pairs"]),
                    "usd": sum(r["usd"] for r in both if r["mode"] == arm),
                    "tokens": sum(r["total_tokens"] for r in both if r["mode"] == arm),
                    "calls": sum(r["agent_calls"] for r in both if r["mode"] == arm)}
    res["pass_diff"] = paired(both, lambda r: float(r["passed"]), seed=1)
    res["calls"] = paired(both, lambda r: r["agent_calls"], seed=2)
    res["wall"] = paired(both, lambda r: r["wall_s"], seed=3)
    res["tokens"] = paired(both, tokens, seed=4)
    res["usd"] = paired(both, lambda r: r["usd"], seed=5)
    for k in ("pass_diff", "calls", "wall", "tokens", "usd"):
        res[k].pop("per_task", None)
    w = [r for r in both if r["mode"] == "weave" and r.get("weave")]
    res["weave_scripts"] = sum(r["weave"]["scripts"] for r in w)
    res["weave_script_errors"] = sum(r["weave"]["script_errors"] for r in w)
    res["weave_tool_calls"] = sum(r["weave"]["tool_calls"] for r in w)
    res["error_types"] = {arm: dict(collections.Counter(r["error_type"] or "pass" for r in both if r["mode"] == arm))
                          for arm in ("weave", "native")}
    return res


def fmt_ci(ci, nd=2):
    return f"[{ci[0]:+.{nd}f}, {ci[1]:+.{nd}f}]"


def table(summary):
    lines = ["| run | pairs | Weave pass | native pass | Δ pass rate [95% CI] | McNemar (W-only/N-only, p) | Δ model calls/task [CI] | Δ wall s/task [CI] | tokens W/N [CI] | $ W/N [CI] | $ Weave / native |",
             "|---|---|---|---|---|---|---|---|---|---|---|"]
    for name, s in summary.items():
        m = s["mcnemar"]
        lines.append(
            f"| {name} | {s['pairs']} | {s['weave']['passed']}/{s['pairs']} | {s['native']['passed']}/{s['pairs']} | "
            f"{s['pass_diff']['mean_diff']:+.3f} {fmt_ci(s['pass_diff']['mean_diff_ci95'], 3)} | "
            f"{m['weave_only']}/{m['native_only']}, p={m['p']:.2f} | "
            f"{s['calls']['mean_diff']:+.2f} {fmt_ci(s['calls']['mean_diff_ci95'])} | "
            f"{s['wall']['mean_diff']:+.2f} {fmt_ci(s['wall']['mean_diff_ci95'])} | "
            f"{s['tokens']['ratio_gm']:.2f} [{s['tokens']['ratio_gm_ci95'][0]:.2f}, {s['tokens']['ratio_gm_ci95'][1]:.2f}] | "
            f"{s['usd']['ratio_gm']:.2f} [{s['usd']['ratio_gm_ci95'][0]:.2f}, {s['usd']['ratio_gm_ci95'][1]:.2f}] | "
            f"${s['weave']['usd']:.3f} / ${s['native']['usd']:.3f} |")
    return "\n".join(lines)


def chart(summary, name=None):
    name = name or CHART
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    labels = list(summary)
    fig, axes = plt.subplots(1, 3, figsize=(13, 3.8))
    specs = (("pass_diff", "mean_diff", "Δ pass rate (Weave − native)", 0.0),
             ("calls", "mean_diff", "Δ model calls per task", 0.0),
             ("usd", "ratio_gm", "list-price $ ratio (Weave / native)", 1.0))
    for ax, (key, stat, title, ref) in zip(axes, specs):
        ci_key = stat + "_ci95"
        for i, lab in enumerate(labels):
            v = summary[lab][key]
            lo, hi = v[ci_key]
            ax.errorbar(v[stat], i, xerr=[[v[stat] - lo], [hi - v[stat]]], fmt="o", color="#1f5f8b", capsize=4)
        ax.axvline(ref, color="#888", lw=1, ls="--")
        if key == "pass_diff":
            ax.axvline(-0.10, color="#c0392b", lw=1, ls=":", label="no-harm bound −0.10")
            ax.legend(loc="lower right", fontsize=8, frameon=False)
        ax.set_yticks(range(len(labels)))
        ax.set_yticklabels(labels if ax is axes[0] else [""] * len(labels))
        ax.set_ylim(-0.6, len(labels) - 0.4)
        ax.invert_yaxis()
        ax.set_title(title, fontsize=10)
    fig.suptitle(TITLE, fontsize=11)
    fig.tight_layout()
    for ext in ("png", "svg"):
        fig.savefig(os.path.join(ROOT, "assets", f"{name}.{ext}"), dpi=150)


def main():
    runs = load()
    summary = {}
    allrows = []
    for model, run in runs.items():
        label = "GPT-4.1" if "4.1" in model else "GPT-5.4 mini"
        summary[label] = stats(as_pairs(run["rows"]))
        summary[label]["skipped_for_budget"] = run["meta"].get("not_run_budget", [])
        summary[label]["run_usd_all_trials"] = sum(r["usd"] for r in run["rows"])
        summary[label]["run_tokens_all_trials"] = sum(r["total_tokens"] for r in run["rows"])
        allrows += as_pairs(run["rows"], pooled_key=True)
    summary["pooled"] = stats(allrows)
    json.dump(summary, open(os.path.join(OUT, "summary.json"), "w"), indent=2, default=str)
    print(table(summary))
    for k, s in summary.items():
        print(f"\n{k}: scripts={s['weave_scripts']} script_errors={s['weave_script_errors']} "
              f"tool_calls={s['weave_tool_calls']} errors={s['error_types']} skipped={s.get('skipped_for_budget')} "
              f"run$={s.get('run_usd_all_trials')} tokens={s.get('run_tokens_all_trials')}")
    if "--no-chart" not in sys.argv:
        chart(summary)


if __name__ == "__main__":
    main()
