"""Per-task, per-turn calls of both arms against BFCL ground truth for one run directory (no API calls).

    /path/to/bfcl-venv/bin/python bench/bfcl/diagnose_r9_losses.py RUN_DIR OUT.json
"""
import json, collections, sys, re
from bfcl_eval.utils import load_dataset_entry, load_ground_truth_entry
D = sys.argv[1]
GT = {g['id']: g['ground_truth'] for g in load_ground_truth_entry('multi_turn_base')}
EN = {e['id']: e for e in load_dataset_entry('multi_turn_base')}
rows = [json.loads(l) for l in open(D + '/rows.jsonl')]
tr = {(t['id'], t['arm']): t for t in map(json.loads, open(D + '/traces.jsonl'))}
by = collections.defaultdict(dict)
for r in rows: by[r['id']][r['arm']] = r
def calls_of(turn):
    out = []
    for step in turn:
        if isinstance(step, list):
            for c in step:
                for k, v in c.items(): out.append((k, json.loads(v) if isinstance(v, str) and v.startswith('{') else v))
    return out
def gt_calls(s):
    m = re.match(r"(\w+)\((.*)\)$", s, re.S); return m.group(1) if m else s
def script_errors(t):
    errs = []
    for m in t['messages']:
        if m.get('role') == 'tool':
            c = m.get('content') or ''
            if c.startswith('error') or '\nerror:' in c or 'Traceback' in c: errs.append(c.split('\n')[0][:160])
    return errs
def err_dicts(t):
    n = 0
    for m in t['messages']:
        if m.get('role') == 'tool': n += len(re.findall(r'-> \{"error":', m.get('content') or ''))
    return n
out = []
for tid, a in sorted(by.items(), key=lambda x: int(x[0].split('_')[-1])):
    w, n = a['weave'], a['native']
    wt, nt = tr[(tid, 'weave')], tr[(tid, 'native')]
    rec = {'id': tid, 'weave': w['passed'], 'native': n['passed'], 'w_err': w['error_type'], 'n_err': n['error_type'],
           'script_errors': script_errors(wt), 'err_dicts_w': err_dicts(wt), 'turns': []}
    for i, gt in enumerate(GT[tid]):
        wc = [c[0] for c in calls_of(wt['result'][i])] if i < len(wt['result']) else None
        nc = [c[0] for c in calls_of(nt['result'][i])] if i < len(nt['result']) else None
        g = [gt_calls(x) for x in gt]
        rec['turns'].append({'gt': g, 'weave': wc, 'native': nc})
    out.append(rec)
json.dump(out, open(sys.argv[2], 'w'), indent=1)
c = collections.Counter(); 
for r in out:
    if not r['weave']: c[r['w_err']] += 1
print('weave fail types', c)
c = collections.Counter()
for r in out:
    if not r['native']: c[r['n_err']] += 1
print('native fail types', c)
