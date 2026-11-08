"""Aggregate budgets are byte-accounting limits, not process memory caps."""
import json
import weave

def rejected(fn, kind):
    try:fn()
    except ValueError as exc:
        assert kind in str(exc),str(exc)
    else:raise AssertionError(f'{kind} budget accepted overflow')

source='x="é"\n'
size=len(source.encode())
for cap in (size-1,size,size+1):
    e=weave.Engine();e.set_limits(max_source_bytes=cap)
    if cap<size:rejected(lambda:e.run(source),'source')
    else:assert e.run(source).bindings['x']=='é'
e=weave.Engine();e.set_limits(max_source_bytes=12)
s=e.session();s.push('x=1\n');s.push('y=2\n');s.push('z=3\n')
rejected(lambda:s.push('q=4\n'),'source');rejected(s.finish,'source')
for cap in (63,64,65):
    e=weave.Engine();e.set_limits(max_output_bytes=cap)
    src='for i in range(32):\n    print("é",end="")\n'
    if cap<64:rejected(lambda:e.run(src),'output')
    else:assert len(e.run(src).stdout.encode())==64
# Aggregate over many individually legal results, including source/manifest.
e=weave.Engine();e.tool('read',lambda:1,effect='read')
src='for i in range(12):\n    print(read())\n'
r=e.run(src)
size=len(json.dumps(json.loads(r.trace_json()),separators=(',',':'),ensure_ascii=False).encode())
for cap in (size-1,size,size+1):
    e.set_limits(max_trace_bytes=cap)
    if cap<size:rejected(lambda:e.run(src),'trace')
    else:assert e.run(src).stdout==r.stdout
# Trace/input/output budgets cannot be caught to keep emitting/dispatching writes.
e=weave.Engine();writes=[];e.tool('write',lambda:writes.append(1),effect='irreversible')
e.set_limits(max_output_bytes=1)
rejected(lambda:e.run('try:\n    print("too much")\nexcept:\n    write()\n'),'output')
assert not writes
print('source UTF-8/session, aggregate stdout, trace envelope/event boundaries and fatal-breach assertions passed')
