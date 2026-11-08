"""Deterministic simulated token stream versus run-after-complete.

    python bench/stream_bench.py

Writes time-to-first-tool-start and total latency. The token gaps and tool
sleep are real `time.sleep` calls in this process.
"""

from __future__ import annotations

import json
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from bench.harness import machine_info, median, utc_stamp  # noqa: E402
from bench.streaming import feed, iter_anthropic_events, iter_openai_chat_chunks  # noqa: E402

SOURCE = 'a = slow("a")\nb = slow("b")\nprint(a)\nprint(b)\n'
GAP_S = 0.012
TOOL_S = 0.08
CHUNK = 4
REPEATS = 3


class _Delta:
    def __init__(self, content: str) -> None:
        self.content = content


class _Choice:
    def __init__(self, content: str) -> None:
        self.delta = _Delta(content)


class _Chunk:
    def __init__(self, content: str) -> None:
        self.choices = [_Choice(content)]


def _pieces() -> list[str]:
    return [SOURCE[i : i + CHUNK] for i in range(0, len(SOURCE), CHUNK)]


def _openai_chunks() -> list[_Chunk]:
    return [_Chunk(piece) for piece in _pieces()]


def _tool(starts: list[float], origin: float):
    def slow(name: str) -> str:
        if not starts:
            starts.append(time.perf_counter() - origin)
        time.sleep(TOOL_S)
        return name.upper()

    return slow


def run_streaming() -> dict:
    from weave import Engine

    origin = time.perf_counter()
    starts: list[float] = []
    engine = Engine()
    engine.tool("slow", _tool(starts, origin), effect="read", arity=1)
    session = engine.session()

    def delayed(pieces):
        for text in pieces:
            time.sleep(GAP_S)
            yield text

    feed(session, delayed(iter_openai_chat_chunks(_openai_chunks())))
    result = session.finish()
    total = time.perf_counter() - origin
    if result.stdout.strip() != "A\nB":
        raise SystemExit(f"streaming output was {result.stdout!r}")
    return {"time_to_first_tool_s": starts[0], "total_s": total}


def run_after_complete() -> dict:
    from weave import Engine

    origin = time.perf_counter()
    # The model emits the same chunks before any execution starts.
    for _ in _pieces():
        time.sleep(GAP_S)
    starts: list[float] = []
    engine = Engine()
    engine.tool("slow", _tool(starts, origin), effect="read", arity=1)
    result = engine.run(SOURCE)
    total = time.perf_counter() - origin
    if result.stdout.strip() != "A\nB":
        raise SystemExit(f"batch output was {result.stdout!r}")
    return {"time_to_first_tool_s": starts[0], "total_s": total}


def _median_row(samples: list[dict]) -> dict:
    return {
        "samples": samples,
        "median_time_to_first_tool_s": median([item["time_to_first_tool_s"] for item in samples]),
        "median_total_s": median([item["total_s"] for item in samples]),
    }


def main() -> int:
    # Touch the adapter once so a shape mismatch fails before the timed loop.
    texts = list(iter_openai_chat_chunks(_openai_chunks()))
    if "".join(texts) != SOURCE:
        raise SystemExit("openai adapter did not rebuild the script")
    anthropic = [
        type("Event", (), {"type": "content_block_delta", "delta": type("Delta", (), {"text": piece})()})()
        for piece in _pieces()
    ]
    if "".join(iter_anthropic_events(anthropic)) != SOURCE:
        raise SystemExit("anthropic adapter did not rebuild the script")
    streaming = [run_streaming() for _ in range(REPEATS)]
    batch = [run_after_complete() for _ in range(REPEATS)]
    payload = {
        "kind": "stream",
        "measured": True,
        "gap_s": GAP_S,
        "tool_sleep_s": TOOL_S,
        "chunk_chars": CHUNK,
        "repeats": REPEATS,
        "machine": machine_info(),
        "notes": {
            "streaming": "Tokens are OpenAI-style chunks fed through Session.push as they arrive.",
            "run_after_complete": "The same gaps elapse first. Engine.run starts only after the full script exists.",
        },
        "streaming": _median_row(streaming),
        "run_after_complete": _median_row(batch),
    }
    out_dir = ROOT / "bench" / "results"
    out_dir.mkdir(parents=True, exist_ok=True)
    text = json.dumps(payload, indent=2) + "\n"
    stamp = utc_stamp()
    path = out_dir / f"stream-{stamp}.json"
    path.write_text(text)
    (out_dir / "stream-latest.json").write_text(text)
    print(f"wrote {path.relative_to(ROOT)}")
    print(
        "streaming median_ttft_s="
        f"{payload['streaming']['median_time_to_first_tool_s']:.6f} "
        f"median_total_s={payload['streaming']['median_total_s']:.6f}"
    )
    print(
        "run_after_complete median_ttft_s="
        f"{payload['run_after_complete']['median_time_to_first_tool_s']:.6f} "
        f"median_total_s={payload['run_after_complete']['median_total_s']:.6f}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
