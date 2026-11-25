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
    # Caught by the static check from the signature, before any tool runs.
    out = tool.run('t = fs_read("a")')
    assert "did not pass the static check" in out and "takes no positional arguments" in out, out
    assert "fs_read(path=...)" in out and "single character" not in out, out
    # An accumulator loop still overlaps its reads (loop prefetch).
    started = time.monotonic()
    out = tool.run("total = 0\nfor n in ['ab', 'cde', 'f', 'ghij', 'k', 'lm']:\n    total += score(n)\nprint(total)")
    assert out == "13\n", out
    assert time.monotonic() - started < 0.45



def test_version_matches_the_package():
    import weave
    from weave.cli import dispatch

    assert weave.__version__ == "1.1.2"
    assert dispatch(["--version"]) == "weave 1.1.2"


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


def test_shape_of_samples_keeps_per_record_keys_generic():
    from weave.agent_tool import shape_of, shape_of_samples

    products = [
        {"name": "Backpack", "variants": {"111": {"options": {"color": "green", "compartment": "camera"}, "price": 9}}},
        {"name": "Lamp", "variants": {"222": {"options": {"color": "red", "bulb": "LED"}, "price": 2.5}}},
    ]
    shape = shape_of_samples(products)
    assert shape == '{"name": str, "variants": dict[str, {"options": dict[str, str], "price": float}]}', shape
    assert shape_of({"id": "W1", "items": [{"p": 1}], "note": None}) == '{"id": str, "items": list[{"p": int}], "note": None}'
    pay = shape_of({"gift_card_1": {"source": "gift_card", "balance": 5}, "credit_card_2": {"source": "credit_card", "last_four": "1"}})
    assert pay == 'dict[str, {"source": str, "balance": int?, "last_four": str?}]', pay


def test_persist_variables_carries_values_between_scripts():
    calls = []

    @weave.declare(effect="read")
    def fetch(k: str) -> dict:
        calls.append(k)
        return {"k": k, "n": [1, 2.5, None, True], "s": "it's \"q\"\n"}

    tool = weave.WeaveTool([fetch], persist_variables=True)
    assert "Variables persist between scripts" in tool.description
    assert tool.run('o = fetch("a")\ncount = 2\nprint(count)') == "2\n"
    assert tool.run('print(o["n"], o["s"] == fetch("a")["s"], count + 1)') == "[1, 2.5, None, True] True 3\n"
    assert calls == ["a", "a"]
    # Error positions refer to the model's script, not the hidden prelude.
    out = tool.run('x = o["k"]\nprint(o["missing"])')
    assert out.startswith("error: L2:"), out
    # Without the option nothing carries over.
    plain = weave.WeaveTool([fetch])
    plain.run('o = fetch("b")')
    assert "used before assignment" in plain.run("print(o)")


def test_conversational_rules_are_opt_in():
    @weave.declare(effect="read")
    def look(k: str) -> str:
        return k

    assert "A person is waiting" not in weave.WeaveTool([look]).description
    assert "A person is waiting" in weave.WeaveTool([look], conversational=True).description


def test_compact_listing_factors_shared_sentences():
    """Round 9: compact=True prints a sentence repeated in 3+ docs once, as a lettered note."""
    shared = "This tool belongs to the vehicle control system, which lets users drive."

    def mk(name):
        def f(x: str) -> str:
            return x
        f.__name__ = name
        f.__doc__ = f"Do {name}. {shared}"
        return weave.declare(effect="read")(f)

    fns = [mk("a"), mk("b"), mk("c")]
    full, small = weave.WeaveTool(fns), weave.WeaveTool(fns, compact=True)
    assert full.description.count(shared) == 3
    assert small.description.count(shared) == 1 and "[A]" in small.description
    assert len(small.description) < len(full.description)
    assert small.run('print(a("x") + b("y"))') == "xy\n"


def test_reply_ends_the_turn_with_its_text():
    @weave.declare(effect="read")
    def look(k: str) -> str:
        return k.upper()

    tool = weave.WeaveTool([look], reply=True)
    assert "reply(" in tool.description
    assert tool.run('reply("got " + look("a"))') == "(replied)"
    assert tool.take_reply() == "got A"
    out = tool.run('print(look("b"))')
    assert out == "B\n" and tool.take_reply() is None
    assert "reply(" not in weave.WeaveTool([look]).description


def test_listing_false_omits_function_lines():
    """With compact=True, listing=False leaves the function list to native schemas the host sends."""
    @weave.declare(effect="read")
    def very_specific_name(k: str) -> str:
        return k

    assert "very_specific_name" in weave.WeaveTool([very_specific_name]).description
    hybrid = weave.WeaveTool([very_specific_name], compact=True, listing=False)
    assert "very_specific_name(" not in hybrid.description
    assert hybrid.run('print(very_specific_name("z"))') == "z\n"


def _gate_tools():
    log = []

    @weave.declare(effect="read")
    def ls() -> list:
        return ["a.txt", "report_2026.txt"]

    @weave.declare(effect="reversible")
    def rm(name: str) -> dict:
        log.append(name)
        return {"ok": True}

    @weave.declare(effect="reversible")
    def cd(d: str) -> dict:
        return {"error": "no such dir"} if d == "nope" else {"cwd": d}

    return log, [ls, rm, cd]


def test_write_gate_pauses_writes_computed_from_unseen_results():
    log, fns = _gate_tools()
    tool = weave.WeaveTool(fns, write_gate="flow")
    assert "paused" in tool.description
    tool.run('rm("a.txt")')
    assert log == ["a.txt"]  # a literal argument is never paused
    out = tool.run('for f in ls():\n    if "report" in f:\n        rm(f)\n        rm("a.txt")')
    assert out.startswith("[paused before rm('report_2026.txt')"), out
    assert 'ls() -> ["a.txt", "report_2026.txt"]' in out
    assert log == ["a.txt"]  # the held write and every later write did not run
    # The model has now seen the listing: the same data-dependent write goes through.
    tool.run('x = ls()\nrm(x[1])')
    assert log == ["a.txt", "report_2026.txt"]


def test_write_gate_pauses_after_a_failed_call_and_counts_user_text():
    log, fns = _gate_tools()
    tool = weave.WeaveTool(fns, write_gate="flow")
    out = tool.run('try:\n    cd("nope")\nexcept Exception:\n    pass\nrm("a.txt")')
    assert "it follows cd('nope'), which failed" in out and log == []
    tool2 = weave.WeaveTool(_gate_tools()[1], write_gate="flow")
    tool2.note_shown("please delete report_2026.txt")
    assert not tool2.run('rm(ls()[1])').startswith("[paused")
    with_reads = weave.WeaveTool(_gate_tools()[1], write_gate="flow", gate_read_names=["rm"])
    assert not with_reads.run('rm(ls()[1])').startswith("[paused")  # rm treated as a read: never gated


def test_write_gate_is_opt_in_and_replays():
    log, fns = _gate_tools()
    assert not weave.WeaveTool(fns).run('rm(ls()[1])').startswith("[paused")
    assert log == ["report_2026.txt"]
    tool = weave.WeaveTool(fns, write_gate="flow")
    src = 'x = ls()\ntry:\n    rm(x[1])\nexcept ToolError:\n    print("held")\n'
    result = tool.engine.run(src)
    assert result.stdout == "held\n" and log == ["report_2026.txt"]
    replayed = tool.engine.replay(src, result.trace_json())  # the refusal is recorded like any tool error
    assert replayed.stdout == result.stdout and log == ["report_2026.txt"]


def test_write_gate_choice_mode_pauses_only_picks_from_lists():
    @weave.declare(effect="read")
    def price(symbol: str) -> dict:
        return {"price": 227.16}

    @weave.declare(effect="reversible")
    def buy(p: float) -> dict:
        return {"ok": True}

    log, fns = _gate_tools()
    tool = weave.WeaveTool(fns + [price, buy], write_gate="choice")
    assert tool.run('print(buy(price("A")["price"]))') == "{'ok': True}\n"  # a single looked-up value passes
    out = tool.run('rm(ls()[1])')
    assert "was picked from a list you have not seen" in out and log == []
    assert "The request is not finished" in out


if __name__ == "__main__":
    for name, fn in list(globals().items()):
        if name.startswith("test_"):
            fn()
            print("ok", name)


def test_request_options_ask_for_one_script_per_turn():
    """Round 4: GPT-4.1 sent two parallel run_weave calls and then ran away to max_output_tokens
    (bench/results/v7/diag-turn1). The tool recommends parallel_tool_calls off and says why."""
    tool = make()
    assert tool.request_options == {"parallel_tool_calls": False}
    assert "once per turn" in tool.description
    assert "do not share variables" in tool.description
