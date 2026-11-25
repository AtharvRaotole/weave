"""Calls to Python tools are checked against each function's signature before
any tool runs. Offline. Run directly."""
import sys

import weave
from weave import WeaveTool, declare


def weather(city, units="c"):
    return f"{city}:{units}"


def make_engine(**kw):
    e = weave.Engine()
    e.tool("weather", weather, effect="read", **kw)
    return e


def test_unknown_keyword_is_caught_with_a_suggestion():
    problems = make_engine().check('t = weather("Paris", unit="f")')
    assert problems == ["L1:C27: tool `weather` has no parameter `unit` hint: did you mean `units`?"], problems


def test_missing_and_extra_arguments():
    assert make_engine().check("t = weather()") == [
        "L1:C5: tool `weather` is missing required argument(s): city hint: pass them, like `weather(city=...)`"
    ]
    problems = make_engine().check('t = weather("a", "c", "x")')
    assert len(problems) == 1 and "takes at most 2 positional argument(s) (city, units), got 3" in problems[0], problems
    problems = make_engine().check('t = weather("a", city="b")')
    assert problems == ["L1:C23: tool `weather` got `city` twice, by position and by name"], problems


def test_good_calls_still_pass_and_run():
    e = make_engine()
    src = 't = weather("Paris")\nu = weather(city="Lima", units="f")\nprint(t, u)'
    assert e.check(src) == []
    assert e.run(src).stdout == "Paris:c Lima:f\n"


def test_positional_only_keyword_only_and_star_args():
    def f(a, /, b, *, c, d=1):
        return [a, b, c, d]

    def g(*args, **kwargs):
        return len(args) + len(kwargs)

    e = weave.Engine()
    e.tool("f", f, effect="read")
    e.tool("g", g, effect="read")
    assert e.check("x = f(1, 2, c=3)\ny = f(1, b=2, c=3, d=4)\nz = g(1, 2, 3, anything=4)") == []
    problems = e.check("x = f(a=1, b=2, c=3)")
    assert any("parameter `a` of tool `f` is positional-only" in p for p in problems), problems
    problems = e.check("x = f(1, 2, 3)")
    assert any("takes at most 2 positional argument(s) (a, b), got 3" in p for p in problems), problems
    assert any("missing required argument(s): c" in p for p in problems), problems


def test_the_injected_cancel_parameter_is_not_a_script_argument():
    def slow(key, cancel):
        return key

    e = weave.Engine()
    e.tool("slow", slow, effect="read")
    assert e.check('x = slow("k")') == []
    assert e.run('print(slow("k"))').stdout == "k\n"


def test_a_bad_call_stops_the_script_before_an_earlier_write():
    written = []

    def save(key, text):
        written.append((key, text))

    e = make_engine()
    e.tool("save", save, effect="irreversible")
    try:
        e.run('save("a", "first")\nt = weather("Paris", unit="f")\nprint(t)')
    except Exception as exc:  # the static check rejects the whole script
        assert "has no parameter `unit`" in str(exc), exc
    else:
        raise AssertionError("the script should not run")
    assert written == [], written


def test_check_args_false_keeps_the_old_runtime_behaviour():
    e = make_engine(check_args=False)
    assert e.check('t = weather("Paris", unit="f")') == []

    @declare(effect="read", check_args=False)
    def lookup(key):
        return key

    tool = WeaveTool([lookup])
    out = tool.run('print(lookup(kee="a"))')
    # The call reaches Python, which rejects it at run time as before.
    assert "static check" not in out and "bad call" in out, out


def test_weave_tool_catches_a_misspelled_keyword_before_running():
    calls = []

    @declare(effect="read")
    def get_order(order_id: str) -> dict:
        "Look up an order."
        calls.append(order_id)
        return {"id": order_id}

    tool = WeaveTool([get_order])
    out = tool.run('o = get_order(orderid="o1")\nprint(o)')
    assert "did not pass the static check" in out and "did you mean `order_id`?" in out, out
    assert calls == []
    assert tool.run('print(get_order(order_id="o1")["id"])') == "o1\n"


def test_write_gate_wrapped_tools_keep_their_signature():
    @declare(effect="irreversible")
    def refund(order_id, amount):
        "Refund an order."
        return "ok"

    tool = WeaveTool([refund], write_gate="errors")
    out = tool.run('refund("o1", amont=5)')
    assert "did you mean `amount`?" in out, out


def test_a_callable_without_a_readable_signature_is_not_checked():
    class Opaque:
        __signature__ = "not a signature"  # inspect.signature raises TypeError

        def __call__(self, *args):
            return len(args)

    e = weave.Engine()
    e.tool("opaque", Opaque(), effect="read")
    assert e.check("x = opaque(1, 2, 3)") == []
    assert e.run("print(opaque(1, 2, 3))").stdout == "3\n"


def test_arity_still_wins_when_both_are_given():
    e = weave.Engine()
    e.tool("weather", weather, effect="read", arity=1)
    problems = e.check("t = weather()")
    assert problems == ["L1:C5: tool `weather` takes 1 argument(s), got 0 hint: pass 1 positional argument(s) to `weather`"], problems


if __name__ == "__main__":
    failed = 0
    for name, fn in list(globals().items()):
        if name.startswith("test_") and callable(fn):
            try:
                fn()
                print("ok", name)
            except Exception as exc:  # noqa: BLE001
                failed += 1
                print("FAIL", name, repr(exc))
    if failed:
        sys.exit(1)
    print("all arg check tests passed")
