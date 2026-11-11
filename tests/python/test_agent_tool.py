"""Tests for weave.WeaveTool, the single run_weave tool adapter. No network."""

from __future__ import annotations

import asyncio
import json
import time
from types import SimpleNamespace

import weave


@weave.declare(effect="read", arity=1)
def get_user(user_id):
    """Load a user record."""
    time.sleep(0.2)
    if user_id == "missing":
        raise weave.ToolError("no such user")
    return {"id": user_id, "name": user_id.upper()}


@weave.declare(effect="irreversible", arity=2)
def send(user_id, text):
    """Send a message."""
    SENT.append((user_id, text))
    return "sent"


SENT: list = []


def make():
    return weave.WeaveTool([get_user, send])


def test_definitions_describe_functions_and_rules():
    tool = make()
    chat = tool.chat_tool()
    assert chat["function"]["name"] == "run_weave"
    assert chat["function"]["parameters"]["required"] == ["source"]
    assert "get_user(user_id), effect read. Load a user record." in tool.description
    assert "send(user_id, text), effect irreversible." in tool.description
    assert "ToolError" in tool.description
    resp = tool.responses_tool()
    assert resp["name"] == "run_weave" and resp["strict"] is True


def test_chat_tool_call_runs_in_parallel():
    tool = make()
    source = 'a = get_user("u1")\nb = get_user("u2")\nprint(a["name"], b["name"])\n'
    call = {"id": "call_1", "type": "function", "function": {"name": "run_weave", "arguments": json.dumps({"source": source})}}
    started = time.perf_counter()
    message = tool.handle(call)
    elapsed = time.perf_counter() - started
    assert message == {"role": "tool", "tool_call_id": "call_1", "content": "U1 U2\n"}
    assert elapsed < 0.38, elapsed  # two 0.2 s calls overlap


def test_sdk_objects_and_responses_items():
    tool = make()
    args = json.dumps({"source": 'print(get_user("x")["id"])'})
    sdk_call = SimpleNamespace(id="c2", function=SimpleNamespace(name="run_weave", arguments=args))
    assert tool.handle(sdk_call)["content"] == "x\n"
    item = {"type": "function_call", "call_id": "fc_1", "name": "run_weave", "arguments": args}
    assert tool.handle(item) == {"type": "function_call_output", "call_id": "fc_1", "output": "x\n"}


def test_errors_come_back_as_repairable_text():
    tool = make()
    assert tool.run("import os\n").startswith("error: the script did not pass the static check")
    out = tool.run('print(get_user("missing"))\n')
    assert out.startswith("error:") and "no such user" in out, out
    caught = tool.run('try:\n    u = get_user("missing")\nexcept ToolError:\n    u = None\nprint(u)\n')
    assert caught == "None\n"
    assert tool.handle({"type": "function_call", "call_id": "c", "arguments": "not json"})["output"].startswith("error:")
    assert "printed nothing" in tool.run('x = get_user("a")\n')


def test_irreversible_calls_run_after_reads():
    SENT.clear()
    out = make().run('u = get_user("u9")\nsend(u["id"], "hi " + u["name"])\nprint("ok")\n')
    assert out == "ok\n" and SENT == [("u9", "hi U9")]


def test_agents_sdk_tool_when_installed():
    try:
        import agents  # noqa: F401
    except ImportError:
        print("skip: openai-agents not installed")
        return
    fn_tool = make().agents_tool()
    assert fn_tool.name == "run_weave"
    out = asyncio.run(fn_tool.on_invoke_tool(None, json.dumps({"source": 'print(get_user("q")["name"])'})))
    assert out == "Q\n"


def test_listing_shows_return_types_and_bad_calls_are_explained():
    import time
    from weave import WeaveTool, declare

    @declare(effect="read")
    def fs_read(*, path):
        "Read a file."
        return {"value": "blue"}

    fs_read.__weave_returns__ = {"type": "object", "properties": {"value": {"type": "string"}}, "required": ["value"]}

    @declare(effect="read")
    def score(name: str) -> int:
        "Score a name."
        time.sleep(0.1)
        return len(name)

    tool = WeaveTool([fs_read, score])
    assert 'fs_read(path) -> {"value": str}, effect read.' in tool.description
    assert "score(name: str) -> int, effect read." in tool.description
    out = tool.run('t = fs_read(path="a")\nprint("got " + t)')
    assert "cannot add str and dict with keys 'value'" in out, out
    out = tool.run('t = fs_read("a")')
    assert "bad call" in out and "fs_read(path=...)" in out and "single character" not in out, out
    # An accumulator loop still overlaps its reads (loop prefetch).
    started = time.monotonic()
    out = tool.run("total = 0\nfor n in ['ab', 'cde', 'f', 'ghij', 'k', 'lm']:\n    total += score(n)\nprint(total)")
    assert out == "13\n", out
    assert time.monotonic() - started < 0.45



def test_version_matches_the_package():
    import weave
    from weave.cli import dispatch

    assert weave.__version__ == "1.0.0"
    assert dispatch(["--version"]) == "weave 1.0.0"


def test_a_final_bare_expression_is_printed():
    from weave import WeaveTool, declare
    from weave.agent_tool import echo_last_expression

    @declare(effect="read")
    def get_customer(cid: str) -> dict:
        "Look up a customer."
        return {"id": cid, "name": "Ada"}

    tool = WeaveTool([get_customer])
    assert tool.run("get_customer('C-1')") == "{'id': 'C-1', 'name': 'Ada'}\n"
    assert tool.run("c = get_customer('C-1')\nc['name']") == "Ada\n"
    assert tool.run("print('x')\nget_customer('C-1')['name']") == "x\nAda\n"
    assert "printed nothing" in tool.run("c = get_customer('C-1')")
    assert echo_last_expression("x = 1\nprint(x)\n") == "x = 1\nprint(x)\n"
    assert echo_last_expression("x = 1; x\n") == "x = 1; x\n"
    assert echo_last_expression("for i in [1]:\n    i\n") == "for i in [1]:\n    i\n"


if __name__ == "__main__":
    for name, fn in list(globals().items()):
        if name.startswith("test_"):
            fn()
            print("ok", name)
