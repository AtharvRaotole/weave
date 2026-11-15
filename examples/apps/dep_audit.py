"""Dependency auditor: check every pinned package of a requirements file in ONE Weave script.

    python examples/apps/dep_audit.py                       # offline: SYNTHETIC packages, simulated 80 ms latency
    python examples/apps/dep_audit.py --live requirements.txt   # live: PyPI JSON API + OSV.dev, read-only

Per package the script asks for the latest release and for known advisories affecting the pinned version, then
sorts and prints only the packages that need attention. A function-calling agent doing the same job would spend one
model turn per round of lookups and carry every raw response in its context; here the model (or you) sends one
script, Weave runs the independent lookups concurrently, and only the summary comes back.

Offline mode uses invented package names and invented advisory ids (prefixed SYNTH-); they describe no real software.
"""

from __future__ import annotations

import json
import sys
import time
import urllib.request

import weave

# ---- offline fixtures (synthetic; not real packages or advisories) ----------------------------------------------
LATENCY_S = 0.08
FIXTURE = {
    "acme-http": ("2.4.1", {"2.1.0": ["SYNTH-0001"]}),
    "acme-yaml": ("6.0.2", {}),
    "tinyorm": ("1.9.0", {"1.2.0": ["SYNTH-0002", "SYNTH-0003"]}),
    "fastjsonish": ("3.3.0", {}),
    "quickcache": ("0.9.4", {}),
    "pixelpress": ("10.1.0", {"9.0.0": ["SYNTH-0004"]}),
    "cronlite": ("1.0.3", {}),
    "mailmerge-x": ("4.2.0", {}),
    "geoshape": ("2.0.0", {}),
    "tabular-io": ("1.5.2", {}),
    "authkit": ("5.0.1", {"4.8.0": ["SYNTH-0005"]}),
    "logfmtx": ("0.4.0", {}),
}
OFFLINE_REQUIREMENTS = """acme-http==2.1.0
acme-yaml==6.0.2
tinyorm==1.2.0
fastjsonish==3.3.0
quickcache==0.9.1
pixelpress==9.0.0
cronlite==1.0.3
mailmerge-x==4.2.0
geoshape==1.7.0
tabular-io==1.5.2
authkit==4.8.0
logfmtx==0.4.0
"""


def _fixture_latest(package):
    time.sleep(LATENCY_S)
    return FIXTURE[package][0]


def _fixture_advisories(package, version):
    time.sleep(LATENCY_S)
    return FIXTURE[package][1].get(version, [])


def offline_tools():
    @weave.declare(effect="read", arity=1)
    def latest_version(package):
        """Latest released version of a package (string)."""
        return _fixture_latest(package)

    @weave.declare(effect="read", arity=2)
    def advisories(package, version):
        """Advisory ids that affect this exact package version (list of strings)."""
        return _fixture_advisories(package, version)

    return [latest_version, advisories]


# ---- live tools (PyPI and OSV.dev, read-only) --------------------------------------------------------------------
def _json(url, body=None):
    req = urllib.request.Request(url, data=json.dumps(body).encode() if body else None,
                                 headers={"Content-Type": "application/json", "User-Agent": "weave-dep-audit"})
    with urllib.request.urlopen(req, timeout=20) as r:
        return json.load(r)


def live_tools():
    @weave.declare(effect="read", arity=1)
    def latest_version(package):
        """Latest released version of a package on PyPI (string)."""
        return _json(f"https://pypi.org/pypi/{package}/json")["info"]["version"]

    @weave.declare(effect="read", arity=2)
    def advisories(package, version):
        """OSV.dev advisory ids that affect this exact PyPI package version (list of strings)."""
        res = _json("https://api.osv.dev/v1/query",
                    {"version": version, "package": {"name": package, "ecosystem": "PyPI"}})
        return sorted(v["id"] for v in res.get("vulns", []))

    return [latest_version, advisories]


# ---- the one script ----------------------------------------------------------------------------------------------
SCRIPT = """
rows = []
for line in requirements:
    name = line.split("==")[0]
    pinned = line.split("==")[1]
    latest = latest_version(name)
    ids = advisories(name, pinned)
    if ids or latest != pinned:
        rows.append({"package": name, "pinned": pinned, "latest": latest, "advisories": ids})
rows = sorted(rows, key=lambda r: (-len(r["advisories"]), r["package"]))
for r in rows:
    ids = r["advisories"]
    flag = "outdated"
    if ids:
        flag = str(len(ids)) + " ADVISORIES " + ", ".join(ids[:3]) + (", ..." if len(ids) > 3 else "")
    print(r["package"] + " " + r["pinned"] + " -> " + r["latest"] + "  " + flag)
print(str(len(rows)) + " of " + str(len(requirements)) + " packages need attention")
"""


def parse(text):
    out = []
    for line in text.splitlines():
        line = line.split("#")[0].strip()
        if "==" in line:
            out.append(line.replace(" ", ""))
    return out


def main(argv):
    live = "--live" in argv
    if live:
        paths = [a for a in argv if not a.startswith("--")]
        reqs = parse(open(paths[0]).read()) if paths else ["requests==2.19.0", "urllib3==1.24.1", "jinja2==2.10"]
        tools = live_tools()
    else:
        reqs = parse(OFFLINE_REQUIREMENTS)
        tools = offline_tools()
    engine = weave.Engine()
    engine.install(tools)
    source = "requirements = " + json.dumps(reqs) + "\n" + SCRIPT
    t0 = time.perf_counter()
    result = engine.run(source)
    weave_s = time.perf_counter() - t0
    print(result.stdout, end="")
    print(f"[weave] {2 * len(reqs)} lookups in one script: {weave_s:.2f} s")
    if not live:
        # The same lookups one after another, as a plain loop would do them.
        t0 = time.perf_counter()
        for r in reqs:
            name, pinned = r.split("==")
            _fixture_latest(name)
            _fixture_advisories(name, pinned)
        print(f"[sequential] the same lookups one by one: {time.perf_counter() - t0:.2f} s")
        assert result.stdout.splitlines()[0].startswith("tinyorm 1.2.0 -> 1.9.0  2 ADVISORIES SYNTH-0002, SYNTH-0003")
        assert result.stdout.splitlines()[-1] == "6 of 12 packages need attention", result.stdout
        print("dep audit ok")


if __name__ == "__main__":
    main(sys.argv[1:])
