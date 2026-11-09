"""Draw the protocol-v3 live benchmark figures from committed summaries only.

    python bench/make_v3_charts.py

Reads bench/results/protocol-v3-live/<run>/summary.json for the runs listed in
RUNS and writes assets/benchmark-v3.{svg,png} and assets/benchmark-v3-tasks.{svg,png}.
No number is typed in by hand; every plotted value comes from summary.json,
which is itself computed from rows.jsonl by bench/protocol_v3/live.py.
"""

from __future__ import annotations

import json
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
RESULTS = ROOT / "bench/results/protocol-v3-live"
# Display label -> committed run directory. Order is the plotting order.
RUNS = json.loads((RESULTS / "chart-runs.json").read_text())
MODES = (("weave", "Weave (one script per turn)", "#2457d6"), ("native", "Native tool calling (parallel)", "#a3a9b4"))

plt.rcParams.update(
    {
        "font.family": ["Inter", "Helvetica Neue", "Helvetica", "Arial", "DejaVu Sans"],
        "font.size": 10,
        "axes.spines.top": False,
        "axes.spines.right": False,
        "axes.spines.left": False,
        "axes.edgecolor": "#d0d4da",
        "axes.titleweight": "semibold",
        "axes.titlesize": 11.5,
        "axes.titlelocation": "left",
        "axes.titlepad": 10,
        "ytick.left": False,
        "xtick.color": "#3b4250",
        "ytick.color": "#6b7280",
        "svg.fonttype": "none",
    }
)


def load():
    data = {}
    for label, run in RUNS.items():
        data[label] = json.loads((RESULTS / run / "summary.json").read_text())
    return data


def err(center, interval):
    return [[center - interval[0]], [interval[1] - center]]


def panel(ax, data, title, key, ci_key, fmt, scale=1.0, ylim=None, ylabel=""):
    labels = list(data)
    width = 0.36
    for j, (mode, name, color) in enumerate(MODES):
        for i, label in enumerate(labels):
            s = data[label]["summary"][mode]
            value = s[key] * scale
            ci = [c * scale for c in s[ci_key]]
            x = i + (j - 0.5) * width
            ax.bar(x, value, width * 0.92, color=color, label=name if i == 0 else None, zorder=2)
            ax.errorbar(x, value, yerr=err(value, ci), fmt="none", ecolor="#1f2937", elinewidth=1, capsize=3, zorder=3)
            ax.text(x, ci[1] + (ylim[1] if ylim else max(1, value)) * 0.015, fmt(value), ha="center", va="bottom", fontsize=8.5, color="#111827")
    ax.set_xticks(range(len(labels)), labels)
    ax.set_title(title)
    ax.set_ylabel(ylabel, color="#6b7280")
    ax.grid(axis="y", color="#eceef2", zorder=0)
    if ylim:
        ax.set_ylim(*ylim)
    ax.margins(y=0.15)


def overview(data):
    fig, axes = plt.subplots(2, 2, figsize=(11, 7.4), dpi=200)
    panel(axes[0][0], data, "Task success (higher is better)", "pass_rate", "pass_rate_wilson95", lambda v: f"{v:.0f}%", 100, (0, 112), "% of trials passed")
    panel(axes[0][1], data, "Model turns per task (lower is better)", "mean_model_turns", "mean_model_turns_boot95", lambda v: f"{v:.2f}", ylabel="mean provider turns")
    panel(axes[1][0], data, "Wall-clock per task, incl. retries (lower is better)", "mean_wall_s", "mean_wall_s_boot95", lambda v: f"{v:.1f}s", ylabel="mean seconds")
    panel(axes[1][1], data, "Tokens per task (lower is better)", "mean_total_tokens", "mean_total_tokens_boot95", lambda v: f"{v / 1000:.1f}k", ylabel="mean input+output tokens")
    first = next(iter(data.values()))
    n = first["summary"]["weave"]["trials"]
    tasks = len(first["task_manifest"]["tasks"])
    fig.suptitle("Weave vs native tool calling — live, protocol v3", x=0.055, y=0.985, ha="left", fontsize=15, fontweight="bold")
    fig.text(
        0.055,
        0.935,
        f"{tasks} frozen file/git/memory tasks × {first['repeats']} repeats = {n} trials per mode per model · same tools, context and backend (weave-mcp) in both modes · "
        "graded on the model's final answer + actual on-disk state",
        fontsize=9,
        color="#4b5563",
    )
    handles, names = axes[0][0].get_legend_handles_labels()
    fig.legend(handles, names, loc="upper right", bbox_to_anchor=(0.985, 0.995), frameon=False, ncol=1, fontsize=9.5)
    dates = sorted({d["started_utc"][:10] for d in data.values()})
    fig.text(
        0.055,
        0.012,
        f"Error bars: 95% Wilson interval (success) or 95% bootstrap interval of the mean (others). Failures stay in every denominator. "
        f"OpenAI Responses API, runs started {', '.join(dates)} UTC.\n"
        "Small, public, maintainer-authored task set: not statistically independent, not held out, one provider. "
        "Methodology: docs/benchmark-protocol-v3.md · raw data: bench/results/protocol-v3-live/",
        fontsize=7.8,
        color="#6b7280",
    )
    fig.tight_layout(rect=(0.03, 0.05, 1, 0.92))
    return fig


def per_task(data):
    labels = list(data)
    task_ids = list(next(iter(data.values()))["summary"]["weave"]["by_task"])
    fig, axes = plt.subplots(1, len(labels), figsize=(4.1 * len(labels), 4.6), dpi=200, sharey=True)
    if len(labels) == 1:
        axes = [axes]
    for ax, label in zip(axes, labels):
        by = {m: data[label]["summary"][m]["by_task"] for m, _, _ in MODES}
        ys = range(len(task_ids))
        for j, (mode, name, color) in enumerate(MODES):
            vals = [by[mode][t]["mean_model_turns"] for t in task_ids]
            ax.barh([y + (j - 0.5) * 0.38 for y in ys], vals, 0.36, color=color, label=name, zorder=2)
            for y, t, v in zip(ys, task_ids, vals):
                g = by[mode][t]
                ax.text(v + 0.08, y + (j - 0.5) * 0.38, f"{v:.1f}  ({g['passed']}/{g['trials']})", va="center", fontsize=7, color="#374151")
        ax.set_yticks(list(ys), task_ids)
        ax.invert_yaxis()
        ax.set_title(label)
        ax.set_xlabel("mean model turns (passed/trials)", color="#6b7280")
        ax.grid(axis="x", color="#eceef2", zorder=0)
        ax.spines["bottom"].set_visible(False)
        ax.tick_params(axis="x", bottom=False)
        ax.set_xlim(0, 11.5)
    handles, names = axes[0].get_legend_handles_labels()
    fig.suptitle("Per-task model turns, protocol v3 live", x=0.02, ha="left", fontsize=13, fontweight="bold")
    fig.legend(handles, names, loc="upper right", frameon=False, ncol=2, fontsize=8.5)
    fig.tight_layout(rect=(0, 0, 1, 0.93))
    return fig


def main():
    data = load()
    for name, fig in (("benchmark-v3", overview(data)), ("benchmark-v3-tasks", per_task(data))):
        for ext in ("svg", "png"):
            fig.savefig(ROOT / "assets" / f"{name}.{ext}", facecolor="white", metadata={"Date": None} if ext == "svg" else None)
        plt.close(fig)
    print("wrote assets/benchmark-v3.{svg,png} and assets/benchmark-v3-tasks.{svg,png}")


if __name__ == "__main__":
    main()
