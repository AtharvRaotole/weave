"""POSIX append-only, fsync'd trial ledger. Local durability, not a WORM service."""

from __future__ import annotations
import fcntl
import hashlib
import json
import os
from pathlib import Path
import stat
import time
from .core import canonical, Usage


class TrialLedger:
    def __init__(self, path):
        self.path = Path(path)
        self.path.parent.mkdir(parents=True, exist_ok=True)

    @staticmethod
    def decode(raw):
        records, damaged = [], []
        for index, line in enumerate(raw.splitlines()):
            try:
                record = json.loads(line)
                checksum = record.pop("checksum")
                if record["seq"] != len(records) or record["previous"] != (
                    records[-1]["checksum"] if records else None
                ):
                    raise ValueError("ledger chain mismatch")
                if hashlib.sha256(canonical(record).encode()).hexdigest() != checksum:
                    raise ValueError("ledger checksum mismatch")
                record["checksum"] = checksum
                records.append(record)
            except (ValueError, KeyError, TypeError):
                # Preserve the raw damaged/truncated line, never truncate history.
                damaged.append(index)
        return records, damaged

    def append(self, trial_id, event, payload):
        fd = os.open(self.path, os.O_CREAT | os.O_RDWR | os.O_APPEND, 0o600)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX)
            if stat.S_IMODE(os.fstat(fd).st_mode) & 0o077:
                raise ValueError("ledger must be private (mode 0600)")
            chunks = []
            os.lseek(fd, 0, os.SEEK_SET)
            while chunk := os.read(fd, 65536):
                chunks.append(chunk)
            raw = b"".join(chunks)
            records, damaged = self.decode(raw)
            if event == "trial_begin" and any(
                r["trial_id"] == trial_id and r["event"] == "trial_begin"
                for r in records
            ):
                raise ValueError("duplicate trial id")
            if event == "trial_end" and any(
                r["trial_id"] == trial_id and r["event"] == "trial_end" for r in records
            ):
                raise ValueError("duplicate terminal trial record")
            record = {
                "recorded_at_ns": time.time_ns(),
                "seq": len(records),
                "previous": records[-1]["checksum"] if records else None,
                "trial_id": trial_id,
                "event": event,
                "payload": payload,
            }
            record["checksum"] = hashlib.sha256(canonical(record).encode()).hexdigest()
            encoded = canonical(record).encode() + b"\n"
            if len(encoded) > 8 * 1024 * 1024:
                raise ValueError("ledger record exceeds 8 MiB")
            if raw and not raw.endswith(b"\n"):
                encoded = b"\n" + encoded
            while encoded:
                written = os.write(fd, encoded)
                if written <= 0:
                    raise OSError("ledger append failed")
                encoded = encoded[written:]
            os.fsync(fd)
            directory = os.open(self.path.parent, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
        finally:
            os.close(fd)

    def records(self):
        if not self.path.exists():
            return [], []
        with self.path.open("rb") as stream:
            fcntl.flock(stream, fcntl.LOCK_SH)
            return self.decode(stream.read())

    def begin(self, trial_id, payload):
        self.append(trial_id, "trial_begin", payload)

    def terminated(self, trial_id, status, *, exit_code=None):
        if status not in ("crashed", "interrupted"):
            raise ValueError("explicit termination status required")
        self.append(trial_id, "trial_end", {"status": status, "exit_code": exit_code})

    def trials(self, *, recover_incomplete=False):
        records, damaged = self.records()
        groups = {}
        for record in records:
            groups.setdefault(record["trial_id"], []).append(record)
        rows = []
        for trial_id, events in groups.items():
            if not any(e["event"] == "trial_begin" for e in events):
                continue
            terminal = next(
                (e["payload"] for e in reversed(events) if e["event"] == "trial_end"),
                None,
            )
            trial_start = next(
                e["payload"] for e in events if e["event"] == "trial_begin"
            )
            starts = [e for e in events if e["event"] == "attempt_begin"]
            ends = {}
            for e in events:
                if e["event"] in ("http_response", "attempt_end"):
                    index = e["payload"]["attempt_index"]
                    try:
                        value = Usage.validate(e["payload"].get("usage"))
                    except ValueError:
                        value = None
                    if index not in ends or value is not None:
                        ends[index] = {"usage": value}
            known = {"input_tokens": 0, "output_tokens": 0, "cached_input_tokens": 0}
            unknown = 0
            for start in starts:
                usage = ends.get(start["payload"]["attempt_index"], {}).get("usage")
                if usage is None:
                    unknown += 1
                else:
                    for key in known:
                        known[key] += usage[key]
            rows.append(
                {
                    "trial_id": trial_id,
                    "status": terminal["status"]
                    if terminal
                    else ("interrupted" if recover_incomplete else "incomplete"),
                    "terminal_recorded": terminal is not None,
                    "task": trial_start.get("task"),
                    "mode": trial_start.get("mode"),
                    "config": trial_start.get("config"),
                    "passed": bool(terminal and terminal.get("passed", False)),
                    "wall_s": terminal.get("wall_s") if terminal else None,
                    "wall_time_complete": bool(
                        terminal and terminal.get("wall_s") is not None
                    ),
                    "known_tokens": known,
                    "unknown_usage_attempts": unknown,
                    "token_totals_complete": unknown == 0,
                    "row": terminal,
                    "damaged_lines": damaged,
                }
            )
        return rows
