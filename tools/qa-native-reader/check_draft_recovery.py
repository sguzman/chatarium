#!/usr/bin/env python3
"""QA-only: assert independent unsent drafts survive navigation and restart.

Only inspect a caller-supplied disposable journal and synthetic screenshots.
Never emit draft text or local identifiers into logs.
"""
import json
from pathlib import Path
import sys
from PIL import Image, ImageChops


def events(path):
    if not path.is_file():
        raise AssertionError("no isolated draft journal")
    return [json.loads(line) for line in path.read_text().splitlines()]


def kind(event):
    return "".join(ch.lower() for ch in str(event.get("kind", "")) if ch.isalnum())


def draft_scopes(rows, marker):
    return [
        event.get("scope")
        for event in rows
        if kind(event) == "draftchanged" and marker in json.dumps(event)
        and isinstance(event.get("scope"), str)
        and event["scope"].startswith("local-conversation:")
    ]


def assert_uncommitted(rows, marker):
    if any(kind(event) == "usermessagecommitted" and marker in json.dumps(event) for event in rows):
        raise AssertionError("synthetic unsent draft unexpectedly committed")


def draft_owner(rows, marker):
    scopes = draft_scopes(rows, marker)
    if not scopes or len(set(scopes)) != 1:
        raise AssertionError("unsent draft lacks unique durable owner")
    assert_uncommitted(rows, marker)
    return scopes[-1]


def region_image(path):
    with Image.open(path) as source:
        image = source.convert("RGB")
    if image.size != (1180, 760):
        raise AssertionError("draft QA window geometry changed unexpectedly")
    # Real native compositor editor: excludes sidebar, transcript and status.
    return image.crop((492, 595, 1140, 663))


def changed_count(left, right):
    if left.size != right.size:
        raise AssertionError("incompatible draft screenshot geometry")
    delta = ImageChops.difference(left, right).convert("L")
    return sum(delta.histogram()[41:])


def verify_images(out):
    labels = ("empty", "alpha", "second-empty", "bravo", "alpha-return",
              "bravo-return", "bravo-restarted", "alpha-restarted")
    images = {label: region_image(out / (label + ".png")) for label in labels}
    a = images["alpha"]
    b = images["bravo"]
    gap = changed_count(a, b)
    empty_gap_a = changed_count(images["empty"], a)
    empty_gap_b = changed_count(images["second-empty"], b)
    if min(gap, empty_gap_a, empty_gap_b) < 100:
        raise AssertionError("two distinct draft texts were not visibly painted")
    # Pixel-level caret/focus differences are tolerated, but A and B must not
    # be mistaken for one another. No OCR, user file or account access.
    comparisons = {}
    for reference, observations in (
        ("alpha", ("alpha-return", "alpha-restarted")),
        ("bravo", ("bravo-return", "bravo-restarted")),
    ):
        for observed in observations:
            pixels = changed_count(images[reference], images[observed])
            comparisons[observed] = pixels
            if pixels >= gap * 0.65:
                raise AssertionError(f"draft not visually restored: {observed}, pixels={pixels}")
    return {"draft_visual_gap_pixels": gap, "replay_changed_pixels": comparisons,
            "screenshots": len(labels)}


def distinct_empty(args):
    first, second, authored = map(Path, args)
    original = region_image(first)
    blank = region_image(second)
    typed = region_image(authored)
    # The new conversation must contain the original blank editor,
    # not an A draft left behind by a missed shortcut.
    if changed_count(blank, original) >= changed_count(typed, original) * 0.6:
        raise AssertionError("new conversation did not present an empty editor")
    print("CHATARIUM_DRAFT_SECOND_EMPTY PASS")


def main(args):
    if len(args) == 3 and args[0] == "saved":
        draft_owner(events(Path(args[1])), args[2])
        return
    if len(args) == 4 and args[0] == "distinct-empty":
        distinct_empty(args[1:])
        return
    if len(args) == 4 and args[0] == "owners":
        rows = events(Path(args[1]))
        if draft_owner(rows, args[2]) == draft_owner(rows, args[3]):
            raise AssertionError("unsent drafts leaked into the same conversation")
        print("CHATARIUM_DRAFT_SCOPED_OWNERS PASS")
        return
    if len(args) == 4 and args[0] == "verify":
        out = Path(args[1])
        rows = events(out / "data/journal.jsonl")
        scopes = [draft_owner(rows, marker) for marker in args[2:]]
        if len(set(scopes)) != 2:
            raise AssertionError("draft conversation isolation lost across process restart")
        visual = verify_images(out)
        result = {
            "schema": "chatarium-durable-draft-recovery-qa",
            "version": 1,
            "status": "pass",
            "separate_conversation_owners": True,
            "unsent_messages_remained_uncommitted": True,
            "same_drafts_visible_after_switch_and_restart": True,
            "isolated_home_and_journal": True,
            **visual,
        }
        (out / "draft-recovery-summary.json").write_text(json.dumps(result, indent=2) + "\n")
        print("CHATARIUM_DRAFT_RECOVERY_QA " + json.dumps(result))
        return
    raise SystemExit("usage: check_draft_recovery.py saved|owners|distinct-empty|verify ...")


if __name__ == "__main__":
    main(sys.argv[1:])
