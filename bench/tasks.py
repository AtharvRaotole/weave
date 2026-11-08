"""Realistic multi-tool tasks. Tool outputs are pure functions of their arguments."""

from __future__ import annotations

import time
from dataclasses import dataclass


@dataclass(frozen=True)
class Node:
    id: str
    tool: str
    # A string starting with "$" copies that node's output. Anything else is a literal.
    arg_specs: tuple[str, ...]
    deps: tuple[str, ...]


def _pause(latency_s: float) -> None:
    if latency_s > 0:
        time.sleep(latency_s)


def search(query: str, latency_s: float) -> str:
    _pause(latency_s)
    return f"hit:{query}"


def summarize(*parts: str, latency_s: float) -> str:
    _pause(latency_s)
    return "|".join(parts)


def geo(city: str, latency_s: float) -> str:
    _pause(latency_s)
    return f"geo:{city}"


def weather(place: str, latency_s: float) -> str:
    _pause(latency_s)
    return f"wx:{place}"


def flights(origin: str, dest: str, latency_s: float) -> str:
    _pause(latency_s)
    return f"fl:{origin}>{dest}"


def hotels(place: str, latency_s: float) -> str:
    _pause(latency_s)
    return f"ht:{place}"


def plan(wx: str, fl: str, ht: str, latency_s: float) -> str:
    _pause(latency_s)
    return f"{wx}/{fl}/{ht}"


def lookup_user(user_id: str, latency_s: float) -> str:
    _pause(latency_s)
    return f"user:{user_id}"


def lookup_policy(latency_s: float) -> str:
    _pause(latency_s)
    return "policy:standard"


def lookup_orders(user: str, latency_s: float) -> str:
    _pause(latency_s)
    return f"orders:{user}"


def lookup_stock(orders: str, latency_s: float) -> str:
    _pause(latency_s)
    return f"stock:{orders}"


def draft(user: str, orders: str, policy: str, stock: str, latency_s: float) -> str:
    _pause(latency_s)
    return f"{user};{orders};{policy};{stock}"


@dataclass(frozen=True)
class Task:
    name: str
    summary: str
    nodes: tuple[Node, ...]
    weave_source: str
    monty_source: str
    expected: str
    # tool name -> (arity, latency as a multiple of the base latency)
    tools: tuple[tuple[str, int, float], ...]


def research_brief(base_s: float) -> Task:
    nodes = (
        Node("s1", "search", ("q1",), ()),
        Node("s2", "search", ("q2",), ()),
        Node("s3", "search", ("q3",), ()),
        Node("s4", "search", ("q4",), ()),
        Node("summary", "summarize", ("$s1", "$s2", "$s3", "$s4"), ("s1", "s2", "s3", "s4")),
    )
    return Task(
        name="research_brief",
        summary="Fan out four searches, then summarize them.",
        nodes=nodes,
        weave_source=(
            's1 = search("q1")\n'
            's2 = search("q2")\n'
            's3 = search("q3")\n'
            's4 = search("q4")\n'
            "summary = summarize(s1, s2, s3, s4)\n"
            "print(summary)\n"
        ),
        monty_source=(
            "import tools\n"
            's1 = tools.search("q1")\n'
            's2 = tools.search("q2")\n'
            's3 = tools.search("q3")\n'
            's4 = tools.search("q4")\n'
            "summary = tools.summarize(s1, s2, s3, s4)\n"
            "print(summary)\n"
        ),
        expected="hit:q1|hit:q2|hit:q3|hit:q4",
        tools=(
            ("search", 1, base_s),
            ("summarize", 4, base_s * 0.5),
        ),
    )


def trip_plan(base_s: float) -> Task:
    nodes = (
        Node("origin", "geo", ("NYC",), ()),
        Node("dest", "geo", ("PAR",), ()),
        Node("wx", "weather", ("$dest",), ("dest",)),
        Node("fl", "flights", ("$origin", "$dest"), ("origin", "dest")),
        Node("ht", "hotels", ("$dest",), ("dest",)),
        Node("itinerary", "plan", ("$wx", "$fl", "$ht"), ("wx", "fl", "ht")),
    )
    return Task(
        name="trip_plan",
        summary="Geocode two cities, then weather, flights, and hotels, then an itinerary.",
        nodes=nodes,
        weave_source=(
            'origin = geo("NYC")\n'
            'dest = geo("PAR")\n'
            "wx = weather(dest)\n"
            "fl = flights(origin, dest)\n"
            "ht = hotels(dest)\n"
            "itinerary = plan(wx, fl, ht)\n"
            "print(itinerary)\n"
        ),
        monty_source=(
            "import tools\n"
            'origin = tools.geo("NYC")\n'
            'dest = tools.geo("PAR")\n'
            "wx = tools.weather(dest)\n"
            "fl = tools.flights(origin, dest)\n"
            "ht = tools.hotels(dest)\n"
            "itinerary = tools.plan(wx, fl, ht)\n"
            "print(itinerary)\n"
        ),
        expected="wx:geo:PAR/fl:geo:NYC>geo:PAR/ht:geo:PAR",
        tools=(
            ("geo", 1, base_s),
            ("weather", 1, base_s),
            ("flights", 2, base_s),
            ("hotels", 1, base_s),
            ("plan", 3, base_s * 0.5),
        ),
    )


def support_ticket(base_s: float) -> Task:
    nodes = (
        Node("user", "lookup_user", ("u1",), ()),
        Node("policy", "lookup_policy", (), ()),
        Node("orders", "lookup_orders", ("$user",), ("user",)),
        Node("stock", "lookup_stock", ("$orders",), ("orders",)),
        Node(
            "reply",
            "draft",
            ("$user", "$orders", "$policy", "$stock"),
            ("user", "orders", "policy", "stock"),
        ),
    )
    return Task(
        name="support_ticket",
        summary="Look up a user and policy together, then orders, stock, and a draft reply.",
        nodes=nodes,
        weave_source=(
            'user = lookup_user("u1")\n'
            "policy = lookup_policy()\n"
            "orders = lookup_orders(user)\n"
            "stock = lookup_stock(orders)\n"
            "reply = draft(user, orders, policy, stock)\n"
            "print(reply)\n"
        ),
        monty_source=(
            "import tools\n"
            'user = tools.lookup_user("u1")\n'
            "policy = tools.lookup_policy()\n"
            "orders = tools.lookup_orders(user)\n"
            "stock = tools.lookup_stock(orders)\n"
            "reply = tools.draft(user, orders, policy, stock)\n"
            "print(reply)\n"
        ),
        expected="user:u1;orders:user:u1;policy:standard;stock:orders:user:u1",
        tools=(
            ("lookup_user", 1, base_s),
            ("lookup_policy", 0, base_s),
            ("lookup_orders", 1, base_s),
            ("lookup_stock", 1, base_s),
            ("draft", 4, base_s * 0.5),
        ),
    )


def build_tasks(base_s: float) -> list[Task]:
    return [research_brief(base_s), trip_plan(base_s), support_ticket(base_s)]


def tool_functions(task: Task) -> dict:
    latency = {name: delay for name, _arity, delay in task.tools}
    impls = {
        "search": lambda query, latency_s: search(query, latency_s),
        "summarize": lambda *parts, latency_s: summarize(*parts, latency_s=latency_s),
        "geo": lambda city, latency_s: geo(city, latency_s),
        "weather": lambda place, latency_s: weather(place, latency_s),
        "flights": lambda origin, dest, latency_s: flights(origin, dest, latency_s),
        "hotels": lambda place, latency_s: hotels(place, latency_s),
        "plan": lambda wx, fl, ht, latency_s: plan(wx, fl, ht, latency_s),
        "lookup_user": lambda user_id, latency_s: lookup_user(user_id, latency_s),
        "lookup_policy": lambda latency_s: lookup_policy(latency_s),
        "lookup_orders": lambda user, latency_s: lookup_orders(user, latency_s),
        "lookup_stock": lambda orders, latency_s: lookup_stock(orders, latency_s),
        "draft": lambda user, orders, policy, stock, latency_s: draft(
            user, orders, policy, stock, latency_s
        ),
    }
    out = {}
    for name, _arity, delay in task.tools:
        fn = impls[name]

        def bound(fn=fn, delay=delay):
            def call(*args):
                return fn(*args, latency_s=delay)

            return call

        out[name] = bound()
    return out


def resolve_args(node: Node, done: dict) -> list:
    args = []
    for spec in node.arg_specs:
        if spec.startswith("$"):
            args.append(done[spec[1:]])
        else:
            args.append(spec)
    return args
