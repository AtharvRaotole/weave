"""Run in an owned process group under the audit watchdog. No network."""
from pathlib import Path
import json
import tempfile
import time
import weave
from weave.workers import Pool

def slow(path):
    time.sleep(.8)
    Path(path).write_text("worker continued")
    return 1

def echo(value):
    return value

def main():
    with tempfile.TemporaryDirectory() as temp:
        for mode in ("tool_timeout", "run_deadline", "drop_session"):
            for trial in range(3):
                e = weave.Engine(worker_pool=1)
                e.tool("slow", slow, effect="read", hard_deadline=True,
                       timeout_ms=50 if mode == "tool_timeout" else 1000)
                e.set_limits(max_wall_time_ms=100)
                paths = [str(Path(temp)/f"{mode}-{trial}-{i}") for i in range(3)]
                src = '\n'.join(f'x{i}=slow({p!r})' for i,p in enumerate(paths))+'\n'
                start=time.monotonic()
                if mode == "drop_session":
                    s=e.session();s.push(src);time.sleep(.02);del s
                else:
                    try: e.run(src)
                    except ValueError: pass
                    else: raise AssertionError("no timeout")
                elapsed=time.monotonic()-start
                assert elapsed < .3, (mode,elapsed)
                time.sleep(.9)
                assert not any(Path(p).exists() for p in paths), mode
                # Explicitly clean pool between cases, including pending jobs.
                del e
                print(mode, trial, elapsed, flush=True)
        pool=Pool(1)
        try:
            try: pool.start(lambda: None,[],{})
            except (AttributeError,TypeError): pass
            else: raise AssertionError("unpickleable function accepted")
            assert pool.idle.qsize()==1
            assert pool.start(echo,["ok"],{}).wait()==("ok","ok")
            # Replace a killed worker; subsequent good work still succeeds.
            job=pool.start(slow,[str(Path(temp)/"replace")],{})
            time.sleep(.02);job.kill()
            assert pool.start(echo,["after replacement"],{}).wait()==("ok","after replacement")
        finally:pool.close()
    print('worker queue, deadlines, dropped session, pickle recovery and replacement passed')

if __name__=='__main__': main()
