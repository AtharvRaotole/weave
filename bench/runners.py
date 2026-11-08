"""Execution strategies for the same task graph."""

from __future__ import annotations

import asyncio
import time
from dataclasses import dataclass

from bench.harness import Probe, overlap_rounds
from bench.tasks import Node, Task, resolve_args, tool_functions


@dataclass
class Sample:
    wall_s: float
    rounds: int
    tool_calls: int
    correct: bool
    output: str


def run_sequential(task: Task) -> Sample:
    probe = Probe()
    tools = {name: probe.wrap(fn) for name, fn in tool_functions(task).items()}
    started = time.perf_counter()
    done: dict = {}
    for node in task.nodes:
        done[node.id] = tools[node.tool](*resolve_args(node, done))
    wall = time.perf_counter() - started
    output = str(done[task.nodes[-1].id])
    return Sample(wall, overlap_rounds(probe.events), len(probe.events), output == task.expected, output)


def run_dag(task: Task) -> Sample:
    probe = Probe()
    tools = {name: probe.wrap(fn) for name, fn in tool_functions(task).items()}

    async def execute() -> str:
        pending = list(task.nodes)
        done: dict = {}
        while pending:
            ready = [node for node in pending if all(dep in done for dep in node.deps)]
            if not ready:
                raise RuntimeError(f"{task.name} has a cycle or a missing dependency")

            async def one(node: Node) -> None:
                done[node.id] = await asyncio.to_thread(tools[node.tool], *resolve_args(node, done))

            await asyncio.gather(*(one(node) for node in ready))
            pending = [node for node in pending if node.id not in done]
        return str(done[task.nodes[-1].id])

    started = time.perf_counter()
    output = asyncio.run(execute())
    wall = time.perf_counter() - started
    return Sample(wall, overlap_rounds(probe.events), len(probe.events), output == task.expected, output)


def run_gather(task: Task) -> Sample:
    """Hand-written asyncio.gather batches, not a general scheduler.

    The trip plan waits for weather before starting flights and hotels, which
    is a realistic hand grouping and leaves parallelism on the table.
    """
    probe = Probe()
    tools = {name: probe.wrap(fn) for name, fn in tool_functions(task).items()}

    async def research() -> str:
        s1, s2, s3, s4 = await asyncio.gather(
            asyncio.to_thread(tools["search"], "q1"),
            asyncio.to_thread(tools["search"], "q2"),
            asyncio.to_thread(tools["search"], "q3"),
            asyncio.to_thread(tools["search"], "q4"),
        )
        return await asyncio.to_thread(tools["summarize"], s1, s2, s3, s4)

    async def trip() -> str:
        origin, dest = await asyncio.gather(
            asyncio.to_thread(tools["geo"], "NYC"),
            asyncio.to_thread(tools["geo"], "PAR"),
        )
        wx = await asyncio.to_thread(tools["weather"], dest)
        fl, ht = await asyncio.gather(
            asyncio.to_thread(tools["flights"], origin, dest),
            asyncio.to_thread(tools["hotels"], dest),
        )
        return await asyncio.to_thread(tools["plan"], wx, fl, ht)

    async def support() -> str:
        user, policy = await asyncio.gather(
            asyncio.to_thread(tools["lookup_user"], "u1"),
            asyncio.to_thread(tools["lookup_policy"]),
        )
        orders = await asyncio.to_thread(tools["lookup_orders"], user)
        stock = await asyncio.to_thread(tools["lookup_stock"], orders)
        return await asyncio.to_thread(tools["draft"], user, orders, policy, stock)

    chosen = {
        "research_brief": research,
        "trip_plan": trip,
        "support_ticket": support,
    }[task.name]
    started = time.perf_counter()
    output = asyncio.run(chosen())
    wall = time.perf_counter() - started
    return Sample(wall, overlap_rounds(probe.events), len(probe.events), output == task.expected, output)


def run_weave(task: Task) -> Sample:
    from weave import Engine

    probe = Probe()
    raw = tool_functions(task)
    engine = Engine()
    arity = {name: count for name, count, _delay in task.tools}
    for name, fn in raw.items():
        engine.tool(name, probe.wrap(fn), effect="read", arity=arity[name])
    started = time.perf_counter()
    result = engine.run(task.weave_source)
    wall = time.perf_counter() - started
    output = result.stdout.strip("\n")
    return Sample(wall, overlap_rounds(probe.events), len(probe.events), output == task.expected, output)


def run_monty(task: Task) -> Sample | None:
    """Run the task as Python inside a warm Pydantic Monty worker.

    Timing covers `feed_run` only. The worker pool is already started.
    Returns None when pydantic-monty is not installed.
    """
    try:
        from pydantic_monty import ExternalModule, Monty
    except ImportError:
        return None
    probe = Probe()
    raw = {name: probe.wrap(fn) for name, fn in tool_functions(task).items()}
    module = ExternalModule(raw)
    printed: list[str] = []

    def on_print(*parts) -> None:
        # Monty passes the stream name ("stdout") and then the text.
        text_parts = parts[1:] if parts and parts[0] == "stdout" else parts
        printed.append("".join(str(part) for part in text_parts))

    with Monty(min_processes=1, max_processes=1) as pool:
        with pool.checkout(external_modules={"tools": module}) as session:
            started = time.perf_counter()
            session.feed_run(task.monty_source, print_callback=on_print)
            wall = time.perf_counter() - started
    output = "".join(printed).strip()
    return Sample(wall, overlap_rounds(probe.events), len(probe.events), output == task.expected, output)
