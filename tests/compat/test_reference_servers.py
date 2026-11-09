"""Compatibility: weave-mcp proxying the official MCP reference servers.

Servers (pinned, installed from lockfiles in this directory):
- @modelcontextprotocol/server-filesystem and server-memory  (npm ci)
- mcp-server-git                                             (pip --require-hashes)

The test writes real files, a real git commit and a real memory graph through
`weave_run`, then checks disk state directly. It also checks that upstream
`outputSchema`, annotations and `structuredContent` pass through weave-mcp.

Set WEAVE_COMPAT_GIT_PYTHON to an interpreter with requirements-git.txt
installed. With WEAVE_COMPAT_REQUIRED=1 (CI) missing servers fail the test;
otherwise the test reports what is missing and exits 0.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
BINARY = ROOT / "target" / "debug" / ("weave-mcp.exe" if os.name == "nt" else "weave-mcp")
FS = HERE / "node_modules/@modelcontextprotocol/server-filesystem/dist/index.js"
MEMORY = HERE / "node_modules/@modelcontextprotocol/server-memory/dist/index.js"
GIT_PYTHON = os.environ.get("WEAVE_COMPAT_GIT_PYTHON", "")
IDENTITY = {"GIT_AUTHOR_NAME": "Atharv Raotole", "GIT_COMMITTER_NAME": "Atharv Raotole",
            "GIT_AUTHOR_EMAIL": "100996694+AtharvRaotole@users.noreply.github.com",
            "GIT_COMMITTER_EMAIL": "100996694+AtharvRaotole@users.noreply.github.com"}


def missing():
    gaps = [str(p) for p in (BINARY, FS, MEMORY) if not p.exists()]
    if not GIT_PYTHON or subprocess.run([GIT_PYTHON, "-c", "import mcp_server_git"], capture_output=True).returncode:
        gaps.append("mcp-server-git (set WEAVE_COMPAT_GIT_PYTHON)")
    return gaps


class Client:
    def __init__(self, config):
        self.proc = subprocess.Popen([str(BINARY), "--config", str(config)], stdin=subprocess.PIPE,
                                     stdout=subprocess.PIPE, text=True)
        self.next = 0
        self.request("initialize", {"protocolVersion": "2025-06-18"})

    def request(self, method, params=None):
        self.next += 1
        self.proc.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.next, "method": method,
                                          "params": params or {}}) + "\n")
        self.proc.stdin.flush()
        reply = json.loads(self.proc.stdout.readline())
        assert "error" not in reply, reply
        return reply["result"]

    def call(self, name, arguments):
        return self.request("tools/call", {"name": name, "arguments": arguments})

    def close(self):
        self.proc.stdin.close()
        self.proc.wait(timeout=20)


def main():
    gaps = missing()
    if gaps:
        print("reference servers not installed:", "; ".join(gaps))
        sys.exit(1 if os.environ.get("WEAVE_COMPAT_REQUIRED") == "1" else 0)
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp).resolve()
        data, memory_file = tmp / "data", tmp / "memory.jsonl"
        data.mkdir()
        git_env = {"GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1", **IDENTITY}
        subprocess.run(["git", "init", "-q", "-b", "main", str(data)], check=True, env={**os.environ, **git_env})
        config = tmp / "config.json"
        config.write_text(json.dumps({"upstreams": [
            {"name": "fs", "command": "node", "args": [str(FS), str(data)], "prefix": "fs_"},
            {"name": "git", "command": GIT_PYTHON, "args": ["-m", "mcp_server_git", "--repository", str(data)],
             "prefix": "", "env": git_env},
            {"name": "memory", "command": "node", "args": [str(MEMORY)], "prefix": "mem_",
             "env": {"MEMORY_FILE_PATH": str(memory_file)}},
        ]}))
        client = Client(config)
        try:
            tools = {t["name"]: t for t in client.request("tools/list")["tools"]}
            assert {"fs_write_file", "fs_read_text_file", "git_add", "git_commit", "git_log",
                    "mem_create_entities", "mem_read_graph"} <= set(tools), sorted(tools)
            # Upstream outputSchema and annotations are forwarded; readOnlyHint
            # reflects the effect weave enforces.
            assert "outputSchema" in tools["fs_read_text_file"], tools["fs_read_text_file"]
            assert "outputSchema" in tools["mem_read_graph"]
            assert tools["fs_write_file"]["annotations"]["destructiveHint"] is True
            assert tools["git_commit"]["annotations"]["readOnlyHint"] is False
            assert tools["git_status"]["annotations"]["readOnlyHint"] is True

            note = data / "notes.txt"
            script = (
                f'fs_write_file(path={str(note)!r}, content="hello reference servers")\n'
                f'text = fs_read_text_file(path={str(note)!r})\n'
                f'git_add(repo_path={str(data)!r}, files=["notes.txt"])\n'
                f'git_commit(repo_path={str(data)!r}, message="Add notes")\n'
                'mem_create_entities(entities=[{"name": "Notes", "entityType": "file", '
                '"observations": ["hello reference servers"]}])\n'
                'graph = mem_read_graph()\n'
                'print(text["content"])\n'
                'names = []\n'
                'for e in graph["entities"]:\n'
                '    names.append(e["name"])\n'
                'print(names)\n'
            )
            result = client.call("weave_run", {"source": script})
            body = result["content"][0]["text"]
            assert not result.get("isError"), body
            stdout = json.loads(body)["stdout"]
            assert stdout == "hello reference servers\n['Notes']\n", stdout

            # Real state, checked without going through the servers.
            assert note.read_text() == "hello reference servers"
            subject = subprocess.run(["git", "-C", str(data), "log", "-1", "--format=%s"], check=True,
                                     capture_output=True, text=True).stdout.strip()
            assert subject == "Add notes", subject
            tracked = subprocess.run(["git", "-C", str(data), "ls-files"], check=True,
                                     capture_output=True, text=True).stdout.split()
            assert tracked == ["notes.txt"], tracked
            assert '"name":"Notes"' in memory_file.read_text().replace(" ", "")

            # Direct calls return the upstream envelope, structuredContent included.
            direct = client.call("fs_read_text_file", {"path": str(note)})
            assert direct["structuredContent"]["content"] == "hello reference servers", direct
            log = client.call("git_log", {"repo_path": str(data), "max_count": 1})
            assert "Add notes" in json.dumps(log), log
            # A path outside the allowed root is refused by the upstream, not weave.
            outside = client.call("fs_read_text_file", {"path": str(tmp / "config.json")})
            assert outside.get("isError") is True, outside
        finally:
            client.close()
    print("reference filesystem/git/memory servers: real writes, outputSchema and structured results passed")


if __name__ == "__main__":
    main()
