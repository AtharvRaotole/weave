"""Measure Weave against sequential tool calls.

Prints only timings this process actually measured.
"""

import time

from weave import Engine

DELAY_S = 0.12
LABELS = ("a", "b", "c", "d")


def slow(name: str) -> str:
    time.sleep(DELAY_S)
    return name.upper()


def sequential() -> tuple[float, list[str]]:
    started = time.perf_counter()
    outputs = [slow(label) for label in LABELS]
    return time.perf_counter() - started, outputs


def weave_parallel() -> tuple[float, object]:
    engine = Engine()
    engine.tool("slow", slow, effect="read")
    lines = [f'{label} = slow("{label}")' for label in LABELS]
    lines.extend(f"print({label})" for label in LABELS)
    source = "\n".join(lines) + "\n"
    started = time.perf_counter()
    result = engine.run(source)
    elapsed = time.perf_counter() - started
    replayed = engine.replay(source, result.trace_json())
    if replayed.stdout != result.stdout or dict(replayed.bindings) != dict(result.bindings):
        raise SystemExit("replay did not reproduce the original output")
    return elapsed, result


def main() -> None:
    seq_s, seq_out = sequential()
    weave_s, result = weave_parallel()
    if weave_s * 1.5 >= seq_s:
        raise SystemExit(
            f"weave was not faster than sequential (weave_s={weave_s:.6f} sequential_s={seq_s:.6f})"
        )
    print(f"sequential_s={seq_s:.6f}")
    print(f"weave_s={weave_s:.6f}")
    print(f"speedup={seq_s / weave_s:.3f}")
    print("sequential_output=" + ",".join(seq_out))
    print("weave_stdout:")
    print(result.stdout, end="")


if __name__ == "__main__":
    main()
