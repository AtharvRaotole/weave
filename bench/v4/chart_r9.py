"""README results chart: round 9, fan-out suite (confirm9) and BFCL multi-turn (mtb60r9), Weave vs native.

    python bench/v4/chart_r9.py   -> assets/results.png, assets/results.svg
"""
import glob
import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, ROOT)
from bench.v4.report_r9 import usd  # noqa: E402
from bench.v4.stats import wilson  # noqa: E402


def groups():
    out = []
    for d in sorted(glob.glob(os.path.join(ROOT, "bench/results/v12/*-confirm9-*"))):
        rows = [json.loads(l) for l in open(os.path.join(d, "rows.jsonl"))]
        m = "GPT-4.1" if "4.1" in rows[0]["model"] else "GPT-5.4 mini"
        w = [r for r in rows if r["mode"] == "weave_compact"]
        n = [r for r in rows if r["mode"] == "native"]
        out.append((f"Fan-out, real services\n{m} ({len(n)} trials)", w, n, usd))
    for d in sorted(glob.glob(os.path.join(ROOT, "bench/results/v12/*-evidence-*"))):
        rows = [json.loads(l) for l in open(os.path.join(d, "rows.jsonl"))]
        m = "GPT-4.1" if "4.1" in rows[0]["model"] else "GPT-5.4 mini"
        w = [r for r in rows if r["arm"] == "weave"]
        n = [r for r in rows if r["arm"] == "native"]
        out.append((f"BFCL multi-turn\n{m} ({len(n)} tasks)", w, n, lambda r: r["usd"]))
    return out


def main():
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    g = groups()
    fig, (a1, a2) = plt.subplots(1, 2, figsize=(12, 4.2))
    xs = range(len(g))
    blue, grey = "#1f5f8b", "#b8bec6"
    for i, (label, w, n, cost) in enumerate(g):
        for off, rows, color, name in ((-0.18, w, blue, "Weave"), (0.18, n, grey, "native function calling")):
            k = sum(r["passed"] for r in rows)
            lo, hi = wilson(k, len(rows))
            p = k / len(rows)
            a1.bar(i + off, p * 100, 0.34, color=color, label=name if i == 0 else None)
            a1.errorbar(i + off, p * 100, yerr=[[(p - lo) * 100], [(hi - p) * 100]], color="#333", capsize=3, lw=1)
            c = sum(map(cost, rows)) / len(rows) * 1000
            a2.bar(i + off, c, 0.34, color=color)
            a2.text(i + off, c, f"{c:.1f}", ha="center", va="bottom", fontsize=8)
    for ax, t in ((a1, "Tasks passed (%), 95% Wilson CI"), (a2, "List-price cost per task (thousandths of $)")):
        ax.set_xticks(list(xs))
        ax.set_xticklabels([x[0] for x in g], fontsize=8)
        ax.set_title(t, fontsize=10)
        ax.spines[["top", "right"]].set_visible(False)
    a1.set_ylim(0, 105)
    a1.legend(frameon=False, fontsize=8, loc="upper right")
    fig.suptitle("Round 9, pre-registered: Weave wins on fan-out; on BFCL it is cheaper, +3 tasks with GPT-4.1, −9 with GPT-5.4 mini",
                 fontsize=10)
    fig.tight_layout()
    for ext in ("png", "svg"):
        fig.savefig(os.path.join(ROOT, "assets", f"results.{ext}"), dpi=150)


if __name__ == "__main__":
    main()
