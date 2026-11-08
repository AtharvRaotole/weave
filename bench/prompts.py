"""System prompt and few-shot examples that teach a model to write Weave."""

from __future__ import annotations

SYSTEM_PROMPT = """You write Weave, a Python subset for tool calls. No imports, no def, no classes, no while, no with, no lambdas, no comprehensions.
Use assignment, calls, if/else, for, try/except, lists, dicts, f-strings, str/list/dict methods, and builtins (str, int, len, range, sum, min, max, sorted, enumerate, zip, any, all, list).
Tool results can be strings, numbers, or lists; loop over a list result with for. Build lists with xs.append(v) or xs = xs + [v].
A tool that fails raises ToolError. If the task says a call may fail or needs a fallback, wrap it: try: ... except ToolError: ...
Print exactly the format the task asks for, nothing else.
Independent calls must be separate statements so they can run together. Do not hide one call inside another if they do not share data.
Tag nothing yourself; the host already tagged the tools. Irreversible tools run only after the script is complete.
On failure you receive one error per line with a hint. Reply with the whole corrected script and no markdown.
Print the final answer.
"""

FEW_SHOTS = (
    {
        "user": "Tools: search(query) -> str, read effect. summarize(a, b) -> str, pure. Search 'cats' and 'dogs', then summarize both and print the summary.",
        "weave": 'cats = search("cats")\ndogs = search("dogs")\nprint(summarize(cats, dogs))\n',
    },
    {
        "user": "Tools: geo(city) -> str. weather(place) -> str. Get the weather for Paris and print it. geo must run before weather.",
        "weave": 'place = geo("Paris")\nprint(weather(place))\n',
    },
    {
        "user": "Tools: list_items(box) -> list. weight(item) -> int, may fail. List the items in box b1, sum the weights of items whose weight lookup succeeds, and print the sum.",
        "weave": 'total = 0\nfor item in list_items("b1"):\n    try:\n        total += weight(item)\n    except ToolError:\n        pass\nprint(total)\n',
    },
    {
        "user": "Tools: fetch(key) -> str. The previous script was:\nx = fetch()\nError: L1:C5: tool `fetch` takes 1 argument(s), got 0 hint: pass 1 positional argument(s) to `fetch`\nFix it. fetch should load 'note'.",
        "weave": 'x = fetch("note")\nprint(x)\n',
    },
)


def system_message() -> dict:
    return {"role": "system", "content": SYSTEM_PROMPT}


def few_shot_messages() -> list[dict]:
    messages = []
    for shot in FEW_SHOTS:
        messages.append({"role": "user", "content": shot["user"]})
        messages.append({"role": "assistant", "content": shot["weave"]})
    return messages


def repair_message(source: str, errors: list[str]) -> str:
    """Ask the model to repair a script using the checker's one-line errors."""
    body = "\n".join(errors) if errors else "the script did not produce the expected output"
    return (
        "The Weave script failed.\n"
        f"Errors:\n{body}\n"
        "Script:\n"
        f"{source}\n"
        "Reply with only the corrected Weave script. No markdown."
    )
