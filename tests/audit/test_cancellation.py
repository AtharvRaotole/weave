"""Request-ID forwarding, queued-write suppression, EOF and unsafe retry regressions."""
import json,subprocess,sys,tempfile,time
from pathlib import Path
import weave
ROOT=Path(__file__).resolve().parents[2]

def retry_case(effect,safe,expected):
    e=weave.Engine(); calls=[]
    def fail(): calls.append(1);raise ValueError('synthetic failure')
    e.tool('fail',fail,effect=effect,retries=2,retry_safe=safe)
    try: e.run('fail()\n')
    except Exception: pass
    else: raise AssertionError('failure unexpectedly succeeded')
    assert len(calls)==expected,(effect,safe,len(calls))
for effect in ('reversible','irreversible'):
    retry_case(effect,False,1);retry_case(effect,True,3)
retry_case('pure',False,3);retry_case('read',False,3)
# Both decorator installation paths preserve the explicit marker.
for path in ('engine','marked'):
    e=weave.Engine();calls=[]
    def fail():calls.append(1);raise ValueError('synthetic')
    if path=='engine': e.declare(effect='irreversible',retries=1,retry_safe=True)(fail)
    else: e.install([weave.declare(effect='irreversible',retries=1,retry_safe=True)(fail)])
    try:e.run('fail()\n')
    except Exception:pass
    assert len(calls)==2,(path,calls)

def wait_for(fn):
    end=time.monotonic()+3
    while time.monotonic()<end:
        value=fn()
        if value:return value
        time.sleep(.005)
    raise AssertionError('fixture timed out')
for direct,honor,disconnect in [(False,True,False),(True,True,False),(False,False,False),(False,True,True)]:
    with tempfile.TemporaryDirectory() as tmp:
        log=Path(tmp)/'events';config=Path(tmp)/'config'
        config.write_text(json.dumps({'upstreams':[{'name':'test','command':sys.executable,'args':[str(Path(__file__).with_name('cancellation_server.py')),str(log),'honor' if honor else 'ignore'],'prefix':'t_'}]}))
        p=subprocess.Popen([str(ROOT/'target/debug/weave-mcp'),'--config',str(config)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        def send(req):p.stdin.write(json.dumps(dict(jsonrpc='2.0',**req))+'\n');p.stdin.flush()
        def events():return [json.loads(x) for x in log.read_text().splitlines()] if log.exists() else []
        send({'id':'req-string','method':'tools/call','params':{'name':'t_slow' if direct else 'weave_run','arguments':{} if direct else {'source':'t_slow()\nt_write()\n'}}})
        started=wait_for(lambda:next((id for k,id in events() if k=='start'),None))
        before=time.monotonic()
        if disconnect:p.stdin.close();p.stdin=None
        else:
            send({'method':'notifications/cancelled','params':{'requestId':'req-string','reason':'fixture'}})
            send({'id':99,'method':'ping'})
        if not disconnect:
            wait_for(lambda:any(k=='cancel' and id==started for k,id in events()))
        assert time.monotonic()-before<.3
        if not disconnect:
            replies=[json.loads(p.stdout.readline()) for _ in range(2)]
            assert any(r['id']==99 for r in replies)
            assert any(r['id']=='req-string' and ('error' in r or r.get('result',{}).get('isError')) for r in replies),replies
            time.sleep(.45)
            assert len([x for x in events() if x[0]=='start'])==1,events()
            assert any(k==('stopped' if honor else 'completed') for k,_ in events()),events()
            p.stdin.close();p.stdin=None
        _,err=p.communicate(timeout=3);assert p.returncode==0,err
        if disconnect:
            assert len([x for x in events() if x[0]=='start'])==1 and not any(k=='completed' for k,_ in events()),events()
print('Cancellation: string-ID script/direct forwarding, queued write suppression, cooperative/ignored notification and EOF checks passed; retry policy: 8 registration/effect cases passed')
# Replay mismatch diagnostics must not echo actual or recorded arguments.
e=weave.Engine();e.tool('echo',lambda x:x,effect='read')
source='echo("SENSITIVE_ACTUAL_ARGUMENT")\n';trace=json.loads(e.run(source).trace_json())
trace['events'][0]['args']=['SENSITIVE_RECORDED_ARGUMENT']
try:e.replay(source,json.dumps(trace))
except Exception as exc:
    assert 'SENSITIVE_ACTUAL_ARGUMENT' not in str(exc) and 'SENSITIVE_RECORDED_ARGUMENT' not in str(exc),str(exc)
else:raise AssertionError('tampered replay accepted')
print('Replay divergence diagnostic omits actual/recorded argument values')

# Bounded admission and duplicate IDs do not block cancellation or ping.
with tempfile.TemporaryDirectory() as tmp:
    cfg=Path(tmp)/'config'
    cfg.write_text(json.dumps({'tools':[{'name':'slow','effect':'read','behavior':'echo','delay_ms':350}]}))
    p=subprocess.Popen([str(ROOT/'target/debug/weave-mcp'),'--config',str(cfg)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    requests=[{'id':i,'method':'tools/call','params':{'name':'weave_run','arguments':{'source':'slow()\n'}}} for i in range(17)]
    requests+=[requests[0],{'id':'ping','method':'ping'}]
    requests += [{'method':'notifications/cancelled','params':{'requestId':i}} for i in range(17)]
    p.stdin.write(''.join(json.dumps(dict(jsonrpc='2.0',**r))+'\n' for r in requests));p.stdin.flush()
    replies=[json.loads(p.stdout.readline()) for _ in range(19)]
    assert any('maximum 16' in r.get('error',{}).get('message','') for r in replies),replies
    assert any('duplicate active' in r.get('error',{}).get('message','') for r in replies),replies
    assert any(r['id']=='ping' for r in replies)
    p.stdin.close();p.stdin=None
    _,err=p.communicate(timeout=3);assert p.returncode==0,err
print('MCP 16-request admission, active duplicate-ID rejection and responsive ping/cancellation checks passed')
