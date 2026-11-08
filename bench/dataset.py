"""Tasks for the real-model eval. Tool results are pure functions of the arguments."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class ToolDef:
    name: str
    params: tuple[str, ...]
    effect: str
    doc: str


@dataclass(frozen=True)
class EvalTask:
    id: str
    instruction: str
    tools: tuple[ToolDef, ...]
    expected: str


def _search(query: str) -> str:
    return f"hit:{query}"


def _summarize(a: str, b: str, c: str, d: str) -> str:
    return "|".join((a, b, c, d))


def _geo(city: str) -> str:
    return f"geo:{city}"


def _weather(place: str) -> str:
    return f"wx:{place}"


def _lookup_user(user_id: str) -> str:
    return f"user:{user_id}"


def _lookup_policy() -> str:
    return "policy:standard"


def _draft(user: str, policy: str) -> str:
    return f"{user}/{policy}"


IMPLEMENTATIONS = {
    "search": _search,
    "summarize": _summarize,
    "geo": _geo,
    "weather": _weather,
    "lookup_user": _lookup_user,
    "lookup_policy": _lookup_policy,
    "draft": _draft,
}


TASKS = (
    EvalTask(
        id="research_four",
        instruction=(
            "Search for q1, q2, q3, and q4 as four separate calls. "
            "Then summarize those four results in that order and print the summary."
        ),
        tools=(
            ToolDef("search", ("query",), "read", "Search one query."),
            ToolDef("summarize", ("a", "b", "c", "d"), "pure", "Join four search hits."),
        ),
        expected="hit:q1|hit:q2|hit:q3|hit:q4",
    ),
    EvalTask(
        id="weather_paris",
        instruction="Geocode Paris, then fetch the weather for that place, and print the weather.",
        tools=(
            ToolDef("geo", ("city",), "read", "Turn a city name into a place id."),
            ToolDef("weather", ("place",), "read", "Weather for a place id."),
        ),
        expected="wx:geo:Paris",
    ),
    EvalTask(
        id="support_draft",
        instruction=(
            "Look up user u1 and the policy at the same time. "
            "Draft a reply from those two results and print it."
        ),
        tools=(
            ToolDef("lookup_user", ("user_id",), "read", "Load a user id."),
            ToolDef("lookup_policy", (), "read", "Load the standard policy."),
            ToolDef("draft", ("user", "policy"), "pure", "Draft a reply from a user and a policy."),
        ),
        expected="user:u1/policy:standard",
    ),
)


def tool_lines(task: EvalTask) -> str:
    lines = []
    for tool in task.tools:
        params = ", ".join(tool.params) if tool.params else ""
        lines.append(f"{tool.name}({params}) -> str, effect {tool.effect}. {tool.doc}")
    return "\n".join(lines)


def call_tool(name: str, args: list) -> str:
    fn = IMPLEMENTATIONS[name]
    return fn(*args)
