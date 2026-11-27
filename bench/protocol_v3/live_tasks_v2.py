"""Frozen live task set ``live-v2`` (round 15) for the shipped weave-mcp fixture.

24 maintainer-authored tasks, 6 per family: fan-out reads, dependent lookups, writes with a precondition, and error
handling. Written and hashed before any live call; the manifest hash is in docs/paper/preregistration.md
(Addendum 15). Same fixture as ``live-v1`` (``local_mcp.RemoteFixture``: fs_read, fs_write, git_head, git_commit,
memory_read, memory_put; a failed call is an opaque "tool request failed", never an error value).

Grading is ``Task.grade``: the exact answer, the final files, git HEAD, tracked files and memory, the ordered log of
successful writes, AND every write attempt (a failed or extra write fails the task). Tasks are public, small and
related: NOT secret and NOT statistically independent. ``REFERENCE`` gives one correct call sequence per task;
``tests/test_live_v2.py`` runs each against the real fixture and checks that it grades as a pass (no model).
"""

from .core import Task, canonical, digest
from .live_tasks import BASELINE_HEAD

FAMILIES = ("fanout", "dependent", "precondition", "errors")


def _task(id, family, instruction, files, answer, writes=(), *, head=BASELINE_HEAD, memory=None):
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
    return Task(id, "live_v2", family, instruction, files, answer, final, tuple(writes))


LIVE_V2 = (
    # Fan-out reads: independent reads, then one computed answer.
    _task("sum4", "fanout",
          "Read w.txt, x.txt, y.txt and z.txt. Each holds one integer. Answer with only their sum as an integer.",
          {"w.txt": "7", "x.txt": "15", "y.txt": "-3", "z.txt": "30"}, "49"),
    _task("top_region", "fanout",
          "Read north.txt, south.txt, east.txt and west.txt. Each holds one integer. Answer with only the name of the "
          "region (the file name without .txt) with the highest number.",
          {"north.txt": "120", "south.txt": "340", "east.txt": "95", "west.txt": "210"}, "south"),
    _task("concat3", "fanout",
          "Read p1.txt, p2.txt and p3.txt. Each holds one word. Answer with only the three words in that file order, "
          "joined by hyphens.",
          {"p1.txt": "alpha", "p2.txt": "beta", "p3.txt": "gamma"}, "alpha-beta-gamma"),
    _task("count_yes", "fanout",
          "Read vote1.txt, vote2.txt, vote3.txt, vote4.txt and vote5.txt. Each says yes or no. Answer with only the "
          "number of yes votes as an integer.",
          {"vote1.txt": "yes", "vote2.txt": "no", "vote3.txt": "yes", "vote4.txt": "yes", "vote5.txt": "no"}, "3"),
    _task("head_vs_file", "fanout",
          "Read version.txt and the subject of the current git HEAD commit. Answer with only yes if the file's content "
          "equals the HEAD subject exactly, otherwise only no.",
          {"version.txt": "release 2"}, "no"),
    _task("average4", "fanout",
          "Read t1.txt, t2.txt, t3.txt and t4.txt. Each holds one integer. Answer with only their average as an "
          "integer.",
          {"t1.txt": "10", "t2.txt": "20", "t3.txt": "30", "t4.txt": "40"}, "25"),
    # Dependent lookups: each read needs the result of an earlier one.
    _task("chain5", "dependent",
          "Read begin.txt. Its content is the name of the next file to read. Keep following file names this way until "
          "a file's content begins with END: and answer with only the text after END:.",
          {"begin.txt": "n1.txt", "n1.txt": "n2.txt", "n2.txt": "n3.txt", "n3.txt": "n4.txt",
           "n4.txt": "END:orchid"}, "orchid"),
    _task("index_min", "dependent",
          "Read index.txt; it lists file names, one per line. Read every listed file; each holds one integer score. "
          "Answer with only the file name that has the lowest score.",
          {"index.txt": "s1.txt\ns2.txt\ns3.txt\ns4.txt\ns5.txt", "s1.txt": "55", "s2.txt": "12", "s3.txt": "80",
           "s4.txt": "31", "s5.txt": "47"}, "s2.txt"),
    _task("pointer_sum", "dependent",
          "Read pointer.txt; it names a file that lists other file names, one per line. Read each of those files "
          "(each holds one integer) and answer with only their sum as an integer.",
          {"pointer.txt": "list.txt", "list.txt": "q1.txt\nq2.txt\nq3.txt", "q1.txt": "4", "q2.txt": "9",
           "q3.txt": "16", "q4.txt": "100"}, "29"),
    _task("owner_lookup", "dependent",
          "Read owners.txt; each line is name:file. Read the file listed for bob and answer with only its content.",
          {"owners.txt": "alice:a_notes.txt\nbob:b_notes.txt\ncarol:c_notes.txt", "a_notes.txt": "ship monday",
           "b_notes.txt": "launch friday", "c_notes.txt": "hold"}, "launch friday"),
    _task("two_level", "dependent",
          "Read dir.txt; it names a folder. Read key.txt inside that folder (folder/key.txt); it names another file in "
          "the same folder. Read that file and answer with only its content.",
          {"dir.txt": "vault", "vault/key.txt": "secret.txt", "vault/secret.txt": "42-amber",
           "vault/other.txt": "decoy"}, "42-amber"),
    _task("best_file", "dependent",
          "Read scores.txt; each line is file:score. Read the file with the highest score and answer with only its "
          "content.",
          {"scores.txt": "m1.txt:3\nm2.txt:9\nm3.txt:5", "m1.txt": "low", "m2.txt": "winner-m2", "m3.txt": "mid"},
          "winner-m2"),
    # Writes with a precondition: write only when a value read first allows it.
    _task("gate_open", "precondition",
          "Read gate.txt. If it says open, write done.txt containing yes and answer with only the word written; "
          "otherwise change nothing and answer with only the word skipped.",
          {"gate.txt": "open"}, "written", (("fs_write:done.txt", "yes"),)),
    _task("gate_closed", "precondition",
          "Read gate.txt. If it says open, write done.txt containing yes and answer with only the word written; "
          "otherwise change nothing and answer with only the word skipped.",
          {"gate.txt": "closed"}, "skipped"),
    _task("reorder", "precondition",
          "Read stock.txt; it holds one integer. If it is below 10, store the text reorder under memory key Stock; "
          "otherwise store ok under that key. Store nothing else. Answer with only the text you stored.",
          {"stock.txt": "7"}, "reorder", (("memory_put:Stock", "reorder"),), memory={"Stock": ["reorder"]}),
    _task("publish_if_match", "precondition",
          "Read draft.txt and expected.txt. If their contents are identical, write final.txt with that content, commit "
          "final.txt with the message Publish final, and answer with only the word published. If they differ, change "
          "nothing and answer with only the word mismatch.",
          {"draft.txt": "v3 text", "expected.txt": "v3 text"}, "published",
          (("fs_write:final.txt", "v3 text"), ("git_commit:final.txt", "Publish final")), head="Publish final"),
    _task("append_if_absent", "precondition",
          "list.txt holds one name per line and ends with a newline. If dave is not already in it, append the line "
          "dave (followed by a newline), preserving the existing content exactly. Answer with only the number of lines "
          "in the file afterwards.",
          {"list.txt": "amy\nben\n"}, "3", (("fs_write:list.txt", "amy\nben\ndave\n"),)),
    _task("total_if_large", "precondition",
          "Read a1.txt and a2.txt; each holds one integer. If their sum is greater than 100, write total.txt "
          "containing only the sum; otherwise change nothing. Answer with only the sum as an integer.",
          {"a1.txt": "64", "a2.txt": "58"}, "122", (("fs_write:total.txt", "122"),)),
    # Error handling: a call fails (the file does not exist) and the task says what to do then.
    _task("fallback3", "errors",
          "Read primary.txt; if that read fails, read secondary.txt; if that fails too, read tertiary.txt. Answer with "
          "only the exact content you obtained.",
          {"tertiary.txt": "third-ok"}, "third-ok"),
    _task("copy_missing", "errors",
          "Read config.txt and write copy.txt with exactly its content, then answer with only the word copied. If "
          "config.txt cannot be read, write nothing and answer with only the word missing.",
          {"readme.txt": "fixture"}, "missing"),
    _task("partial_sum", "errors",
          "Read n1.txt, n2.txt, n3.txt and n4.txt. Some may not exist; skip any that cannot be read. Answer with only "
          "the sum of the integers you could read.",
          {"n1.txt": "5", "n3.txt": "12", "n4.txt": "-2"}, "15"),
    _task("create_then_commit", "errors",
          "Read notes.txt. If it cannot be read, write notes.txt containing saved. Then commit notes.txt with the "
          "message Save notes. Answer with only the subject of the git HEAD commit at the end.",
          {"readme.txt": "fixture"}, "Save notes",
          (("fs_write:notes.txt", "saved"), ("git_commit:notes.txt", "Save notes")), head="Save notes"),
    _task("memory_fallback", "errors",
          "Read target.txt; its content is a memory key. Store the text done under that key. If target.txt cannot be "
          "read, store done under the key Fallback instead. Answer with only the key you used.",
          {"readme.txt": "fixture"}, "Fallback", (("memory_put:Fallback", "done"),), memory={"Fallback": ["done"]}),
    _task("compare_missing", "errors",
          "Read left.txt and right.txt. If either cannot be read, change nothing and answer with only the word "
          "incomplete. Otherwise write same.txt containing yes if they are equal or no if not, and answer with only "
          "that word.",
          {"left.txt": "same"}, "incomplete"),
)

# One correct call sequence per task: (tool, arguments) in order, then the answer. Used only by the offline test.
REFERENCE = {
    "sum4": ([("fs_read", {"path": p}) for p in ("w.txt", "x.txt", "y.txt", "z.txt")], "49"),
    "top_region": ([("fs_read", {"path": p + ".txt"}) for p in ("north", "south", "east", "west")], "south"),
    "concat3": ([("fs_read", {"path": p}) for p in ("p1.txt", "p2.txt", "p3.txt")], "alpha-beta-gamma"),
    "count_yes": ([("fs_read", {"path": f"vote{i}.txt"}) for i in range(1, 6)], "3"),
    "head_vs_file": ([("fs_read", {"path": "version.txt"}), ("git_head", {})], "no"),
    "average4": ([("fs_read", {"path": f"t{i}.txt"}) for i in range(1, 5)], "25"),
    "chain5": ([("fs_read", {"path": p}) for p in ("begin.txt", "n1.txt", "n2.txt", "n3.txt", "n4.txt")], "orchid"),
    "index_min": ([("fs_read", {"path": "index.txt"})] + [("fs_read", {"path": f"s{i}.txt"}) for i in range(1, 6)],
                  "s2.txt"),
    "pointer_sum": ([("fs_read", {"path": p}) for p in ("pointer.txt", "list.txt", "q1.txt", "q2.txt", "q3.txt")],
                    "29"),
    "owner_lookup": ([("fs_read", {"path": "owners.txt"}), ("fs_read", {"path": "b_notes.txt"})], "launch friday"),
    "two_level": ([("fs_read", {"path": p}) for p in ("dir.txt", "vault/key.txt", "vault/secret.txt")], "42-amber"),
    "best_file": ([("fs_read", {"path": "scores.txt"}), ("fs_read", {"path": "m2.txt"})], "winner-m2"),
    "gate_open": ([("fs_read", {"path": "gate.txt"}), ("fs_write", {"path": "done.txt", "text": "yes"})], "written"),
    "gate_closed": ([("fs_read", {"path": "gate.txt"})], "skipped"),
    "reorder": ([("fs_read", {"path": "stock.txt"}), ("memory_put", {"path": "Stock", "text": "reorder"})],
                "reorder"),
    "publish_if_match": ([("fs_read", {"path": "draft.txt"}), ("fs_read", {"path": "expected.txt"}),
                          ("fs_write", {"path": "final.txt", "text": "v3 text"}),
                          ("git_commit", {"path": "final.txt", "text": "Publish final"})], "published"),
    "append_if_absent": ([("fs_read", {"path": "list.txt"}),
                          ("fs_write", {"path": "list.txt", "text": "amy\nben\ndave\n"})], "3"),
    "total_if_large": ([("fs_read", {"path": "a1.txt"}), ("fs_read", {"path": "a2.txt"}),
                        ("fs_write", {"path": "total.txt", "text": "122"})], "122"),
    "fallback3": ([("fs_read", {"path": p}) for p in ("primary.txt", "secondary.txt", "tertiary.txt")], "third-ok"),
    "copy_missing": ([("fs_read", {"path": "config.txt"})], "missing"),
    "partial_sum": ([("fs_read", {"path": f"n{i}.txt"}) for i in range(1, 5)], "15"),
    "create_then_commit": ([("fs_read", {"path": "notes.txt"}), ("fs_write", {"path": "notes.txt", "text": "saved"}),
                            ("git_commit", {"path": "notes.txt", "text": "Save notes"}), ("git_head", {})],
                           "Save notes"),
    "memory_fallback": ([("fs_read", {"path": "target.txt"}), ("memory_put", {"path": "Fallback", "text": "done"})],
                        "Fallback"),
    "compare_missing": ([("fs_read", {"path": "left.txt"}), ("fs_read", {"path": "right.txt"})], "incomplete"),
}


def live_v2_manifest(tasks=LIVE_V2):
    return {
        "task_set": "live-v2",
        "tasks": [t.id for t in tasks],
        "manifest_hash": digest([
            {"id": t.id, "family": t.family, "instruction": t.instruction, "fixture_hash": digest(t.files),
             "oracle_hash": digest({"answer": t.answer, "final_files": t.final_files, "writes": t.expected_writes})}
            for t in tasks
        ]),
        "statistically_independent": False,
        "secret_or_unseen": False,
    }
