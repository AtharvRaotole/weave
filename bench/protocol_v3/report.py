"""Print markdown tables for protocol-v3 live runs from their summary.json files.

    python -m bench.protocol_v3.report bench/results/protocol-v3-live/<run> [...]

Used to produce the tables in README.md and docs/benchmarks.md, so that no
number is typed by hand.
"""

import json
import sys
from pathlib import Path


def pct(x):
    return f"{100 * x:.0f}%"


def ci(pair, f):
    return f"{f(pair[0])}–{f(pair[1])}"


def main(paths):
    print("| Model (run) | Mode | Passed | 95% CI | First try | Mean turns | p50 / p95 wall | Mean tokens / trial | Failures |")
    print("| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |")
    for p in paths:
        s = json.loads((Path(p) / "summary.json").read_text())
        model = s["config"]["model_revision"]
        effort = s["config"]["generation"]["reasoning"]
        label = f"`{model}`" + (f" ({effort})" if effort else " (temp 0)")
        for mode in ("weave", "native"):
            x = s["summary"][mode]
            fails = ", ".join(f"{k} {v}" for k, v in x["failures"].items()) or "none"
            print(
                f"| {label} | {mode} | {x['passed']}/{x['trials']} | {ci(x['pass_rate_wilson95'], pct)} | "
                f"{x['first_pass']}/{x['trials']} | {x['mean_model_turns']:.2f} | "
                f"{x['p50_wall_s']:.1f}s / {x['p95_wall_s']:.1f}s | {x['mean_total_tokens']:,.0f} | {fails} |"
            )
    print()
    print("| Model | Run id | Started (UTC) | Source commit | HTTP requests | Input / output / cached tokens (both modes) |")
    print("| --- | --- | --- | --- | ---: | --- |")
    for p in paths:
        s = json.loads((Path(p) / "summary.json").read_text())
        t = {k: sum(s["summary"][m]["known_tokens"][k] for m in ("weave", "native")) for k in ("input_tokens", "output_tokens", "cached_input_tokens")}
        print(
            f"| `{s['config']['model_revision']}` | [`{s['run_id']}`](../bench/results/protocol-v3-live/{s['run_id']}/summary.json) | {s['started_utc']} | `{s['source_commit'][:7]}` | {s['http_requests_sent']} | "
            f"{t['input_tokens']:,} / {t['output_tokens']:,} / {t['cached_input_tokens']:,} |"
        )


if __name__ == "__main__":
    main(sys.argv[1:])
