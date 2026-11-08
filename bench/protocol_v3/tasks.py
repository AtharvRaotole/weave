"""Public development and candidate held-out fixtures. Never include oracles in prompts.

These small related fixtures are NOT statistically independent or genuinely
secret/unseen. Freeze a reviewed manifest before future live evaluation.
"""

from .core import Task, digest

DEVELOPMENT = (
    Task(
        "dev_sum",
        "development",
        "parallel_sum",
        "Read a.txt and b.txt; answer their integer sum.",
        {"a.txt": "11", "b.txt": "20"},
        "31",
        {"a.txt": "11", "b.txt": "20"},
    ),
    Task(
        "dev_fallback",
        "development",
        "missing_fallback",
        "Read missing.txt; on failure read backup.txt. Answer its content.",
        {"backup.txt": "safe-backup"},
        "safe-backup",
        {"backup.txt": "safe-backup"},
    ),
    Task(
        "dev_receipt",
        "development",
        "receipt_write",
        "Read item.txt; write receipt.txt with exactly RECEIVED followed by a space and the item. Answer recorded.",
        {"item.txt": "blue"},
        "recorded",
        {"item.txt": "blue", "receipt.txt": "RECEIVED blue"},
        (("receipt.txt", "RECEIVED blue"),),
    ),
)
HELD_OUT = (
    Task(
        "hold_inventory",
        "held_out_candidate",
        "inventory_threshold",
        "List files, read each .stock file. Answer the number with integer stock below 5. Do not write.",
        {"ax.stock": "8", "by.stock": "2", "cz.stock": "4", "ignore.txt": "0"},
        "2",
        {"ax.stock": "8", "by.stock": "2", "cz.stock": "4", "ignore.txt": "0"},
        aliases={
            "read_file": "load_blob",
            "write_file": "store_blob",
            "list_files": "enumerate_blobs",
        },
    ),
    Task(
        "hold_journal",
        "held_out_candidate",
        "journal_preservation",
        "Read journal.txt and update it by appending exactly closed followed by a newline, preserving existing content. Answer the new line count.",
        {"journal.txt": "open\n"},
        "2",
        {"journal.txt": "open\nclosed\n"},
        (("journal.txt", "open\nclosed\n"),),
    ),
    Task(
        "hold_noop",
        "held_out_candidate",
        "negative_write",
        "Read policy.txt; if it says deny, change nothing and answer denied.",
        {"policy.txt": "deny"},
        "denied",
        {"policy.txt": "deny"},
    ),
)


def validate_splits(dev=DEVELOPMENT, held=HELD_OUT):
    for attr in ("id", "family", "instruction"):
        if {getattr(t, attr).strip().lower() for t in dev} & {
            getattr(t, attr).strip().lower() for t in held
        }:
            raise ValueError(f"cross-split duplicate {attr}")
    if {digest(t.files) for t in dev} & {digest(t.files) for t in held}:
        raise ValueError("cross-split duplicate fixture")
    if any(t.split != "development" for t in dev) or any(
        t.split != "held_out_candidate" for t in held
    ):
        raise ValueError("incorrect split label")
    return {
        "development": [t.id for t in dev],
        "held_out_candidate": [t.id for t in held],
        "manifest_hash": digest(
            [
                {
                    "id": t.id,
                    "split": t.split,
                    "family": t.family,
                    "instruction": t.instruction,
                    "fixture_hash": digest(t.files),
                }
                for t in (*dev, *held)
            ]
        ),
        "statistically_independent": False,
        "secret_or_unseen": False,
    }
