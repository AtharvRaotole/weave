import json
import os
import signal
import subprocess
import sys
from pathlib import Path

here = Path(__file__).resolve().parent
for marker in here.glob("deadline-marker-*"):
    marker.unlink()
results = []
for mode in ("saturation", "pickle"):
    proc = subprocess.Popen([sys.executable, str(here / "deadline_probe.py"), mode], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, start_new_session=True)
    timed_out = False
    try:
        stdout, stderr = proc.communicate(timeout=4)
    except subprocess.TimeoutExpired:
        timed_out = True
        os.killpg(proc.pid, signal.SIGKILL)
        stdout, stderr = proc.communicate()
    results.append({"mode": mode, "watchdog_timeout": timed_out, "exit": proc.returncode, "stdout": stdout, "stderr": stderr})
print(json.dumps(results, indent=2))
