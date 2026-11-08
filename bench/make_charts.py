"""Draw README charts from measured results. No numbers are typed in by hand.

    python bench/make_charts.py

Reads the M11 real-tools runs (one file per model) and writes
assets/benchmark.svg. assets/how-it-works.svg is a diagram, not data.
"""

from __future__ import annotations

import json
import statistics
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
RUNS = {
    "gpt-5.4-mini": "bench/results/real-20261108T175251Z.json",
    "gpt-6.1-sol": "bench/results/real-20261108T175500Z.json",
}
MODES = (("weave", "Weave (one script)", "#2563eb"), ("native_tools", "Native tool calling", "#9ca3af"))


def stats(path: str) -> dict:
    rows = json.loads((ROOT / path).read_text())["rows"]
    out = {}
    for mode, _, _ in MODES:
        group = [r for r in rows if r["mode"] == mode]
        out[mode] = {
            "median_s": statistics.median(r["wall_s"] for r in group),
            "mean_turns": sum(r["model_turns"] for r in group) / len(group),
            "passed": sum(r["passed"] for r in group),
            "runs": len(group),
        }
    return out


def bars(title: str, key: str, unit: str, data: dict, y0: int, scale: float) -> list[str]:
    parts = [f'<text x="24" y="{y0}" class="h">{title}</text>']
    y = y0 + 18
    for model, by_mode in data.items():
        parts.append(f'<text x="24" y="{y + 26}" class="m">{model}</text>')
        for mode, label, color in MODES:
            value = by_mode[mode][key]
            width = max(2.0, value * scale)
            parts.append(f'<rect x="150" y="{y}" width="{width:.1f}" height="18" rx="3" fill="{color}"/>')
            parts.append(f'<text x="{150 + width + 8:.1f}" y="{y + 13}" class="v">{value:.2f}{unit}</text>')
            y += 22
        y += 14
    return parts


def benchmark_svg() -> str:
    data = {model: stats(path) for model, path in RUNS.items()}
    width, height = 720, 420
    max_s = max(m["median_s"] for d in data.values() for m in d.values())
    max_t = max(m["mean_turns"] for d in data.values() for m in d.values())
    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" font-family="-apple-system,Segoe UI,Helvetica,Arial,sans-serif">',
        "<style>.h{font-size:15px;font-weight:600;fill:#111827}.m{font-size:13px;fill:#374151}.v{font-size:12px;fill:#111827}.n{font-size:11px;fill:#6b7280}</style>",
        f'<rect width="{width}" height="{height}" fill="#ffffff"/>',
    ]
    parts += bars("Median wall time per task (lower is better)", "median_s", " s", data, 30, 420 / max_s)
    parts += bars("Mean model turns per task (lower is better)", "mean_turns", "", data, 210, 420 / max_t)
    lx = 24
    for _, label, color in MODES:
        parts.append(f'<rect x="{lx}" y="{height - 36}" width="12" height="12" rx="2" fill="{color}"/>')
        parts.append(f'<text x="{lx + 18}" y="{height - 26}" class="m">{label}</text>')
        lx += 190
    passed = "; ".join(
        f'{model} {d["weave"]["passed"]}/{d["weave"]["runs"]} vs {d["native_tools"]["passed"]}/{d["native_tools"]["runs"]}'
        for model, d in data.items()
    )
    parts.append(f'<text x="24" y="{height - 46}" class="n">16 tasks x 3 repeats on 4 real MCP servers. Passed (Weave vs native): {passed}.</text>')
    parts.append(f'<text x="24" y="{height - 6}" class="n">Source: bench/results/{" and ".join(Path(p).name for p in RUNS.values())}</text>')
    parts.append("</svg>")
    return "\n".join(parts) + "\n"


def diagram_svg() -> str:
    w, h = 760, 304
    p = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" font-family="-apple-system,Segoe UI,Helvetica,Arial,sans-serif">',
        "<style>.t{font-size:15px;font-weight:600;fill:#111827}.s{font-size:12px;fill:#374151}.c{font-size:11px;fill:#ffffff;font-family:ui-monospace,Menlo,Consolas,monospace}.k{font-size:11px;fill:#111827;font-family:ui-monospace,Menlo,Consolas,monospace}</style>",
        f'<rect width="{w}" height="{h}" fill="#ffffff"/>',
        '<text x="20" y="28" class="t">Native tool calling: a model turn per step</text>',
        '<text x="20" y="294" class="s">Illustration. Measured turns are in the chart below.</text>',
        '<text x="400" y="28" class="t">Weave: one script, independent calls together</text>',
        '<line x1="380" y1="14" x2="380" y2="286" stroke="#e5e7eb" stroke-width="2"/>',
    ]
    # left: alternating model / tool boxes in time order
    y = 46
    steps = [("model", "decide: read a.txt"), ("tool", "read a.txt"), ("model", "decide: read b.txt"), ("tool", "read b.txt"),
             ("model", "decide: read c.txt"), ("tool", "read c.txt"), ("model", "answer")]
    for kind, label in steps:
        color = "#f59e0b" if kind == "model" else "#9ca3af"
        p.append(f'<rect x="20" y="{y}" width="230" height="26" rx="4" fill="{color}"/>')
        p.append(f'<text x="30" y="{y + 17}" class="c">{kind}: {label}</text>')
        y += 33
    p.append('<text x="290" y="168" class="s">time</text><line x1="272" y1="46" x2="272" y2="280" stroke="#6b7280" marker-end="url(#a)"/>')
    p.append('<defs><marker id="a" markerWidth="8" markerHeight="8" refX="4" refY="7" orient="auto"><path d="M0,0 L8,0 L4,8 z" fill="#6b7280"/></marker></defs>')
    # right: one model turn, then a script, then parallel calls
    p.append('<rect x="400" y="46" width="340" height="26" rx="4" fill="#f59e0b"/><text x="410" y="63" class="c">model: write one script</text>')
    code = ['a = fs_read_text_file(f"{root}/a.txt")', 'b = fs_read_text_file(f"{root}/b.txt")', 'c = fs_read_text_file(f"{root}/c.txt")', 'print(int(a) + int(b) + int(c))']
    p.append('<rect x="400" y="80" width="340" height="80" rx="4" fill="#f3f4f6" stroke="#e5e7eb"/>')
    for i, line in enumerate(code):
        p.append(f'<text x="410" y="{98 + i * 17}" class="k">{line.replace("{", "&#123;").replace("}", "&#125;")}</text>')
    for i, name in enumerate(("a.txt", "b.txt", "c.txt")):
        p.append(f'<rect x="{400 + i * 116}" y="172" width="108" height="26" rx="4" fill="#2563eb"/><text x="{410 + i * 116}" y="189" class="c">read {name}</text>')
    p.append('<text x="400" y="216" class="s">The three reads are independent, so they run together.</text>')
    p.append('<rect x="400" y="226" width="340" height="26" rx="4" fill="#10b981"/><text x="410" y="243" class="c">print the sum, write a replayable trace</text>')
    p.append('<text x="400" y="276" class="s">Writes (effect irreversible) wait until the script is complete.</text>')
    p.append("</svg>")
    return "\n".join(p) + "\n"


def main() -> None:
    (ROOT / "assets").mkdir(exist_ok=True)
    (ROOT / "assets" / "benchmark.svg").write_text(benchmark_svg())
    (ROOT / "assets" / "how-it-works.svg").write_text(diagram_svg())
    print("wrote assets/benchmark.svg and assets/how-it-works.svg")


if __name__ == "__main__":
    main()
