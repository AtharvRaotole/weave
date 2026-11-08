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
        raise LookupError("no such user")
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


if __name__ == "__main__":
    for name, fn in list(globals().items()):
        if name.startswith("test_"):
            fn()
            print("ok", name)
