"""Figures and tables for benchmark v4, computed only from committed results.

    python bench/v4/charts.py

Reads bench/results/v4/runs.json (label -> run directory under bench/results/v4 or v5) and each run's
rows.jsonl, and writes assets/benchmark-v4*.{svg,png}, assets/benchmark-v5*.{svg,png} and
docs/paper/results-tables.md. No number is typed in by hand.
"""

from __future__ import annotations

import json
import logging
import statistics
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from bench.v4 import stats  # noqa: E402

RESULTS = ROOT / "bench/results/v4"
ASSETS = ROOT / "assets"
TABLES = ROOT / "docs/paper/results-tables.md"
BLUE, GREY = "#2457d6", "#a3a9b4"


def load(run):
    d = RESULTS / run
    for newer in ("v5", "v6"):
        if not d.exists():
            d = ROOT / "bench/results" / newer / run
    rows = [json.loads(line) for line in (d / "rows.jsonl").read_text().splitlines() if line.strip()]
    return json.loads((d / "run.json").read_text()), rows


def sel(rows, **kw):
    return [r for r in rows if all(r[k] == v for k, v in kw.items())]


def fmt_ci(ci, nd=2):
    return f"[{ci[0]:.{nd}f}, {ci[1]:.{nd}f}]" if ci else "n/a"


def headline(runs, suite="held_out"):
    """One row per (model, latency): pass rates, means, paired differences."""
    out = []
    for label, run in runs.get(suite, {}).items():
        meta, rows = load(run)
        for lat in sorted({r["latency_ms"] for r in rows}, reverse=True):
            g = [r for r in sel(rows, latency_ms=lat) if r["mode"] in ("weave", "native")]
            dw, dn = stats.describe(sel(g, mode="weave")), stats.describe(sel(g, mode="native"))
            out.append({
                "label": label, "latency": lat, "meta": meta, "weave": dw, "native": dn,
                "turns": stats.paired(g, lambda r: r["turns"]), "wall": stats.paired(g, lambda r: r["wall_s"], 1),
                "tokens": stats.paired(g, lambda r: r["total_tokens"], 2), "mcnemar": stats.mcnemar_exact(g),
                "spent": sum(r["total_tokens"] for r in rows) if lat == max(r["latency_ms"] for r in rows) else 0,
            })
    return out


def families(runs, lat=200.0, suite="held_out", modes=("weave", "native")):
    """Per model and family: mean turns, wall, tokens per mode, and pass counts."""
    out = {}
    for label, run in runs.get(suite, {}).items():
        _, rows = load(run)
        rows = sel(rows, latency_ms=lat)
        for fam in sorted({r["family"] for r in rows}):
            for mode in modes:
                g = sel(rows, family=fam, mode=mode)
                if g:
                    out[(label, fam, mode)] = {
                        "n": len(g), "passed": sum(r["passed"] for r in g),
                        "turns": statistics.fmean(r["turns"] for r in g),
                        "wall": statistics.fmean(r["wall_s"] for r in g),
                        "exec": statistics.fmean(r["exec_s"] for r in g),
                        "tokens": statistics.fmean(r["total_tokens"] for r in g),
                        "wall_net": statistics.fmean(r["wall_s"] - r["retry_wait_s"] for r in g)}
    return out


def family_table(fams, title, wall="wall"):
    lines = ["", title, "",
             "| model | family | pass W / N | turns W / N | wall s W / N | script/tool exec s W / N | tokens W / N |",
             "|---|---|---|---|---|---|---|"]
    for (label, fam, mode), v in fams.items():
        if mode != "weave" or (label, fam, "native") not in fams:
            continue
        n = fams[(label, fam, "native")]
        lines.append(f"| {label} | {fam} | {v['passed']}/{v['n']} / {n['passed']}/{n['n']} | {v['turns']:.2f} / {n['turns']:.2f} | "
                     f"{v[wall]:.2f} / {n[wall]:.2f} | {v['exec']:.2f} / {n['exec']:.2f} | {v['tokens']:.0f} / {n['tokens']:.0f} |")
    return lines


def as_native(rows, mode):
    """Rows of `mode` relabelled `native`, so stats.paired compares Weave with that arm."""
    return [dict(r, mode="native") for r in rows if r["mode"] == mode] + [r for r in rows if r["mode"] == "weave"]


WRITE_LOOPS = ("triage", "notify_gold")


def footprint_section(runs):
    """F1/F2/F3 on confirm2: execution time on write-loop families, Weave vs native and vs weave_nofp."""
    lines, data = [], {}
    entries = runs.get("confirm2", {})
    if not entries:
        return lines, data
    lines += ["", "## Footprint-aware ordering on `confirm2` (write-loop families: triage, notify_gold; 200 ms)", "",
              "Ratios are geometric means of paired (task, repeat) `exec_s` with 95% cluster-bootstrap CIs.", "",
              "| model | exec s W / native / W-no-footprints | W / native [95% CI] | W / W-no-footprints [95% CI] | pass W / native / no-fp (whole suite) |",
              "|---|---|---|---|---|"]
    for label, run in entries.items():
        _, rows = load(run)
        wl = [r for r in rows if r["family"] in WRITE_LOOPS]
        m = {mode: statistics.fmean(r["exec_s"] for r in wl if r["mode"] == mode) for mode in ("weave", "native", "weave_nofp")
             if any(r["mode"] == mode for r in wl)}
        vs_n = stats.paired(wl, lambda r: r["exec_s"], 3)
        vs_fp = stats.paired(as_native(wl, "weave_nofp"), lambda r: r["exec_s"], 4) if "weave_nofp" in m else None
        p = {mode: f"{sum(r['passed'] for r in rows if r['mode'] == mode)}/{sum(r['mode'] == mode for r in rows)}"
             for mode in ("weave", "native", "weave_nofp")}
        data[label] = {"exec": m, "vs_native": vs_n, "vs_nofp": vs_fp}
        lines.append(f"| {label} | {m.get('weave', 0):.2f} / {m.get('native', 0):.2f} / {m.get('weave_nofp', float('nan')):.2f} | "
                     f"{vs_n['ratio_gm']:.2f} {fmt_ci(vs_n['ratio_gm_ci95'])} | "
                     + (f"{vs_fp['ratio_gm']:.2f} {fmt_ci(vs_fp['ratio_gm_ci95'])}" if vs_fp else "n/a")
                     + f" | {p['weave']} / {p['native']} / {p['weave_nofp']} |")
    return lines, data


def echo_section(runs):
    """E1: Weave scripts whose last statement is a bare expression, and what they got back."""
    import ast
    lines = ["", "## Final-expression echo on `confirm2` (E1)", "",
             "| model | Weave scripts | ending in a bare expression | of those, got 'printed nothing' |", "|---|---|---|---|"]
    for label, run in runs.get("confirm2", {}).items():
        _, rows = load(run)
        total = bare = empty = 0
        for r in rows:
            if not r["mode"].startswith("weave"):
                continue
            for s in r["scripts_log"]:
                total += 1
                try:
                    body = ast.parse(s["source"]).body
                except SyntaxError:
                    continue
                if body and isinstance(body[-1], ast.Expr) and not (
                        isinstance(body[-1].value, ast.Call) and getattr(body[-1].value.func, "id", "") == "print"):
                    bare += 1
                    empty += "printed nothing" in s["output"]
        lines.append(f"| {label} | {total} | {bare} | {empty} |")
    return lines


SUGAR_MESSAGES = ("comprehensions are not supported", "`lambda` is not supported", "`def` is not supported",
                  "list has no method `sort`")


def static_section(runs, suites=("confirm3", "real2")):
    """S1: every Weave script that failed the static check on the round-3 suites, by message."""
    import re
    lines = ["", "## Static-check failures in Weave scripts, round 3 (S1)", "",
             "Every Weave script on `confirm3` and `real2` that the static check rejected, counted by message "
             "(line numbers stripped). The round-2 messages for `def`, `lambda`, comprehensions and `list.sort` "
             "no longer exist in the engine; any count against them would refute S1.", "",
             "| model | suite | Weave scripts | rejected by the static check | sugar rejections (round-2 messages) | messages |",
             "|---|---|---|---|---|---|"]
    for suite in suites:
        for label, run in runs.get(suite, {}).items():
            _, rows = load(run)
            total = rejected = sugar = 0
            msgs = {}
            for r in rows:
                if not r["mode"].startswith("weave"):
                    continue
                for sc in r["scripts_log"]:
                    total += 1
                    out = sc["output"]
                    if "did not pass the static check" not in out:
                        continue
                    rejected += 1
                    sugar += any(m in out for m in SUGAR_MESSAGES)
                    for m in re.findall(r"L\d+:C\d+: ([^\n]+)", out):
                        m = m.split(" hint:")[0].strip()
                        msgs[m] = msgs.get(m, 0) + 1
            listed = "; ".join(f"{k} ({v})" for k, v in sorted(msgs.items(), key=lambda kv: -kv[1])) or "none"
            lines.append(f"| {label} | {suite} | {total} | {rejected} | {sugar} | {listed.replace('|', '/')} |")
    return lines


def headline_net(runs, suite):
    """Like headline, with wall time net of provider waits (free-tier pacing, retries)."""
    out = []
    for label, run in runs.get(suite, {}).items():
        meta, rows = load(run)
        g = [r for r in rows if r["mode"] in ("weave", "native")]
        out.append({"label": label, "latency": rows[0]["latency_ms"] if rows else 0, "meta": meta,
                    "weave": stats.describe(sel(g, mode="weave")), "native": stats.describe(sel(g, mode="native")),
                    "turns": stats.paired(g, lambda r: r["turns"]),
                    "wall": stats.paired(g, lambda r: r["wall_s"] - r["retry_wait_s"], 1),
                    "tokens": stats.paired(g, lambda r: r["total_tokens"], 2), "mcnemar": stats.mcnemar_exact(g),
                    "statuses": sorted({r["status"] for r in rows}), "n": len(rows)})
    return out


def headline_table(head):
    lines = ["| model | latency | pass weave | pass native | McNemar p | turns W / N | Δ turns [95% CI] | wall s W / N | Δ wall s [95% CI] | tokens W / N | token ratio [95% CI] |",
             "|---|---|---|---|---|---|---|---|---|---|---|"]
    for h in head:
        w, n = h["weave"], h["native"]
        lines.append(
            f"| {h['label']} | {h['latency']:.0f} ms | {w['passed']}/{w['trials']} {fmt_ci(w['pass_wilson95'])} | "
            f"{n['passed']}/{n['trials']} {fmt_ci(n['pass_wilson95'])} | {h['mcnemar']['p']:.3f} | "
            f"{w['mean_turns']:.2f} / {n['mean_turns']:.2f} | {h['turns']['mean_diff']:+.2f} {fmt_ci(h['turns']['mean_diff_ci95'])} | "
            f"{w['mean_wall_s']:.2f} / {n['mean_wall_s']:.2f} | {h['wall']['mean_diff']:+.2f} {fmt_ci(h['wall']['mean_diff_ci95'])} | "
            f"{w['mean_tokens']:.0f} / {n['mean_tokens']:.0f} | {h['tokens']['ratio_gm']:.2f} {fmt_ci(h['tokens']['ratio_gm_ci95'])} |")
    return lines


def write_tables(runs, head, fams, scaling, extra):
    lines = ["# Benchmark v4 results (generated by `bench/v4/charts.py`; do not edit)", ""]
    lines += ["## Held-out suite, per model and latency", "",
              "Differences are Weave minus native: mean over tasks of per-task means, 95% cluster bootstrap over tasks. "
              "Ratio is the geometric mean of per-trial Weave/native.", ""]
    lines += headline_table(head)
    confirm = headline(runs, "confirm")
    if confirm:
        lines += ["", "## Confirm suite (fresh seeds, after the post-held-out fixes)", ""] + headline_table(confirm)
    confirm2 = headline(runs, "confirm2")
    if confirm2:
        lines += ["", "## Confirm2 suite (round 2: fresh seeds, footprints and echo; weave vs native)", ""] + headline_table(confirm2)
    real = headline_net(runs, "real")
    if real:
        lines += ["", "## Real services (`real` suite: SQLite, git, filesystem, local HTTP server, PyPI; no injected latency)", "",
                  "Wall time is net of provider waits (zero for OpenAI; pacing and retries for free tiers).", ""]
        lines += [x.replace(" 0 ms |", " real |") for x in headline_table(real)]
    confirm3 = headline(runs, "confirm3")
    if confirm3:
        lines += ["", "## Confirm3 suite (round 3: fresh seeds, Python sugar; weave vs native)", ""] + headline_table(confirm3)
    real2 = headline_net(runs, "real2")
    if real2:
        lines += ["", "## Real services, round 3 (`real2`: fresh seeds; no injected latency)", "",
                  "Wall time is net of provider waits (zero for OpenAI).", ""]
        lines += [x.replace(" 0 ms |", " real |") for x in headline_table(real2)]
    partial3 = headline_net(runs, "partial3")
    if partial3:
        lines += ["", "## Best-effort providers, round 3 (not pooled; wall net of pacing)", "",
                  "| model | suite | trials run | statuses |", "|---|---|---|---|"]
        lines += [f"| {h['label']} | {h['meta']['suite']} | {h['n']} | {', '.join(h['statuses'])} |" for h in partial3]
        lines += ["", "No (task, repeat) pair finished in both arms, so no paired comparison is reported."]
    partial = headline_net(runs, "partial2")
    if partial:
        lines += ["", "## Best-effort providers, round 2 (not pooled; wall net of pacing)", "",
                  "| model | suite | trials run | statuses |", "|---|---|---|---|"]
        lines += [f"| {h['label']} | {h['meta']['suite']} | {h['n']} | {', '.join(h['statuses'])} |" for h in partial]
        lines += [""] + headline_table(partial)
    lines += family_table(fams, "## Held-out by family at 200 ms (mean per trial)")
    if confirm2:
        lines += family_table(families(runs, 200.0, "confirm2"), "## Confirm2 by family at 200 ms (mean per trial)")
    if real:
        lines += family_table(families(runs, 0.0, "real"), "## Real services by family (mean per trial)")
    if confirm3:
        lines += family_table(families(runs, 200.0, "confirm3"), "## Confirm3 by family at 200 ms (mean per trial)")
    if real2:
        lines += family_table(families(runs, 0.0, "real2"), "## Real services, round 3 (`real2`), by family (mean per trial)")
    if scaling:
        lines += ["", "## Scaling suite (mean per trial)", "",
                  "| model | latency | task | pass W / N | turns W / N | wall s W / N | tokens W / N |", "|---|---|---|---|---|---|---|"]
        for (label, lat, task), v in sorted(scaling.items(), key=lambda kv: (kv[0][0], -kv[0][1], kv[0][2].split('-')[0], int(kv[0][2].split('-')[1][1:]))):
            w, n = v.get("weave"), v.get("native")
            if w and n:
                lines.append(f"| {label} | {lat:.0f} ms | {task} | {w['passed']}/{w['n']} / {n['passed']}/{n['n']} | "
                             f"{w['turns']:.2f} / {n['turns']:.2f} | {w['wall']:.2f} / {n['wall']:.2f} | {w['tokens']:.0f} / {n['tokens']:.0f} |")
        lines += ["", "Least-squares slopes over the scaling suite (per-trial means):", "",
                  "| model | latency | fan-out tokens per item W / N | chain turns per hop W / N |", "|---|---|---|---|"]
        for label, lat in sorted({(k[0], k[1]) for k in scaling}, key=lambda x: (x[0], -x[1])):
            def slope(fam, mode, key):
                pts = [(v[mode]["size"], v[mode][key]) for (lab, la, t), v in scaling.items()
                       if lab == label and la == lat and mode in v and v[mode]["family"] == fam]
                if len(pts) < 2:
                    return float("nan")
                mx, my = statistics.fmean(p[0] for p in pts), statistics.fmean(p[1] for p in pts)
                return sum((x - mx) * (y - my) for x, y in pts) / sum((x - mx) ** 2 for x, _ in pts)
            lines.append(f"| {label} | {lat:.0f} ms | {slope('fanout_sum', 'weave', 'tokens'):.1f} / {slope('fanout_sum', 'native', 'tokens'):.1f} | "
                         f"{slope('chain', 'weave', 'turns'):.2f} / {slope('chain', 'native', 'turns'):.2f} |")
    lines += extra
    lines += ["", "## Runs", "", "| label | suite | run directory | source commit | trials | total tokens |", "|---|---|---|---|---|---|"]
    for suite, entries in runs.items():
        for label, run in entries.items():
            meta, rows = load(run)
            lines.append(f"| {label} | {suite} | `{run}` | `{meta['source_commit'][:7]}` | {len(rows)} | {sum(r['total_tokens'] for r in rows):,} |")
    TABLES.write_text("\n".join(lines) + "\n")


def scaling_data(runs):
    out = {}
    for label, run in runs.get("scaling", {}).items():
        _, rows = load(run)
        for r in rows:
            out.setdefault((label, r["latency_ms"], r["task"]), {}).setdefault(r["mode"], []).append(r)
    agg = {}
    for key, modes in out.items():
        agg[key] = {m: {"n": len(g), "passed": sum(r["passed"] for r in g), "size": g[0]["size"], "family": g[0]["family"],
                        "turns": statistics.fmean(r["turns"] for r in g), "wall": statistics.fmean(r["wall_s"] for r in g),
                        "tokens": statistics.fmean(r["total_tokens"] for r in g)} for m, g in modes.items()}
    return agg


def ablation(runs):
    lines = []
    for label, run in runs.get("ablation", {}).items():
        _, rows = load(run)
        lines += ["", f"## Ablation: loop pipelining off ({label}, 200 ms)", "",
                  "| family | pass pipelined / off | exec s pipelined / off | wall s pipelined / off |", "|---|---|---|---|"]
        for fam in sorted({r["family"] for r in rows}):
            a, b = sel(rows, family=fam, mode="weave"), sel(rows, family=fam, mode="weave_nopipe")
            if a and b:
                m = lambda g, k: statistics.fmean(r[k] for r in g)
                lines.append(f"| {fam} | {sum(r['passed'] for r in a)}/{len(a)} / {sum(r['passed'] for r in b)}/{len(b)} | "
                             f"{m(a, 'exec_s'):.2f} / {m(b, 'exec_s'):.2f} | {m(a, 'wall_s'):.2f} / {m(b, 'wall_s'):.2f} |")
    return lines


def charts_v5(runs, fp):
    import matplotlib.pyplot as plt
    if fp:
        labels = list(fp)
        modes = (("weave", "Weave", BLUE), ("weave_nofp", "Weave, no footprints", "#9db5ee"), ("native", "native", GREY))
        fig, ax = plt.subplots(figsize=(7.2, 2.8))
        width = 0.26
        for k, (mode, name, color) in enumerate(modes):
            ys = [fp[l]["exec"].get(mode, 0) for l in labels]
            xs = [j + (k - 1) * width for j in range(len(labels))]
            ax.bar(xs, ys, width * 0.92, color=color, label=name)
            for x, y in zip(xs, ys):
                ax.text(x, y + 0.03, f"{y:.2f}", ha="center", va="bottom", fontsize=7)
        ax.set_xticks(range(len(labels)), labels)
        ax.set_ylabel("tool/script execution s per trial")
        ax.set_title("confirm2, write-loop families (triage, notify_gold), 200 ms per call", fontsize=9)
        ax.legend(frameon=False, fontsize=7, ncol=3, loc="upper center", bbox_to_anchor=(0.5, -0.1))
        fig.tight_layout()
        save(fig, "benchmark-v5-footprints")
    real = [h for h in headline(runs, "real")]
    if real:
        metrics = (("turns", "model turns"), ("wall", "wall time"), ("tokens", "total tokens"))
        fig, ax = plt.subplots(figsize=(7.2, 2.9))
        width = 0.8 / max(1, len(real))
        for i, h in enumerate(real):
            xs = [j + (i - (len(real) - 1) / 2) * width for j in range(len(metrics))]
            ys = [h[m]["ratio_gm"] for m, _ in metrics]
            lo = [y - h[m]["ratio_gm_ci95"][0] for y, (m, _) in zip(ys, metrics)]
            hi = [h[m]["ratio_gm_ci95"][1] - y for y, (m, _) in zip(ys, metrics)]
            ax.bar(xs, ys, width * 0.9, yerr=[lo, hi], capsize=2, label=h["label"],
                   color=plt.cm.Greens(0.45 + 0.5 * i / max(1, len(real) - 1)))
            for x, y in zip(xs, ys):
                ax.text(x, 0.03, f"{y:.2f}", ha="center", va="bottom", fontsize=7, color="white")
        ax.axhline(1.0, color="#333", lw=0.8, ls="--")
        ax.set_xticks(range(len(metrics)), [m for _, m in metrics])
        ax.set_ylabel("Weave / native (geo. mean)")
        ax.set_title("Real services (SQLite, git, files, HTTP, PyPI): below 1 means Weave used less", fontsize=9)
        ax.legend(frameon=False, fontsize=7, ncol=len(real), loc="upper center", bbox_to_anchor=(0.5, -0.12))
        fig.tight_layout()
        save(fig, "benchmark-v5-real")


def charts_v6(runs):
    """Round 2 (`real`) vs round 3 (`real2`): Weave/native wall time and turns per OpenAI model."""
    import matplotlib.pyplot as plt
    before = {h["label"]: h for h in headline(runs, "real")}
    after = {h["label"]: h for h in headline(runs, "real2")}
    labels = [l for l in after if l in before]
    if not labels:
        return
    fig, axes = plt.subplots(1, 2, figsize=(7.2, 2.9), sharey=True)
    for ax, (metric, title) in zip(axes, (("wall", "wall time"), ("turns", "model turns"))):
        for k, (data, name, color) in enumerate(((before, "round 2 (`real`)", GREY), (after, "round 3 (`real2`)", BLUE))):
            xs = [j + (k - 0.5) * 0.38 for j in range(len(labels))]
            ys = [data[l][metric]["ratio_gm"] for l in labels]
            lo = [y - data[l][metric]["ratio_gm_ci95"][0] for y, l in zip(ys, labels)]
            hi = [data[l][metric]["ratio_gm_ci95"][1] - y for y, l in zip(ys, labels)]
            ax.bar(xs, ys, 0.36, yerr=[lo, hi], capsize=2, color=color, label=name)
            for x, y in zip(xs, ys):
                ax.text(x, 0.03, f"{y:.2f}", ha="center", va="bottom", fontsize=7, color="white")
        ax.axhline(1.0, color="#333", lw=0.8, ls="--")
        ax.set_xticks(range(len(labels)), labels, fontsize=7)
        ax.set_title(f"Weave / native {title}", fontsize=9)
    axes[0].set_ylabel("geo. mean ratio (below 1: Weave less)")
    axes[0].legend(frameon=False, fontsize=7, loc="upper left")
    fig.suptitle("Real services before and after Python sugar (fresh seeds in round 3)", fontsize=9)
    fig.tight_layout()
    save(fig, "benchmark-v6-real")


def charts(head, fams, scaling):
    import matplotlib
    matplotlib.use("Agg")
    logging.getLogger("matplotlib.font_manager").setLevel(logging.ERROR)
    import matplotlib.pyplot as plt
    plt.rcParams.update({"font.size": 9, "axes.spines.top": False, "axes.spines.right": False, "svg.hashsalt": "weave"})

    # Figure 1: Weave/native ratios with CIs, per model, at 200 ms.
    rows = [h for h in head if h["latency"] == 200.0]
    metrics = (("turns", "model turns"), ("wall", "wall time"), ("tokens", "total tokens"))
    fig, ax = plt.subplots(figsize=(7.2, 2.9))
    width = 0.8 / max(1, len(rows))
    for i, h in enumerate(rows):
        xs = [j + (i - (len(rows) - 1) / 2) * width for j in range(len(metrics))]
        ys = [h[m]["ratio_gm"] for m, _ in metrics]
        lo = [y - h[m]["ratio_gm_ci95"][0] for y, (m, _) in zip(ys, metrics)]
        hi = [h[m]["ratio_gm_ci95"][1] - y for y, (m, _) in zip(ys, metrics)]
        ax.bar(xs, ys, width * 0.9, yerr=[lo, hi], capsize=2, label=h["label"], color=plt.cm.Blues(0.45 + 0.5 * i / max(1, len(rows) - 1)))
        for x, y in zip(xs, ys):
            ax.text(x, 0.03, f"{y:.2f}", ha="center", va="bottom", fontsize=7, color="white")
    ax.axhline(1.0, color="#333", lw=0.8, ls="--")
    ax.set_xticks(range(len(metrics)), [m for _, m in metrics])
    ax.set_ylabel("Weave / native (geo. mean)")
    ax.set_title("Held-out suite, 200 ms per backend call: below 1 means Weave used less", fontsize=9)
    ax.legend(frameon=False, fontsize=7, ncol=len(rows), loc="upper center", bbox_to_anchor=(0.5, -0.12))
    fig.tight_layout()
    save(fig, "benchmark-v4")

    # Figure 2: per-family wall time at 200 ms for each model.
    labels = sorted({k[0] for k in fams}, key=[h["label"] for h in rows].index)
    fam_names = sorted({k[1] for k in fams})
    fig, axes = plt.subplots(1, len(labels), figsize=(2.6 * len(labels), 2.9), sharey=True, squeeze=False)
    for ax, label in zip(axes[0], labels):
        for k, (mode, color) in enumerate((("weave", BLUE), ("native", GREY))):
            ys = [fams.get((label, f, mode), {}).get("wall", 0) for f in fam_names]
            ax.barh([j + (k - 0.5) * 0.38 for j in range(len(fam_names))], ys, 0.36, color=color, label=mode)
        ax.set_yticks(range(len(fam_names)), fam_names)
        ax.invert_yaxis()
        ax.set_xlabel("mean wall time (s)")
        ax.set_title(label, fontsize=8)
    axes[0][0].legend(frameon=False, fontsize=7, loc="lower right")
    fig.suptitle("Held-out suite by task family, 200 ms per backend call", fontsize=9)
    fig.tight_layout()
    save(fig, "benchmark-v4-families")

    # Figure 3: scaling.
    if scaling:
        fig, (a1, a2) = plt.subplots(1, 2, figsize=(7.2, 2.8))
        for mode, color in (("weave", BLUE), ("native", GREY)):
            for ax, fam, key, ylabel in ((a1, "fanout_sum", "tokens", "total tokens per trial"), (a2, "chain", "turns", "model turns per trial")):
                pts = sorted((v[mode]["size"], v[mode][key]) for (lab, lat, t), v in scaling.items()
                             if lat == 200.0 and mode in v and v[mode]["family"] == fam)
                if pts:
                    ax.plot([p[0] for p in pts], [p[1] for p in pts], "o-", color=color, label=mode)
                ax.set_ylabel(ylabel)
        a1.set_xlabel("fan-out width n (fanout_sum)")
        a2.set_xlabel("chain depth (chain)")
        a1.legend(frameon=False, fontsize=7)
        fig.tight_layout()
        save(fig, "benchmark-v4-scaling")


def save(fig, name):
    for ext in ("svg", "png"):
        fig.savefig(ASSETS / f"{name}.{ext}", dpi=180, metadata={"Date": None} if ext == "svg" else None)


def main():
    runs = json.loads((RESULTS / "runs.json").read_text())
    head = headline(runs)
    fams = families(runs)
    scaling = scaling_data(runs)
    fp_lines, fp = footprint_section(runs)
    extra = ablation(runs) + fp_lines + (echo_section(runs) if fp else [])
    if "confirm3" in runs or "real2" in runs:
        extra += static_section(runs)
    write_tables(runs, head, fams, scaling, extra)
    try:
        charts(head, fams, scaling)
        charts_v5(runs, fp)
        charts_v6(runs)
    except ImportError:
        print("matplotlib not installed: wrote tables only")
    print(f"wrote {TABLES.relative_to(ROOT)} and assets/benchmark-v4*.svg")


if __name__ == "__main__":
    main()
