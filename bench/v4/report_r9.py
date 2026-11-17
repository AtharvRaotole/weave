"""Addendum 9 fan-out reconfirm (`confirm9`): weave_compact and weave vs native, per model (committed rows only).

    python bench/v4/report_r9.py
"""

from __future__ import annotations

import glob
import json
import os
import statistics as st
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, ROOT)
from bench.v4.stats import mcnemar_exact, paired  # noqa: E402

PRICES = {"gpt-4.1": (2.00, 8.00, 0.50), "gpt-5.4-mini": (0.75, 4.50, 0.075)}


def usd(r):
    i, o, c = PRICES["gpt-5.4-mini" if "mini" in r["model"] else "gpt-4.1"]
    t = r["tokens"]
    return ((t["input"] - t["cached"]) * i + t["cached"] * c + t["output"] * o) / 1e6


def main():
    out = {}
    lines = ["| model | arm vs native | pairs | pass arm / native (McNemar p) | Δ wall s [95% CI] | tokens arm/native [CI] | $ arm/native [CI] | $ arm / native |",
             "|---|---|---|---|---|---|---|---|"]
    for d in sorted(glob.glob(os.path.join(ROOT, "bench/results/v12/*-confirm9-*"))):
        rows = [json.loads(l) for l in open(os.path.join(d, "rows.jsonl"))]
        model = rows[0]["model"]
        label = "GPT-4.1" if "4.1" in model else "GPT-5.4 mini"
        for arm in ("weave_compact", "weave"):
            sub = [dict(r, mode="weave" if r["mode"] == arm else "native", task=r["task"])
                   for r in rows if r["mode"] in (arm, "native")]
            m = mcnemar_exact(sub)
            wall = paired(sub, lambda r: r["wall_s"], seed=1)
            tok = paired(sub, lambda r: r["total_tokens"], seed=2)
            cost = paired(sub, usd, seed=3)
            a = [r for r in sub if r["mode"] == "weave"]
            n = [r for r in sub if r["mode"] == "native"]
            out[f"{label} {arm}"] = {"pairs": wall["pairs"], "pass": [sum(r["passed"] for r in a), sum(r["passed"] for r in n)],
                                     "mcnemar": m, "wall": wall["mean_diff"], "wall_ci": wall["mean_diff_ci95"],
                                     "tokens": tok["ratio_gm"], "tokens_ci": tok["ratio_gm_ci95"],
                                     "usd": cost["ratio_gm"], "usd_ci": cost["ratio_gm_ci95"],
                                     "usd_sum": [sum(map(usd, a)), sum(map(usd, n))]}
            o = out[f"{label} {arm}"]
            lines.append(f"| {label} | {arm} | {o['pairs']} | {o['pass'][0]}/{len(a)} / {o['pass'][1]}/{len(n)} (p={m['p']:.2f}) | "
                         f"{o['wall']:+.2f} [{o['wall_ci'][0]:+.2f}, {o['wall_ci'][1]:+.2f}] | "
                         f"{o['tokens']:.2f} [{o['tokens_ci'][0]:.2f}, {o['tokens_ci'][1]:.2f}] | "
                         f"{o['usd']:.2f} [{o['usd_ci'][0]:.2f}, {o['usd_ci'][1]:.2f}] | ${o['usd_sum'][0]:.3f} / ${o['usd_sum'][1]:.3f} |")
        out[f"{label} run_usd"] = sum(map(usd, rows))
        out[f"{label} run_tokens"] = sum(r["total_tokens"] for r in rows)
    json.dump(out, open(os.path.join(ROOT, "bench/results/v12/confirm9-summary.json"), "w"), indent=2)
    print("\n".join(lines))
    print({k: v for k, v in out.items() if "run_" in k})


if __name__ == "__main__":
    main()
