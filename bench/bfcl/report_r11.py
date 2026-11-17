"""Addendum 11: round 10 completed on Azure (mixed endpoint), and the fresh confirmation (committed rows only).

    python bench/bfcl/report_r11.py   -> tables on stdout, bench/results/v14/summary.json
"""
from __future__ import annotations

import glob
import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, ROOT)
from bench.bfcl.report import stats  # noqa: E402

V13, V14 = (os.path.join(ROOT, "bench", "results", v) for v in ("v13", "v14"))


def runs(root):
    for d in sorted(glob.glob(os.path.join(root, "*-evidence-*"))):
        meta = json.load(open(os.path.join(d, "run.json")))
        cat = meta["manifest"]["category"].replace("multi_turn_", "")
        model = "GPT-4.1" if "4.1" in meta["model"] else "GPT-5.4 mini"
        rows = [json.loads(l) for l in open(os.path.join(d, "rows.jsonl"))]
        yield os.path.basename(d), model, cat, rows


def complete(rows):
    by = {}
    for r in rows:
        by.setdefault(r["task"], {})[r["mode"]] = r
    return [r for v in by.values() if len(v) == 2 for r in v.values()]


def line(name, s):
    f = lambda x: f"[{x[0]:+.3f}, {x[1]:+.3f}]"  # noqa: E731
    g = lambda x: f"[{x[0]:.2f}, {x[1]:.2f}]"  # noqa: E731
    m = s["mcnemar"]
    return (f"| {name} | {s['pairs']} | {s['weave']['passed']} / {s['native']['passed']} | "
            f"{s['pass_diff']['mean_diff']:+.3f} {f(s['pass_diff']['mean_diff_ci95'])} | "
            f"{m['weave_only']} / {m['native_only']} ({m['p']:.2f}) | {s['calls']['mean_diff']:+.2f} | "
            f"{s['tokens']['ratio_gm']:.2f} {g(s['tokens']['ratio_gm_ci95'])} | {s['usd']['ratio_gm']:.2f} "
            f"{g(s['usd']['ratio_gm_ci95'])} | {s['weave']['usd']:.3f} / {s['native']['usd']:.3f} | {s['gate_pauses']} |")


def summarize(groups):
    out, lines = {}, ["| group | pairs | pass W / N | Δ pass rate [95% CI] | W-only / N-only (p) | Δ calls | "
                      "token ratio [CI] | $ ratio [CI] | $ W / N | gate pauses |", "|---|---|---|---|---|---|---|---|---|---|"]
    for name, rows in groups.items():
        rows = complete(rows)
        if not rows:
            continue
        s = stats(rows)
        s["gate_pauses"] = sum((r.get("weave") or {}).get("gate_pauses", 0) for r in rows if r["mode"] == "weave")
        out[name] = s
        lines.append(line(name, s))
    return out, lines


def main():
    a, b = {}, {}
    # Part A: round 10 with the Azure completion pairs (mixed endpoint).
    for _, model, cat, rows in list(runs(V13)) + [x for x in runs(V14) if x[0].endswith(("-A_mp", "-A_mf", "-A_41"))]:
        src = "azure" if rows and rows[0].get("endpoint", "").endswith("azure.com") else "openai"
        keyed = [dict(r, task=(cat, r["task_index"], src), mode=r["arm"]) for r in rows]
        a.setdefault(f"R10+A {model}, {cat}", []).extend(keyed)
        a.setdefault(f"R10+A {model}, pooled", []).extend(keyed)
    for _, model, cat, rows in runs(V14):
        if _.endswith(("-A_mp", "-A_mf", "-A_41")):
            continue
        keyed = [dict(r, task=(cat, r["task_index"]), mode=r["arm"]) for r in rows]
        b.setdefault(f"fresh {model}, {cat}", []).extend(keyed)
        b.setdefault(f"fresh {model}, pooled", []).extend(keyed)
        b.setdefault("fresh both models, pooled", []).extend(dict(r, task=(model,) + r["task"]) for r in keyed)
    sa, la = summarize(a)
    sb, lb = summarize(b)
    json.dump({"round10_completed": sa, "confirmation": sb}, open(os.path.join(V14, "summary.json"), "w"), indent=1,
              default=str)
    print("Round 10 completed (pairs keyed per endpoint; an OpenAI orphan never pairs with an Azure row):")
    print("\n".join(la))
    print("\nConfirmation (Addendum 11 part B, Azure):")
    print("\n".join(lb))
    for name, s in sb.items():
        print(name, "errors", s["error_types"])


if __name__ == "__main__":
    main()
