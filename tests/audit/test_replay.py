"""Exact replay identity, streaming, loops, lazy expressions and errors."""
import threading
import time
import weave

for source in [
    "a = read()\nb = read()\nprint(a, b)\n",
    'for i in [1, 2, 3]:\n    x = read()\n    print(x)\n',
    'for i in [1, 2]:\n    for j in [1, 2]:\n        print(read() if i else 0)\n',
    'for i in [1, 2]:\n    print(True and read())\n',
    'try:\n    x = failing()\nexcept ToolError:\n    print(read())\n',
]:
    for streamed in [False, True]:
        for _ in range(30):
            lock = threading.Lock()
            calls = [0]
            def read():
                with lock:
                    calls[0] += 1
                    n = calls[0]
                time.sleep(.005 if n % 2 else .001)
                return n
            def failing():
                raise RuntimeError("expected")
            e = weave.Engine()
            e.tool("read", read, effect="read")
            e.tool("failing", failing, effect="read")
            if streamed:
                s = e.session()
                for chunk in source.splitlines(keepends=True):
                    s.push(chunk)
                r = s.finish()
            else:
                r = e.run(source)
            before = calls[0]
            for _ in range(3):
                replay = e.replay(source, r.trace_json())
                assert replay.stdout == r.stdout, (source, r.stdout, replay.stdout)
                assert replay.bindings == r.bindings
            assert calls[0] == before
            try:
                e.replay(source + 'print("changed")\n', r.trace_json())
            except ValueError:
                pass
            else:
                raise AssertionError("changed source accepted")
print("300 batch/streamed recordings, 900 exact replays passed")

# Replay and speculative hits must use the same value-ingress checks.
import json
for n in (63, 64, 65):
    e=weave.Engine(); e.tool("large",lambda n=n:"a"*n,effect="read")
    trace=e.run("x=large()\n").trace_json()
    e.set_limits(max_value_size=64)
    if n <=64:
        assert len(e.replay("x=large()\n",trace).bindings["x"])==n
    else:
        try:e.replay("x=large()\n",trace)
        except ValueError:pass
        else:raise AssertionError("oversized replay accepted")
    cache=e.speculate([("large",[])])
    time.sleep(.01)
    try:out=e.run("x=large()\n",speculation=cache)
    except ValueError:assert n>64
    else:assert n<=64
print("replay and speculative 63/64/65-byte boundaries passed")
