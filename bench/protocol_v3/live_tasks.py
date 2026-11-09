"""Frozen live task set ``live-v1`` for the shipped weave-mcp fixture.

Ten maintainer-authored tasks over the pinned local file/git/memory MCP fixture
(``local_mcp.RemoteFixture``). They were written and frozen *before* the first
live run, and the manifest hash below is recorded in every result file. They are
public, small and related: NOT secret, NOT statistically independent, and NOT a
general agent benchmark. They deliberately include tasks where native tool
calling needs no extra turns (single reads, one-shot fan-out) as well as
dependent chains where a script can save turns.

Oracles (answer, final state, ordered write log) live only here and in the
grader; prompts carry only ``instruction`` and the tool contracts.
"""

from .core import Task, canonical, digest

BASELINE_HEAD = "fixture baseline"


def _task(
    id,
    family,
    instruction,
    files,
    answer,
    writes=(),
    *,
    head=BASELINE_HEAD,
    memory=None,
):
    final = dict(files)
    tracked = set(files)
    for key, text in writes:
        tool, path = key.split(":", 1)
        if tool == "fs_write":
            final[path] = text
        elif tool == "git_commit":
            tracked.add(path)
    final["@git_head"] = head
    final["@git_tracked"] = "\n".join(sorted(tracked))
    final["@memory"] = canonical(memory or {})
    return Task(id, "live_v1", family, instruction, files, answer, final, tuple(writes))


LIVE_V1 = (
    _task(
        "sum3",
        "parallel_fanout",
        "Read a.txt, b.txt and c.txt. Each holds one integer. Answer with only their sum as an integer.",
        {"a.txt": "11", "b.txt": "20", "c.txt": "-4"},
        "27",
    ),
    _task(
        "fallback",
        "missing_fallback",
        "Read missing.txt; if that read fails, read backup.txt instead. Answer with only the exact content you obtained.",
        {"backup.txt": "safe-backup"},
        "safe-backup",
    ),
    _task(
        "chain",
        "dependent_chain",
        "Read start.txt. Its content is the name of the next file to read. Keep following file names this way until a file's content begins with END: and answer with only the text after END:.",
        {
            "start.txt": "hop1.txt",
            "hop1.txt": "hop2.txt",
            "hop2.txt": "hop3.txt",
            "hop3.txt": "END:violet",
        },
        "violet",
    ),
    _task(
        "index_max",
        "dependent_fanout",
        "Read index.txt; it lists file names, one per line. Read every listed file; each holds one integer score. Answer with only the file name that has the highest score.",
        {
            "index.txt": "r1.txt\nr2.txt\nr3.txt\nr4.txt\nr5.txt\nr6.txt",
            "r1.txt": "42",
            "r2.txt": "17",
            "r3.txt": "93",
            "r4.txt": "8",
            "r5.txt": "61",
            "r6.txt": "77",
        },
        "r3.txt",
    ),
    _task(
        "receipt",
        "read_then_write",
        "Read item.txt and write receipt.txt containing exactly RECEIVED followed by one space and the item text. Then answer with only the word recorded.",
        {"item.txt": "blue"},
        "recorded",
        (("fs_write:receipt.txt", "RECEIVED blue"),),
    ),
    _task(
        "journal",
        "append_preserve",
        "journal.txt ends with a newline. Append the line closed (followed by a newline) to it, preserving all existing content exactly. Answer with only the new number of lines in the file.",
        {"journal.txt": "open\nreview\n"},
        "3",
        (("fs_write:journal.txt", "open\nreview\nclosed\n"),),
    ),
    _task(
        "integration",
        "file_git_memory",
        "Read item.txt, write receipt.txt as RECEIVED plus a space and its content, commit receipt.txt with the message Record receipt, store the text recorded blue under memory key Receipt, then answer with only the word recorded.",
        {"item.txt": "blue"},
        "recorded",
        (
            ("fs_write:receipt.txt", "RECEIVED blue"),
            ("git_commit:receipt.txt", "Record receipt"),
            ("memory_put:Receipt", "recorded blue"),
        ),
        head="Record receipt",
        memory={"Receipt": ["recorded blue"]},
    ),
    _task(
        "conditional_memory",
        "branch_then_write",
        "Read status.txt. If its content is ok, store the observation green under memory key Status; otherwise store red under that key. Store nothing else. Answer with only the observation you stored.",
        {"status.txt": "ok"},
        "green",
        (("memory_put:Status", "green"),),
        memory={"Status": ["green"]},
    ),
    _task(
        "deny_noop",
        "negative_write",
        "Read policy.txt. If it says deny, change nothing and answer with only the word denied; otherwise write allowed.txt containing yes and answer with only the word allowed.",
        {"policy.txt": "deny"},
        "denied",
    ),
    _task(
        "git_head",
        "single_read",
        "Answer with only the subject line of the current git HEAD commit.",
        {"readme.txt": "fixture"},
        BASELINE_HEAD,
    ),
)


def live_manifest(tasks=LIVE_V1):
    return {
        "task_set": "live-v1",
        "tasks": [t.id for t in tasks],
        "manifest_hash": digest(
            [
                {
                    "id": t.id,
                    "family": t.family,
                    "instruction": t.instruction,
                    "fixture_hash": digest(t.files),
                    "oracle_hash": digest(
                        {
                            "answer": t.answer,
                            "final_files": t.final_files,
                            "writes": t.expected_writes,
                        }
                    ),
                }
                for t in tasks
            ]
        ),
        "statistically_independent": False,
        "secret_or_unseen": False,
    }
