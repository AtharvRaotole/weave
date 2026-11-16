"""Real-services backend: the same agent loop against real systems, not a simulator.

Each trial gets a fresh temporary directory holding

- a SQLite database (``tickets`` and ``stock`` tables), queried and updated with
  real SQL on every call;
- a git repository whose history is built with real commits, read with
  ``git log`` and written with ``git tag``;
- a folder of JSON order files on the real filesystem;
- a real HTTP server on 127.0.0.1 (``http.server``) that serves stock levels
  from the SQLite database over real sockets;

and the ``pypi_package`` tool calls the public PyPI JSON API over the internet.
Nothing injects latency: every call costs what the real system costs. Both
modes call the same Python functions with the same concurrency cap, and the
grader inspects the real end state (database rows, git tags, file contents).
"""

from __future__ import annotations

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import random
import shutil
import sqlite3
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

from .tasks import NAMES, Task, _ids
from .world import ToolFailure

READ, WRITE = "read", "irreversible"

_obj = lambda props: {"type": "object", "properties": props, "required": list(props)}  # noqa: E731
_S, _I = {"type": "string"}, {"type": "integer"}
OK = _obj({"ok": {"type": "boolean"}})

TOOLS = {
    "pypi_package": (READ, {"name": "string"},
                     _obj({"name": _S, "version": _S, "requires_python": {"type": ["string", "null"]}}),
                     "Latest release of a package on the public PyPI index (live HTTP request to pypi.org)."),
    "db_list_tickets": (READ, {"status": "string"}, {"type": "array", "items": _S},
                        "Ticket ids with a status (open, closed, investigating), from the SQLite ticket database."),
    "db_get_ticket": (READ, {"ticket_id": "string"},
                      _obj({"id": _S, "customer": _S, "priority": _S, "age_days": _I, "status": _S}),
                      "One ticket row from the SQLite ticket database."),
    "db_update_ticket": (WRITE, {"ticket_id": "string", "status": "string"}, OK,
                         "Set a ticket's status in the SQLite database. Irreversible: never repeat a call."),
    "git_last_commit": (READ, {"path": "string"}, _obj({"commit": _S, "author": _S}),
                        "The last commit in the git repository that changed a file: full hash and author name."),
    "git_create_tag": (WRITE, {"tag": "string", "commit": "string"}, OK,
                       "Create a lightweight git tag pointing at a commit. Irreversible: never repeat a call."),
    "fs_list": (READ, {"directory": "string"}, {"type": "array", "items": _S},
                "File names in a directory of the workspace, sorted."),
    "fs_read_json": (READ, {"path": "string"}, {"type": "object"},
                     "Read a JSON file from the workspace (path relative to the workspace root)."),
    "fs_write": (WRITE, {"path": "string", "text": "string"}, OK,
                 "Write a text file in the workspace, replacing it if it exists. Irreversible: never repeat a call."),
    "warehouse_stock": (READ, {"sku": "string"}, _obj({"sku": _S, "on_hand": _I}),
                        "Units on hand for a SKU from the warehouse HTTP API."),
}

FOOTPRINTS = {
    "pypi_package": {"reads": ["pypi:{name}"]},
    "db_list_tickets": {"reads": ["db:tickets"]},
    "db_get_ticket": {"reads": ["db:ticket:{ticket_id}"]},
    "db_update_ticket": {"writes": ["db:ticket:{ticket_id}", "db:tickets"]},
    "git_last_commit": {"reads": ["git:log"]},
    "git_create_tag": {"writes": ["git:tags"]},
    "fs_list": {"reads": ["fs:listing"]},
    "fs_read_json": {"reads": ["fs:file:{path}"]},
    "fs_write": {"writes": ["fs:file:{path}", "fs:listing"]},
    "warehouse_stock": {"reads": ["stock:{sku}"]},
}

# Packages with plain numeric versions (major.minor...), so the major version is unambiguous.
PACKAGES = ["requests", "numpy", "flask", "django", "click", "rich", "httpx", "pydantic", "attrs", "jinja2",
            "pytest", "urllib3", "idna", "packaging", "setuptools", "pip", "wheel", "six", "pyyaml", "sqlalchemy",
            "boto3", "pandas", "scipy", "matplotlib"]
PYPI = "https://pypi.org/pypi/{}/json"


def fetch_pypi(name, timeout=15):
    req = urllib.request.Request(PYPI.format(urllib.parse.quote(name)), headers={"User-Agent": "weave-bench"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            info = json.load(resp)["info"]
    except urllib.error.HTTPError as exc:
        raise ToolFailure(f"pypi: HTTP {exc.code} for {name}") from None
    except (urllib.error.URLError, TimeoutError) as exc:
        raise ToolFailure(f"pypi: {exc}") from None
    return {"name": info["name"], "version": info["version"], "requires_python": info.get("requires_python")}


def _git(repo, *args, env=None):
    return subprocess.run(["git", *args], cwd=repo, check=True, capture_output=True, text=True,
                          env={**os.environ, **(env or {})}).stdout


class RealWorld:
    """Same interface as ``world.World``: ``call``, ``calls``, ``writes``, ``peak``."""

    def __init__(self, task, *, concurrency=8, **_):
        self.task = task
        self.sem = threading.BoundedSemaphore(concurrency)
        self.lock = threading.Lock()
        self.calls, self.writes = [], []
        self.peak = self.active = 0
        self.tool_busy_s = 0.0
        self.dir = Path(tempfile.mkdtemp(prefix="weave-real-"))
        self.db = self.dir / "ops.sqlite"
        self.repo = self.dir / "repo"
        self.ws = self.dir / "workspace"
        self._server = None
        d = task.data
        self._build_db(d)
        if "git" in d:
            self._build_repo(d["git"])
        self._build_workspace(d.get("orders", {}))
        if "stock" in d:
            self._start_http()
        self.answer = self._pypi_answer(d["packages"]) if "packages" in d else task.answer

    # ---- setup -------------------------------------------------------------------------------
    def _build_db(self, d):
        con = sqlite3.connect(self.db)
        con.execute("create table tickets (id text primary key, customer text, priority text, age_days integer,"
                    " status text)")
        con.execute("create table stock (sku text primary key, on_hand integer)")
        con.executemany("insert into tickets values (?,?,?,?,?)",
                        [(k, v["customer"], v["priority"], v["age_days"], v["status"])
                         for k, v in d.get("tickets", {}).items()])
        con.executemany("insert into stock values (?,?)", list(d.get("stock", {}).items()))
        con.commit()
        con.close()

    def _build_repo(self, g):
        self.repo.mkdir()
        _git(self.repo, "init", "-q", "-b", "main")
        for i, (author, files) in enumerate(g["commits"]):
            for f in files:
                (self.repo / f).write_text(f"{f} revision {i}\n")
            _git(self.repo, "add", "-A")
            email = author.lower().replace(" ", ".") + "@example.com"
            stamp = f"2026-01-01T00:{i // 60:02d}:{i % 60:02d}+0000"
            _git(self.repo, "commit", "-q", "-m", f"change {i}", env={
                "GIT_AUTHOR_NAME": author, "GIT_AUTHOR_EMAIL": email, "GIT_COMMITTER_NAME": author,
                "GIT_COMMITTER_EMAIL": email, "GIT_AUTHOR_DATE": stamp, "GIT_COMMITTER_DATE": stamp})
        self.tags_before = set(_git(self.repo, "tag").split())

    def _build_workspace(self, orders):
        (self.ws / "orders").mkdir(parents=True)
        for name, body in orders.items():
            (self.ws / "orders" / name).write_text(json.dumps(body))

    def _start_http(self):
        db = self.db

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):  # noqa: N802
                parts = self.path.strip("/").split("/")
                row = None
                if len(parts) == 2 and parts[0] == "stock":
                    con = sqlite3.connect(db, timeout=10)
                    row = con.execute("select on_hand from stock where sku = ?",
                                      (urllib.parse.unquote(parts[1]),)).fetchone()
                    con.close()
                body = json.dumps({"on_hand": row[0]} if row else {"error": "unknown sku"}).encode()
                self.send_response(200 if row else 404)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *a):
                pass

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.base = f"http://127.0.0.1:{self._server.server_address[1]}"
        threading.Thread(target=self._server.serve_forever, daemon=True).start()

    def _pypi_answer(self, names):
        # Ground truth is fetched live at trial start, so a release during the run is the only way to drift.
        # Retried: one timeout here crashed a whole run (GPT-4.1, `real`, 2026-11-10; see the paper).
        total = 0
        for n in names:
            for attempt in range(4):
                try:
                    total += int(fetch_pypi(n)["version"].split(".")[0])
                    break
                except ToolFailure:
                    if attempt == 3:
                        raise
                    time.sleep(2.0 * (attempt + 1))
        return str(total)

    # ---- calls -------------------------------------------------------------------------------
    def call(self, name, args):
        if name not in TOOLS:
            raise ToolFailure(f"unknown tool {name}")
        _, params, _, _ = TOOLS[name]
        if set(args) != set(params):
            raise ToolFailure(f"{name} takes arguments {sorted(params)}")
        for k, v in args.items():
            if not isinstance(v, str):
                raise ToolFailure(f"argument {k} must be a string")
        with self.sem:
            with self.lock:
                self.active += 1
                self.peak = max(self.peak, self.active)
                entry = {"name": name, "args": dict(args), "t": time.monotonic()}
                self.calls.append(entry)
            t0 = time.monotonic()
            try:
                out = getattr(self, "_" + name)(**args)
                entry["ok"] = True
                return out
            except ToolFailure as exc:
                entry["ok"], entry["error"] = False, str(exc)
                raise
            finally:
                with self.lock:
                    self.active -= 1
                    self.tool_busy_s += time.monotonic() - t0

    def _sql(self, q, params=(), write=False):
        con = sqlite3.connect(self.db, timeout=10)
        try:
            cur = con.execute(q, params)
            rows = cur.fetchall()
            if write:
                con.commit()
            return rows, cur.rowcount
        finally:
            con.close()

    def _pypi_package(self, name):
        return fetch_pypi(name)

    def _db_list_tickets(self, status):
        return [r[0] for r in self._sql("select id from tickets where status = ? order by rowid", (status,))[0]]

    def _db_get_ticket(self, ticket_id):
        rows, _ = self._sql("select id, customer, priority, age_days, status from tickets where id = ?", (ticket_id,))
        if not rows:
            raise ToolFailure(f"unknown ticket {ticket_id}")
        return dict(zip(("id", "customer", "priority", "age_days", "status"), rows[0]))

    def _db_update_ticket(self, ticket_id, status):
        if status not in ("open", "closed", "investigating"):
            raise ToolFailure(f"bad status {status}")
        _, n = self._sql("update tickets set status = ? where id = ?", (status, ticket_id), write=True)
        if n != 1:
            raise ToolFailure(f"unknown ticket {ticket_id}")
        with self.lock:
            self.writes.append(("db_update_ticket", ticket_id, status))
        return {"ok": True}

    def _git_last_commit(self, path):
        if not (self.repo / path).is_file():
            raise ToolFailure(f"no such file {path}")
        out = _git(self.repo, "log", "-1", "--format=%H%x09%an", "--", path).strip()
        sha, author = out.split("\t")
        return {"commit": sha, "author": author}

    def _git_create_tag(self, tag, commit):
        try:
            _git(self.repo, "tag", tag, commit)
        except subprocess.CalledProcessError as exc:
            raise ToolFailure(f"git tag failed: {exc.stderr.strip()[:200]}") from None
        with self.lock:
            self.writes.append(("git_create_tag", tag, commit))
        return {"ok": True}

    def _path(self, rel):
        p = (self.ws / rel).resolve()
        if self.ws.resolve() not in p.parents and p != self.ws.resolve():
            raise ToolFailure("path escapes the workspace")
        return p

    def _fs_list(self, directory):
        p = self._path(directory)
        if not p.is_dir():
            raise ToolFailure(f"no such directory {directory}")
        return sorted(x.name for x in p.iterdir())

    def _fs_read_json(self, path):
        p = self._path(path)
        if not p.is_file():
            raise ToolFailure(f"no such file {path}")
        return json.loads(p.read_text())

    def _fs_write(self, path, text):
        p = self._path(path)
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)
        with self.lock:
            self.writes.append(("fs_write", path, "".join(text.split())))
        return {"ok": True}

    def _warehouse_stock(self, sku):
        try:
            with urllib.request.urlopen(f"{self.base}/stock/{urllib.parse.quote(sku)}", timeout=10) as resp:
                return {"sku": sku, "on_hand": json.load(resp)["on_hand"]}
        except urllib.error.HTTPError:
            raise ToolFailure(f"unknown sku {sku}") from None

    # ---- grading -----------------------------------------------------------------------------
    def expected_writes(self):
        """For git tasks the expected tags name real commit hashes, known only once the repo exists."""
        if "git" not in self.task.data:
            return list(self.task.writes)
        out = []
        for tag, path in self.task.data["git"]["expect"]:
            out.append(("git_create_tag", tag, self._git_last_commit(path)["commit"]))
        return out

    def verify(self):
        """Check the real end state, not just the write log."""
        d = self.task.data
        if "tickets" in d:
            rows = dict(self._sql("select id, status from tickets")[0])
            want = {k: v["status"] for k, v in d["tickets"].items()}
            for _, tid, status in self.task.writes:
                want[tid] = status
            if rows != want:
                return False
        if "git" in d:
            tags = set(_git(self.repo, "tag").split()) - self.tags_before
            want = {t for t, _ in d["git"]["expect"]}
            if tags != want:
                return False
            for tag, path in d["git"]["expect"]:
                if _git(self.repo, "rev-parse", tag + "^{commit}").strip() != self._git_last_commit(path)["commit"]:
                    return False
        if "orders" in d:
            report = self.ws / "shortfall.txt"
            if not report.is_file() or "".join(report.read_text().split()) != d["report"]:
                return False
        return True

    def close(self):
        if self._server:
            self._server.shutdown()
            self._server.server_close()
        shutil.rmtree(self.dir, ignore_errors=True)


# ---- task families ---------------------------------------------------------------------------------

def pypi_major(n, seed, split):
    rng = random.Random(f"pypi_major:{n}:{seed}")
    names = sorted(rng.sample(PACKAGES, n))
    rng.shuffle(names)
    return Task(f"pypi_major-n{n}-s{seed}", "pypi_major", split, n, seed,
                f"Look up the latest release on PyPI of each of these packages: {', '.join(names)}. Add up their "
                "major version numbers (the integer before the first '.' in the version string). Answer with only "
                "the sum.", {"packages": names}, "live", (), min_tool_calls=n, min_dependent_rounds=1,
                backend="real")


def sqlite_triage(n, seed, split):
    rng = random.Random(f"sqlite_triage:{n}:{seed}")
    tids = _ids(rng, "T", n + 3)
    limit = rng.choice([14, 21, 30])
    tickets = {t: {"customer": rng.choice(NAMES), "priority": rng.choice(["low", "normal", "high"]),
                   "age_days": rng.randrange(1, 60), "status": "open"} for t in tids[:n]}
    for t in tids[n:]:
        tickets[t] = {"customer": NAMES[0], "priority": "low", "age_days": 90, "status": "closed"}
    if not any(v["age_days"] > limit for v in tickets.values() if v["status"] == "open"):
        tickets[tids[0]]["age_days"] = limit + 5
    stale = [t for t, v in tickets.items() if v["status"] == "open" and v["age_days"] > limit]
    return Task(f"sqlite_triage-n{n}-s{seed}", "sqlite_triage", split, n, seed,
                f"In the ticket database, close every open ticket that is more than {limit} days old "
                f"(age_days > {limit}) by setting its status to 'closed'. Leave every other ticket unchanged. "
                "Answer with only the number of tickets you closed.",
                {"tickets": tickets}, str(len(stale)), tuple(sorted(("db_update_ticket", t, "closed") for t in stale)),
                min_tool_calls=1 + n + len(stale), min_dependent_rounds=3, backend="real")


def git_review(n, seed, split):
    rng = random.Random(f"git_review:{n}:{seed}")
    words = ["parser", "lexer", "eval", "cache", "router", "auth", "billing", "search", "export", "import",
             "queue", "worker", "schema", "config", "metrics", "logger", "client", "server", "codec", "index"]
    files = [f"{w}.py" for w in rng.sample(words, n)]
    authors = rng.sample(NAMES, 3)
    commits = [(rng.choice(authors), [f]) for f in files]  # every file exists
    for _ in range(2 * n):
        commits.append((rng.choice(authors), rng.sample(files, rng.randint(1, 3))))
    last = {}
    for a, fs in commits:
        for f in fs:
            last[f] = a
    target = authors[0]
    if target not in last.values():
        commits.append((target, [files[0]]))
        last[files[0]] = target
    expect = sorted((f"review-{f[:-3]}", f) for f in files if last[f] == target)
    return Task(f"git_review-n{n}-s{seed}", "git_review", split, n, seed,
                f"In the git repository, look at these files: {', '.join(files)}. For every file whose last change "
                f"was made by {target}, create the tag review-<name> (the file name without .py, for example "
                f"review-{files[0][:-3]}) pointing at the last commit that changed that file. Create no other tags. "
                "Answer with only the number of tags you created.",
                {"git": {"commits": commits, "expect": expect}}, str(len(expect)), (),
                min_tool_calls=n + len(expect), min_dependent_rounds=2, backend="real")


def warehouse_shortfall(n, seed, split):
    rng = random.Random(f"warehouse_shortfall:{n}:{seed}")
    skus = _ids(rng, "SKU", n)
    stock = {s: rng.randrange(0, 40) for s in skus}
    orders = {f"order-{i + 1:03d}.json": {"sku": s, "qty": rng.randrange(1, 40)} for i, s in enumerate(skus)}
    if not any(o["qty"] > stock[o["sku"]] for o in orders.values()):
        first = next(iter(orders))
        orders[first]["qty"] = stock[orders[first]["sku"]] + 3
    short = sorted(k for k, o in orders.items() if o["qty"] > stock[o["sku"]])
    report = ",".join(short)
    return Task(f"warehouse_shortfall-n{n}-s{seed}", "warehouse_shortfall", split, n, seed,
                "Each JSON file in the workspace directory 'orders' is an order with a sku and a qty. Using the "
                "warehouse stock for each order's SKU, find the orders whose qty is greater than the units on hand. "
                "Write their file names, sorted and comma-separated (for example order-001.json,order-007.json), "
                "to the file 'shortfall.txt'. Answer with only the number of such orders.",
                {"orders": orders, "stock": stock, "report": report}, str(len(short)),
                (("fs_write", "shortfall.txt", report),), min_tool_calls=2 + 2 * n, min_dependent_rounds=4,
                backend="real")


FAMILIES = {"pypi_major": pypi_major, "sqlite_triage": sqlite_triage, "git_review": git_review,
            "warehouse_shortfall": warehouse_shortfall}

REAL_DEV = tuple(FAMILIES[f](n, 1, "real_dev") for f, n in [
    ("pypi_major", 6), ("sqlite_triage", 8), ("git_review", 6), ("warehouse_shortfall", 6)])
# Pre-registered in docs/paper/preregistration.md before any live run.
REAL = tuple(FAMILIES[f](n, s, "real") for f, n, s in [
    ("pypi_major", 10, 101), ("pypi_major", 16, 102),
    ("sqlite_triage", 12, 101), ("sqlite_triage", 20, 102),
    ("git_review", 8, 101), ("git_review", 14, 102),
    ("warehouse_shortfall", 10, 101), ("warehouse_shortfall", 16, 102)])
# Round 3, pre-registered in Addendum 3 before any live run: the REAL recipe with fresh seeds.
REAL2 = tuple(FAMILIES[t.family](t.size, t.seed + 100, "real2") for t in REAL)
# Round 4, pre-registered in Addendum 4 before any live run: the REAL recipe with seeds + 200, plus two more
# git_review tasks (the family round 3 lost on wall time).
CONFIRM4 = tuple(FAMILIES[t.family](t.size, t.seed + 200, "confirm4") for t in REAL) + (
    git_review(10, 303, "confirm4"), git_review(18, 304, "confirm4"))
# Round 5, pre-registered in Addendum 5: the CONFIRM4 recipe with seeds + 100 (parallel_tool_calls ablation).
CONFIRM5 = tuple(FAMILIES[t.family](t.size, t.seed + 100, "confirm5") for t in CONFIRM4)
# Round 9, pre-registered in Addendum 9: the CONFIRM4 recipe with seeds + 500 (fan-out reconfirm, compact listing).
CONFIRM9 = tuple(FAMILIES[t.family](t.size, t.seed + 500, "confirm9") for t in CONFIRM4)
# Round 4 dev-only suite for diagnosing git_review (tuning allowed; never an evidence suite).
REAL_DEV_GIT = tuple(git_review(n, s, "real_dev") for n in (8, 14) for s in (901, 902, 903, 904))
REAL_SUITES = {"real_dev": REAL_DEV, "real_dev_git": REAL_DEV_GIT, "real": REAL, "real2": REAL2, "confirm4": CONFIRM4, "confirm5": CONFIRM5, "confirm9": CONFIRM9}
