"""Deterministic generated exception programs vs independent CPython execution.
No model or network. Exact W13 counterexamples are included, never weakened.
"""
import contextlib
import io
import random
import weave

class ToolError(Exception): pass

def compare(source):
    expected_writes=[]
    def fail(): raise ToolError('expected')
    def pywrite(x): expected_writes.append(x)
    scope={'fail':fail, 'write':pywrite, 'ToolError':ToolError}
    output=io.StringIO()
    with contextlib.redirect_stdout(output): exec(source,scope)
    for streamed in (False,True):
        actual=[]
        e=weave.Engine()
        e.tool('fail',fail,effect='read')
        e.tool('write',lambda x:actual.append(x),effect='irreversible')
        if streamed:
            session=e.session()
            for line in source.splitlines(keepends=True): session.push(line)
            result=session.finish()
        else: result=e.run(source)
        assert result.stdout==output.getvalue(), (source,output.getvalue(),result.stdout)
        assert actual==expected_writes,(source,expected_writes,actual)
        assert result.bindings['x']==scope['x'],source
        replay=e.replay(source,result.trace_json())
        assert replay.stdout==result.stdout and replay.bindings==result.bindings,source
        assert actual==expected_writes,'replay invoked write'

fixtures=[
'x=0\ntry:\n    if True:\n        x=1\n        fail()\nexcept ToolError:\n    print(x)\nprint(x)\n',
'x=0\ntry:\n    for i in [1,2,3]:\n        x=i\n        if i==2:\n            fail()\nexcept ToolError:\n    print(x)\nprint(x)\n',
'x=0\ntry:\n    try:\n        x=1\n        fail()\n    except ToolError:\n        x=2\n        fail()\nexcept ToolError:\n    print(x)\nprint(x)\n',
'x=0\nfor i in [1,2,3]:\n    try:\n        if i==2:\n            continue\n        x+=i\n        if i==3:\n            break\n    except:\n        x=99\nprint(x)\n',
]
for source in fixtures: compare(source)
for seed in range(500):
    rng=random.Random(seed)
    stop=rng.randrange(1,5)
    value=rng.randrange(1,100)
    mode=rng.randrange(3)
    if mode==0:
        middle=f'if i=={stop}:\n            x={value}\n            fail()\n        x+=1'
    elif mode==1:
        middle=f'try:\n            x+=i\n            if i=={stop}:\n                fail()\n        except ToolError:\n            x+={value}\n            fail()'
    else:
        middle=f'for j in [1,2]:\n            x+=j\n            if i=={stop}:\n                fail()'
    source=f'x=0\ntry:\n    for i in [1,2,3,4]:\n        {middle}\n        write(x)\nexcept ToolError:\n    print(x)\nprint(x)\n'
    compare(source)
print('504 CPython differential exception programs; 1008 batch/stream runs and exact replays passed')
