"""What happens after a tool returns an {"error": ...} dict vs raises, in every recorded BFCL Weave trace (rounds 11 to 14).

Reads committed traces only, no API calls. Calls are taken from the `[calls]` trace in each run_weave result.

    python bench/bfcl/error_flow.py
"""
import json, glob, re, collections, os
ROOT=os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
eff=json.load(open(ROOT+'/bench/bfcl/effects.json'))
READS=set(eff['reads'])
CALL = re.compile(r"^(\w+)\((.*?)\) -> (.*)$")
stats=collections.Counter(); after_calls=collections.Counter()
for path in sorted(glob.glob(ROOT+'/bench/results/v1*/**/traces.jsonl', recursive=True)):
    if 'bfcl' not in path: continue
    for line in open(path):
        t=json.loads(line)
        if t.get('arm')!='weave': continue
        msgs=t['messages']
        for i,m in enumerate(msgs):
            if m.get('role')!='tool': continue
            c=m.get('content') or ''
            calls=[CALL.match(l) for l in c.split('[calls]\n',1)[1].splitlines()] if '[calls]\n' in c else []
            calls=[x for x in calls if x]
            first_err=None; first_raise=None
            for k,x in enumerate(calls):
                if x.group(3).startswith('{"error"') and first_err is None: first_err=k
                if x.group(3).startswith('Error during execution') and first_raise is None: first_raise=k
            # what did the model do next? next assistant message
            nxt=next((mm for mm in msgs[i+1:] if mm.get('role') in ('assistant','user')), None)
            retried = bool(nxt and nxt.get('role')=='assistant' and nxt.get('tool_calls'))
            if first_err is not None:
                stats['scripts_with_error_dict']+=1
                later=calls[first_err+1:]
                if later:
                    stats['error_dict_then_more_calls']+=1
                    w=[x.group(1) for x in later if x.group(1) not in READS]
                    if w: stats['error_dict_then_state_changing_calls']+=1; after_calls['n_state_changing_after']+=len(w)
                    after_calls['n_after']+=len(later)
                stats['error_dict_model_called_again' if retried else 'error_dict_model_stopped']+=1
                script_failed = c.startswith('error') or '\nerror:' in c
                stats['error_dict_script_reported_error' if script_failed else 'error_dict_script_looked_ok']+=1
                if not script_failed: stats['error_dict_looked_ok_model_called_again' if retried else 'error_dict_looked_ok_model_stopped']+=1
            elif first_raise is not None:
                stats['scripts_with_raised_error']+=1
                stats['raised_model_called_again' if retried else 'raised_model_stopped']+=1
print(json.dumps(dict(sorted(stats.items())),indent=1)); print(dict(after_calls))

