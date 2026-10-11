#!/usr/bin/env python3
"""Prove that two synthetic local commits retain exactly their owners on restart.

Only a caller-supplied, isolated journal is read. A no-account QA helper,
never a migration tool; neither journal bodies nor credential paths are logged.
"""
import json
from pathlib import Path
import sys

if len(sys.argv) != 6 or sys.argv[1] not in ("snapshot", "verify"):
    raise SystemExit(
        "usage: check_replay.py snapshot|verify JOURNAL SNAPSHOT MARKER1 MARKER2"
    )
mode, journal_path, snapshot_path, *markers = sys.argv[1:]
journal, snapshot = Path(journal_path), Path(snapshot_path)
if not journal.is_file():
    raise AssertionError("isolated production journal was not created")
owners = {marker: [] for marker in markers}
total_rows = 0
for line in journal.read_text().splitlines():
    total_rows += 1
    event = json.loads(line)  # truncated/corrupt events fail QA rather than being ignored
    kind = "".join(x.lower() for x in str(event.get("kind", "")) if x.isalnum())
    if kind != "usermessagecommitted":
        continue
    payload = event.get("payload")
    if isinstance(payload, str):
        payload = json.loads(payload)
    if not isinstance(payload, dict):
        raise AssertionError("typed user commit payload is not an object")
    if payload.get("schema") != "chatarium-user-message-commit":
        continue
    if payload.get("text") in owners:
        owners[payload["text"]].append(payload.get("conversation_id"))
if any(len(owners[marker]) != 1 or not owners[marker][0] for marker in markers):
    raise AssertionError("each synthetic message must have one durable owner")
if owners[markers[0]][0] == owners[markers[1]][0]:
    raise AssertionError("both synthetic conversation commits share one owner")
result = {"schema": "chatarium-production-restart-ownership", "version": 1,
          "owners": {k: v[0] for k, v in owners.items()},
          "journal_rows": total_rows}
if mode == "snapshot":
    snapshot.write_text(json.dumps(result, indent=2) + "\n")
    print("CHATARIUM_PRODUCTION_BEFORE_RESTART PASS")
else:
    before = json.loads(snapshot.read_text())
    if result["owners"] != before["owners"]:
        raise AssertionError("durable message owner changed across production process restart")
    print("CHATARIUM_PRODUCTION_REPLAY_OWNERSHIP PASS")
