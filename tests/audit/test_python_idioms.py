"""Round-6 Python idioms vs independent CPython execution. No model or network.

Covers what models write in conversational tool use (tau-bench train traces):
dict iteration, `except Exception` / builtin exception classes, `raise` and
`assert`, `isinstance`, `round`, format specs, `str.format`, `%` formatting,
`json.dumps`, `d.update` / `d.pop` / `xs.pop` statements, and Python's
floor-division/modulo signs. Same contract as test_python_sugar: batch,
streamed and replayed runs must match CPython's output, ordered writes and
bindings, and a program CPython rejects must fail in Weave after the same
writes. Seeds are fixed.
"""
import contextlib
import io
import json
import random
import sys
import os

sys.path.insert(0, os.path.dirname(__file__))
import test_python_sugar as base  # noqa: E402


def run_cpython(source):
    store = dict(base.STORE0)
    writes = []

    def get(k):
        return store.get(k, 0)

    def put(k, v):
        writes.append([k, v])
        old = store.get(k, 0)
        store[k] = v
        return old

    def fail():
        raise base.ToolError("expected")

    hidden = ("get", "put", "fail", "ToolError", "json")
    scope = {"get": get, "put": put, "fail": fail, "ToolError": base.ToolError, "json": json}
    out = io.StringIO()
    error = None
    try:
        with contextlib.redirect_stdout(out):
            exec(source, scope)
    except Exception as exc:
        error = exc
    bindings = {
        k: v for k, v in scope.items()
        if not k.startswith("__") and k not in hidden and not callable(v) and not isinstance(v, BaseException)
    }
    return out.getvalue(), writes, bindings, error


base.run_cpython = run_cpython
compare = base.compare

FIXTURES = [
    # dict iteration
    "d = {'x': get('a'), 'y': get('b')}\nfor k in d:\n    print(k, d[k])\n",
    "d = {'x': 1, 'y': 2}\nprint(sorted(d), list(d), max(d), min(d), sum(d.values()), any(d), [k for k in d])\n",
    "d = {'b': 2, 'a': 1}\nfor k, v in sorted(d.items()):\n    put(k, v)\nprint(dict(sorted(d.items())) if False else {k: v * 2 for k, v in d.items()})\n",
    "d = {'p': [1, 2], 'q': []}\nfor key in d.keys():\n    print(key, len(d[key]))\nfor v in d.values():\n    print(v)\n",
    "print([list(p) for p in enumerate({'a': 1, 'b': 2})], [list(p) for p in zip({'a': 1}, [9])], sorted('cab'))\n",
    "d = {}\nfor k in d:\n    put(k, 1)\nprint('empty ok')\n",
    # exception classes
    "try:\n    x = {'a': 1}['zz']\nexcept KeyError:\n    x = 'missing'\nprint(x)\n",
    "try:\n    x = [1][5]\nexcept IndexError:\n    x = -1\nprint(x)\n",
    "try:\n    x = [1][5]\nexcept LookupError:\n    x = -2\nprint(x)\n",
    "try:\n    x = 1 / 0\nexcept ZeroDivisionError:\n    x = 'inf'\nprint(x)\n",
    "try:\n    x = 1 // 0\nexcept ArithmeticError:\n    x = 'arith'\nprint(x)\n",
    "try:\n    x = 'a' + 1\nexcept TypeError:\n    x = 'type'\nprint(x)\n",
    "try:\n    x = int('abc')\nexcept ValueError:\n    x = 0\nprint(x)\n",
    "r = 'none'\ntry:\n    fail()\nexcept Exception:\n    r = 'tool caught by Exception'\nprint(r)\n",
    "try:\n    x = {'a': 1}['b']\nexcept (KeyError, IndexError):\n    x = None\nprint(x)\n",
    "r = 0\ntry:\n    fail()\nexcept (ToolError, KeyError):\n    r = 1\nprint(r)\n",
    "try:\n    x = {}['k']\nexcept IndexError:\n    x = 'wrong handler'\nprint(x)\n",  # KeyError escapes: both fail
    "r = 0\ntry:\n    fail()\nexcept KeyError:\n    r = 'wrong'\nprint(r)\n",  # ToolError escapes: both fail
    "try:\n    x = [][0]\nexcept KeyError:\n    x = 'wrong'\nprint(x)\n",  # IndexError escapes
    "out = []\nfor k in ['a', 'zz', 'b']:\n    try:\n        out.append({'a': 1, 'b': 2}[k])\n    except KeyError:\n        out.append(None)\nprint(out)\n",
    "try:\n    put('w', 1)\n    x = {}['k']\n    put('w', 2)\nexcept Exception:\n    put('w', 3)\nprint(get('w'))\n",
    # raise / assert: nothing after them may run, writes included
    "if get('a') > 1:\n    raise ValueError('too big')\nput('z', 1)\n",
    "put('z', 1)\nraise RuntimeError('stop')\n",
    "if get('a') > 100:\n    raise ValueError('never')\nput('z', 2)\nprint('after')\n",
    "r = 0\ntry:\n    raise ValueError('bad input')\nexcept ValueError:\n    r = 'handled'\nprint(r)\n",
    "r = 0\ntry:\n    raise KeyError('k')\nexcept LookupError:\n    r = 'lookup'\nprint(r)\n",
    "r = 0\ntry:\n    raise ValueError('x')\nexcept TypeError:\n    r = 'wrong'\nprint(r)\n",  # escapes
    "def check(n):\n    if n < 0:\n        raise ValueError('negative')\n    return n * 2\nprint(check(2))\ntry:\n    check(-1)\nexcept ValueError:\n    print('neg')\n",
    "assert get('a') == 3, 'a must be 3'\nput('k', 1)\nprint('ok')\n",
    "assert get('a') == 4, 'a must be 4'\nput('k', 1)\n",
    "try:\n    assert False\nexcept AssertionError:\n    print('assert caught')\n",
    # isinstance
    "vals = [1, 2.5, 'x', True, None, [1], {'a': 1}]\nprint([isinstance(v, int) for v in vals], [isinstance(v, (int, float)) for v in vals])\n",
    "vals = [1, 'x', [1], {'a': 1}, None]\nprint([isinstance(v, dict) for v in vals], [isinstance(v, (list, tuple)) for v in vals], [isinstance(v, str) for v in vals])\n",
    "r = {'items': [{'p': 3}]}\nif isinstance(r, dict) and 'items' in r:\n    print(sum(i['p'] for i in r['items']))\n",
    # round
    "print(round(2.5), round(3.5), round(-0.5), round(2.675, 2), round(1.005, 2), round(7), round(1234, -2), round(0.125, 2), round(-1.25, 1))\n",
    "print(round(10.0 / 3, 3), round(99.995, 2), round(5, 1), round(2.5, 0))\n",
    # format specs
    "x = 1234.5678\nprint(f'{x:.2f} {x:,.2f} {x:10.1f}| {x:<10.1f}| {x:^12.3f}| {x:+.0f} {x:e} {x:.3E}')\n",
    "n = 1234567\nprint(f'{n:,} {n:d} {n:>10} {n:<10}| {n:010d} {n:x} {n:X} {n:o} {n:b} {-n:,}')\n",
    "s = 'ab'\nprint(f'[{s:>5}] [{s:<5}] [{s:^6}] [{s:*^7}] [{s:.1}] [{s}]')\n",
    "p = 0.4567\nprint(f'{p:.1%} {p:%} {-p:.2f} {0.0:.2f} {-0.0:.1f} {2:.2f} {True:d}')\n",
    "x = 3.0\nprint(f'{x:} {x:>6} {1e20:} {1e-5:}')\n",
    "price = 19.999\nqty = 3\nprint(f'Total: ${price * qty:,.2f} for {qty:d} items')\n",
    "print(f'{\"x\":5}|{5:5}|{-5:05}|{-5.5:08.2f}|{5: d}|{5:+d}')\n",
    "x = 'abc'\nprint(f'{x:d}')\n",  # CPython ValueError: both fail
    # str.format
    "print('{} + {} = {}'.format(1, 2, 3), '{0}{1}{0}'.format('a', 'b'), '{:.2f}'.format(3.14159), '{!r}'.format('q'), '{{x}}'.format())\n",
    "print('{:>8}|{:<4}|{:^5}|{:,}'.format('id', 7, 'm', 10000))\n",
    # % formatting
    "print('%s has %d items costing %.2f' % ('cart', 3, 9.5), '%5s|%-5s|%05d|%+d|%x' % ('a', 'b', 42, 7, 255))\n",
    "print('%d%%' % 50, '%s' % [1, 2], '%r' % 'x', '%.1f' % 2, '%d' % 3.9, '%i' % -2)\n",
    # json.dumps
    "print(json.dumps({'a': [1, 2.5, None, True], 'b': {'c': 'x\"y'}}), json.dumps('caf\u00e9'), json.dumps([]), json.dumps({1: 2}))\n",
    "print(json.loads('{\"k\": [1, 2]}')['k'])\n",
    # update / pop statements
    "d = {'a': 1}\nd.update({'b': 2, 'a': 9})\nd.update(c=3)\nprint(d)\n",
    "d = {'a': 1, 'b': 2}\nd.pop('a')\nd.pop('zz', None)\nprint(d)\n",
    "xs = [1, 2, 3, 4]\nxs.pop()\nxs.pop(0)\nprint(xs)\n",
    "d = {'a': 1}\nd.pop('zz')\n",  # KeyError: both fail
    # arithmetic signs
    "print(7 // 2, -7 // 2, 7 // -2, -7 // -2, 7 % 3, -7 % 3, 7 % -3, -7 % -3)\n",
    "print(7.5 // 2, -7.5 // 2, 7.5 % 2, -7.5 % 2, 7.5 % -2, -6.0 % 3)\n",
    # a branch or handler that jumps does not leave its names unassigned (round-5 tau-bench trace)
    "out = []\nfor x in [1, None, 2]:\n    try:\n        n = x + 1\n    except:\n        continue\n    out.append(n)\nprint(out)\n",
    "out = []\nfor x in [3, None, 4]:\n    if x is None:\n        continue\n    else:\n        y = x * 2\n    out.append(y)\nprint(out)\n",
    "for x in [3, 0, 4]:\n    if x == 0:\n        break\n    else:\n        y = x\n    put('y', y)\nprint(get('y'))\n",
    "v = get('a')\nif v > 1:\n    w = v\nelse:\n    raise ValueError('small')\nprint(w)\n",
    "def f(x):\n    for i in range(3):\n        if i == x:\n            return i\n        else:\n            k = i\n        put('k', k)\n    return -1\nprint(f(1), f(9))\n",
    # error messages on the wrong shape must still be errors
    "o = 'W123'\nprint(o['items'])\n",
    "o = [{'a': 1}]\nprint(o['a'])\n",
]

SPECS = ["", ".2f", ",.2f", ">8", "<6", "^9", "+.1f", "08.3f", ".0f", "e", ".2e", ".1%", ",", "d", ",d", "010d", "x", "b", " d", "*>7", "-^9"]
VALUES = ["0", "7", "-42", "1234567", "3.14159", "-2.5", "0.005", "1e6", "123456.789", "True", "'txt'", "''"]


def gen_program(rng):
    src = ["d = {'a': get('a'), 'b': get('b'), 'c': get('c')}\n"]
    names = []
    for i in range(rng.randrange(2, 6)):
        name = f"v{i}"
        kind = rng.randrange(9)
        if kind == 0:
            spec, val = rng.choice(SPECS), rng.choice(VALUES)
            line = f"try:\n    {name} = f'{{{val}:{spec}}}'\nexcept (ValueError, TypeError):\n    {name} = 'bad'"
        elif kind == 1:
            line = f"{name} = []\nfor k in d:\n    if d[k] {rng.choice(['>', '<', '!='])} {rng.randrange(8)}:\n        {name}.append(k)"
        elif kind == 2:
            k = rng.choice(["'a'", "'zz'", "'b'"])
            exc = rng.choice(["KeyError", "LookupError", "Exception", "(KeyError, ToolError)"])
            line = f"try:\n    {name} = d[{k}] * 2\nexcept {exc}:\n    {name} = -1"
        elif kind == 3:
            ndig = rng.choice(["", ", 0", ", 1", ", 2", ", -1"])
            val = rng.choice(["2.5", "0.125", "2.675", "-1.5", "1234.5678", "7", "d['a'] / 7"])
            line = f"{name} = round({val}{ndig})"
        elif kind == 4:
            a, b = rng.choice(["7", "-7", "7.5", "-7.5", "d['b']", "0"]), rng.choice(["2", "-2", "3", "-3.0", "d['c']"])
            line = f"{name} = [{a} // {b}, {a} % {b}]"
        elif kind == 5:
            line = f"{name} = {{k: v for k, v in d.items() if isinstance(v, {rng.choice(['int', 'str', '(int, float)', 'dict'])})}}"
        elif kind == 6:
            line = (f"try:\n    if d['a'] {rng.choice(['>', '<'])} {rng.randrange(6)}:\n        raise ValueError('x')\n"
                    f"    put('r{i}', 1)\n    {name} = 'ran'\nexcept {rng.choice(['ValueError', 'Exception'])}:\n    {name} = 'raised'")
        elif kind == 7:
            line = f"{name} = '%{rng.choice(['s', 'd', '.2f', '5d', '-4s', '05.1f'])}' % {rng.choice(['3', '2.75', '-1'])}"
        else:
            line = f"{name} = json.dumps({{k: [d[k], str(d[k])] for k in sorted(d, reverse={rng.choice(['True', 'False'])})}})"
        src.append(line + "\n")
        names.append(name)
    src.append(f"print({', '.join(names)})\n")
    return "".join(src)


def main():
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 600
    for source in FIXTURES:
        compare(source)
    for seed in range(n):
        compare(gen_program(random.Random(10_000 + seed)))
    total = len(FIXTURES) + n
    print(f"{total} CPython differential idiom programs; {2 * total} batch/stream runs and replays passed")


if __name__ == "__main__":
    main()
