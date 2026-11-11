"""Comprehensions, generator expressions, `key=` lambdas, map/filter, sets and
`def` helpers vs independent CPython execution. No model or network.

Every program runs in CPython and in Weave three ways (batch, streamed one
line at a time, and replayed from the batch trace). Printed output, the
ordered list of writes, and every top-level binding must match exactly; a
program CPython rejects at runtime must fail in Weave too, after the same
writes. Seeds are fixed, so a failure reproduces from its seed alone.
"""
import contextlib
import io
import random
import sys

import weave


class ToolError(Exception):
    pass


STORE0 = {"a": 3, "b": 7, "c": 1}


def run_cpython(source):
    store = dict(STORE0)
    writes = []

    def get(k):
        return store.get(k, 0)

    def put(k, v):
        writes.append([k, v])
        old = store.get(k, 0)
        store[k] = v
        return old

    def fail():
        raise ToolError("expected")

    scope = {"get": get, "put": put, "fail": fail, "ToolError": ToolError}
    out = io.StringIO()
    error = None
    try:
        with contextlib.redirect_stdout(out):
            exec(source, scope)
    except Exception as exc:  # the program's own runtime error
        error = exc
    bindings = {
        k: v for k, v in scope.items()
        if not k.startswith("__") and k not in ("get", "put", "fail", "ToolError") and not callable(v)
    }
    return out.getvalue(), writes, bindings, error


def engine(writes):
    store = dict(STORE0)
    e = weave.Engine()

    def put(k, v):
        writes.append([k, v])
        old = store.get(k, 0)
        store[k] = v
        return old

    def fail():
        raise ToolError("expected")

    e.tool("get", lambda k: store.get(k, 0), effect="read")
    e.tool("put", put, effect="irreversible")
    e.tool("fail", fail, effect="read")
    return e


def normal(v):
    # CPython tuples print as tuples and Weave has none; compare as lists.
    if isinstance(v, (list, tuple)):
        return [normal(x) for x in v]
    if isinstance(v, dict):
        return {k: normal(x) for k, x in v.items()}
    return v


def compare(source):
    expected_out, expected_writes, expected_bindings, error = run_cpython(source)
    results = []
    for streamed in (False, True):
        writes = []
        e = engine(writes)
        try:
            if streamed:
                session = e.session()
                for line in source.splitlines(keepends=True):
                    session.push(line)
                result = session.finish()
            else:
                result = e.run(source)
        except Exception as exc:
            if error is None:
                raise AssertionError(f"Weave failed where CPython did not:\n{source}\n{exc}") from exc
            assert writes == expected_writes, (source, expected_writes, writes)
            results.append(None)
            continue
        assert error is None, f"CPython raised {error!r} but Weave succeeded:\n{source}"
        assert result.stdout == expected_out, (source, expected_out, result.stdout)
        assert writes == expected_writes, (source, expected_writes, writes)
        got = {k: normal(v) for k, v in result.bindings.items()}
        want = normal(expected_bindings)
        for k, v in expected_bindings.items():
            # Weave has no set type: a set is a duplicate-free list (documented).
            if isinstance(v, (set, frozenset)) and isinstance(got.get(k), list):
                assert len(got[k]) == len(v) and set(got[k]) == v, (source, k, v, got[k])
                got[k] = want[k] = None
        assert got == want, (source, expected_bindings, result.bindings)
        results.append((e, result))
    if results[0] is not None:
        e, result = results[0]
        replay_writes = []
        r = engine(replay_writes).replay(source, result.trace_json())
        assert r.stdout == result.stdout and r.bindings == result.bindings, source
        assert replay_writes == [], "replay invoked a tool"


FIXTURES = [
    # Seen live (GPT-4.1, bench v5 real services): key= lambdas, comprehensions, helpers.
    "rows = [{'n': 'a', 'v': get('a')}, {'n': 'b', 'v': get('b')}, {'n': 'c', 'v': get('b')}]\n"
    "best = max(rows, key=lambda r: r['v'])\nprint(best['n'])\n"
    "order = [r['n'] for r in sorted(rows, key=lambda r: r['v'], reverse=True)]\nprint(order)\n",
    "def score(k):\n    v = get(k)\n    if v > 5:\n        return v * 2\n    return v\n"
    "totals = {k: score(k) for k in ['a', 'b', 'c']}\nprint(totals, sum(totals.values()))\n",
    "xs = [1, 5, 2, 8]\nhit = any(put('w', x) > 4 for x in xs)\nprint(hit, get('w'))\n",
    "xs = [9, 1, 9]\nok = all(put('w', x) > 2 for x in xs)\nprint(ok, get('w'))\n",
    "def first(xs, limit=3):\n    for x in xs:\n        for y in xs:\n            if x + y > limit:\n                return [x, y]\n    return None\n"
    "print(first([1, 2, 3]), first([0, 1], limit=10), first(xs=[5], limit=1))\n",
    "names = ['b', 'A', 'c']\nprint(sorted(names, key=str.lower), max(names, key=len), min([], default='none'))\n",
    "seen = set()\nfor x in [3, 1, 3, 2, 1]:\n    seen.add(x)\nprint(sorted(seen), len(seen), 2 in seen)\n",
    "print(len({x % 3 for x in range(10)}), sorted({get(k) for k in ['a', 'b', 'a']}))\n",
    "xs = [5, 3, 9]\nxs.sort(key=lambda v: -v)\nprint(xs)\nxs.sort()\nprint(xs)\nxs.reverse()\nprint(xs)\n",
    "print(list(map(str, range(3))), list(filter(None, [0, 1, 2])), list(filter(lambda v: v > get('a'), [2, 4, 6])))\n",
    "m = [[1, 2], [3, 4]]\nflat = [y for row in m for y in row if y != 2]\nprint(flat, [[y * 10 for y in row] for row in m])\n",
    "x = 5\nys = [x * 2 for x in range(3)]\nprint(x, ys)\n",
    "data = [3, 0, 2]\nr = [put('k', d) for d in data]\nprint(r, get('k'))\n",
    "def safe(k):\n    try:\n        fail()\n    except ToolError:\n        return -1\n    return get(k)\nprint([safe(k) for k in 'ab'])\n",
    "def area(w, h=2):\n    return w * h\nprint(area(3), area(3, 4), area(h=5, w=1))\n",
    "xs = []\nprint(xs and max(x for x in xs), max((x for x in xs), default=-1), sum(x for x in xs))\n",
    "vals = [get(k) for k in ['a', 'b']]\ns = f\"total={sum(v for v in vals)} top={max(vals)}\"\nprint(s)\n",
    "y = [1, 2] if get('a') > 9 else [v * v for v in range(4)]\nprint(y)\n",
    "def pick(xs):\n    best = None\n    for x in xs:\n        if best is None or x > best:\n            best = x\n    return best\nprint(pick([4, 9, 2]), pick([]))\n",
    "pairs = [(k, get(k)) for k in ['a', 'b', 'c']]\nprint(sorted(pairs, key=lambda p: p[1])[0][0])\n",
    "counts = {'x': 2, 'y': 5}\nprint(max(counts, key=counts.get), sorted(counts, key=lambda k: counts[k], reverse=True))\n",
    "def inc(n):\n    return n + 1\nprint(sorted([3, 1, 2], key=inc), list(map(inc, [1, 2])))\n",
    "total = 0\nfor k in ['a', 'b']:\n    total += sum(put(k, i) for i in range(2))\nprint(total)\n",
    "print(max([], key=lambda v: v))\n",  # CPython raises: Weave must fail too
    "def g(x):\n    return x * 2\ndef f(x):\n    return g(x) + 1\nprint(f(3), [f(i) for i in range(3)])\n",
]


def gen_program(rng):
    src = ["def h(a, b=1):\n    if a > b:\n        return put('h', a - b)\n    for j in range(3):\n        if get('a') + j > a:\n            return j\n    return b\n"]
    pool = ["[1, 4, 2]", "range(4)", "[get('a'), get('b')]", "[]", "[7]"]

    def ex(v):
        return rng.choice([f"{v} * 2", f"{v} + get('c')", f"put('w', {v})", f"h({v})", f"{v} % 3", f"-{v}", f"h({v}, b={v})"])

    def cond(v):
        return rng.choice([f"{v} > 1", f"{v} % 2 == 0", f"get('b') > {v}", "True"])

    names = []
    for i in range(rng.randrange(2, 6)):
        src_list = rng.choice(pool)
        kind = rng.randrange(12)
        name = f"v{i}"
        if kind == 0:
            line = f"{name} = [{ex('x')} for x in {src_list} if {cond('x')}]"
        elif kind == 1:
            line = f"{name} = {{x: {ex('x')} for x in {src_list}}}"
        elif kind == 2:
            line = f"{name} = sorted({src_list}, key=lambda x: {ex('x')}, reverse={rng.choice(['True', 'False'])})"
        elif kind == 3:
            fn = rng.choice(["max", "min"])
            line = f"{name} = {fn}({src_list}, key=lambda x: {ex('x')}, default=-1)"
        elif kind == 4:
            fn = rng.choice(["any", "all"])
            line = f"{name} = {fn}({ex('x')} > 2 for x in {src_list})"
        elif kind == 5:
            line = f"{name} = sum({ex('x')} for x in {src_list} if {cond('x')})"
        elif kind == 6:
            line = f"{name} = h({ex('3')}) + len([y for y in {src_list}])"
        elif kind == 7:
            line = f"{name} = sorted({{{ex('x')} for x in {src_list}}})"
        elif kind == 8:
            line = (f"{name} = 0\nfor x in {src_list}:\n    {name} += h(x)\n    if {name} > 5:\n        break")
        elif kind == 10:
            line = (f"try:\n    {name} = [{ex('x')} if x != 2 else fail() for x in {src_list}]\n"
                    f"except ToolError:\n    {name} = 'caught'")
        elif kind == 11:
            line = f"{name} = get('a') > 2 and any({cond('x')} for x in {src_list}) or [put('o', x) for x in {src_list}]"
        else:
            line = f"{name} = [[{ex('y')} for y in range(2)] for x in {src_list}] if {cond('get(\"a\")')} else None"
        src.append(line + "\n")
        names.append(name)
    src.append(f"print({', '.join(names)})\n")
    return "".join(src)


def main():
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 600
    for source in FIXTURES:
        compare(source)
    for seed in range(n):
        compare(gen_program(random.Random(seed)))
    total = len(FIXTURES) + n
    print(f"{total} CPython differential sugar programs; {2 * total} batch/stream runs and replays passed")


if __name__ == "__main__":
    main()
