"""Local-only cancellation fixture; no credentials/network or external writes."""
import json,sys,threading,time
from pathlib import Path
log=Path(sys.argv[1]); honor=sys.argv[2]=='honor'; lock=threading.Lock(); cancelled=set()
def record(kind,id):
    with lock:
        with log.open('a') as f: f.write(json.dumps([kind,id])+'\n')
def send(value):
    with lock: print(json.dumps(value),flush=True)
def work(req):
    id=req['id'];record('start',id);time.sleep(.35)
    if honor and id in cancelled: record('stopped',id)
    else: record('completed',id)
    send({'jsonrpc':'2.0','id':id,'result':{'content':[{'type':'text','text':'done'}]}})
for line in sys.stdin:
    req=json.loads(line);method=req.get('method');id=req.get('id')
    if method=='notifications/cancelled':
        cancelled.add(req['params']['requestId']);record('cancel',req['params']['requestId']);continue
    if id is None: continue
    if method=='initialize': result={'protocolVersion':'2025-06-18','capabilities':{},'serverInfo':{'name':'fixture','version':'0'}}
    elif method=='tools/list': result={'tools':[{'name':'slow','inputSchema':{'type':'object'},'annotations':{'readOnlyHint':True}},{'name':'write','inputSchema':{'type':'object'}}]}
    elif method=='tools/call': threading.Thread(target=work,args=(req,),daemon=True).start();continue
    else: result={}
    send({'jsonrpc':'2.0','id':id,'result':result})
