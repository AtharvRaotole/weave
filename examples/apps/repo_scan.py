"""Multi-repo scan: one Weave script inspects many git repositories at once.

    python examples/apps/repo_scan.py                 # creates 8 throwaway repos in a temp dir (real git, no network)
    python examples/apps/repo_scan.py ~/src/a ~/src/b  # scan your own repositories (read-only git commands)

For every repository the script reads the current branch, uncommitted changes, local branches with their last commit
dates, and the latest commits; then it reports dirty trees, stale branches (no commit for 90 days) and who committed
most recently across all repos. Every tool runs a read-only `git` command; Weave runs the repositories concurrently and
returns only the report.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import time

import weave

STALE_DAYS = 90


def git(repo, *args):
    return subprocess.run(["git", "-C", repo, *args], check=True, capture_output=True, text=True).stdout


@weave.declare(effect="read", arity=1)
def current_branch(repo):
    """Name of the checked-out branch."""
    return git(repo, "rev-parse", "--abbrev-ref", "HEAD").strip()


@weave.declare(effect="read", arity=1)
def dirty_files(repo):
    """Paths with uncommitted changes (list of strings)."""
    return [line[3:] for line in git(repo, "status", "--porcelain").splitlines()]


@weave.declare(effect="read", arity=1)
def branches(repo):
    """Local branches as a list of {"name", "last_commit_unix"}."""
    out = git(repo, "for-each-ref", "--format=%(refname:short) %(committerdate:unix)", "refs/heads")
    return [{"name": n, "last_commit_unix": int(t)} for n, t in (line.rsplit(" ", 1) for line in out.splitlines())]


@weave.declare(effect="read", arity=2)
def recent_commits(repo, n):
    """The latest n commits on HEAD as a list of {"author", "subject", "unix"}."""
    out = git(repo, "log", f"-{int(n)}", "--format=%an%x1f%s%x1f%ct")
    return [{"author": a, "subject": s, "unix": int(t)} for a, s, t in (l.split("\x1f") for l in out.splitlines())]


SCRIPT = """
report = []
authors = {}
for repo in repos:
    head = current_branch(repo)
    dirty = dirty_files(repo)
    stale = [b["name"] for b in branches(repo) if now - b["last_commit_unix"] > stale_days * 86400]
    for c in recent_commits(repo, 5):
        authors[c["author"]] = authors.get(c["author"], 0) + 1
    if dirty or stale:
        report.append({"repo": repo.split("/")[-1], "head": head, "dirty": len(dirty), "stale": sorted(stale)})
for r in sorted(report, key=lambda r: r["repo"]):
    print(r["repo"] + " (" + r["head"] + "): " + str(r["dirty"]) + " dirty, stale: " + (", ".join(r["stale"]) or "-"))
top = sorted(authors, key=lambda a: (-authors[a], a))
print("most recent committers: " + ", ".join(a + " " + str(authors[a]) for a in top[:3]))
print(str(len(report)) + " of " + str(len(repos)) + " repos need attention")
"""


def make_fixture(root, count=8):
    """Real git repos with deterministic history: some dirty, some with old branches."""
    now = int(time.time())
    people = ["Ada", "Grace", "Linus", "Barbara"]
    repos = []
    for i in range(count):
        repo = os.path.join(root, f"service-{i:02d}")
        os.makedirs(repo)
        git(repo, "init", "-q", "-b", "main")
        for k in range(3 + i % 3):
            who = people[(i + k) % len(people)]
            when = now - (200 - 30 * k) * 86400 if i % 4 == 0 and k == 0 else now - (10 - k) * 86400
            env = dict(os.environ, GIT_AUTHOR_DATE=f"{when} +0000", GIT_COMMITTER_DATE=f"{when} +0000")
            with open(os.path.join(repo, "file.txt"), "a") as f:
                f.write(f"change {k}\n")
            subprocess.run(["git", "-C", repo, "add", "-A"], check=True)
            subprocess.run(["git", "-C", repo, "-c", f"user.name={who}", "-c", f"user.email={who.lower()}@example.com",
                            "commit", "-qm", f"commit {k}"], check=True, env=env)
            if k == 0 and i % 4 == 0:
                git(repo, "branch", "old-experiment")
        if i % 3 == 1:
            with open(os.path.join(repo, "scratch.txt"), "w") as f:
                f.write("work in progress\n")
        repos.append(repo)
    return repos, now


def main(argv):
    with tempfile.TemporaryDirectory() as tmp:
        if argv:
            repos, now = [os.path.abspath(os.path.expanduser(a)) for a in argv], int(time.time())
        else:
            repos, now = make_fixture(tmp)
        engine = weave.Engine()
        engine.install([current_branch, dirty_files, branches, recent_commits])
        source = f"repos = {json.dumps(repos)}\nnow = {now}\nstale_days = {STALE_DAYS}\n" + SCRIPT
        t0 = time.perf_counter()
        result = engine.run(source)
        print(result.stdout, end="")
        print(f"[weave] {4 * len(repos)} git reads in one script: {time.perf_counter() - t0:.2f} s")
        if not argv:
            lines = result.stdout.splitlines()
            assert lines[-1] == "4 of 8 repos need attention", result.stdout
            assert "service-00 (main): 0 dirty, stale: old-experiment" in lines, result.stdout
            print("repo scan ok")


if __name__ == "__main__":
    main(sys.argv[1:])
