#!/usr/bin/env python3
"""Prove the actual production transcript is displaying a new empty chat.

No OCR or user journal access. Both screenshots come from the same
isolated Xvfb session, with the app's first clean launch as reference.
"""
from pathlib import Path
from PIL import Image, ImageChops, ImageStat
import sys

if len(sys.argv) != 3:
    raise SystemExit("usage: check_empty_conversation.py INITIAL.png CANDIDATE.png")
before, after = [Image.open(Path(name)).convert("RGB") for name in sys.argv[1:]]
if before.size != after.size or before.width < 1100 or before.height < 700:
    raise AssertionError("production empty conversation reference size mismatch")
area = (480, 0, 1150, 470)
difference = ImageChops.difference(before.crop(area), after.crop(area))
mean = sum(ImageStat.Stat(difference).mean) / 3
# Successful UI creation is stable at 0.0 on the Xvfb runner; a missed
# shortcut has ~6.2 RGB intensity units of difference from the empty view.
if mean >= 0.8:
    print(f"CHATARIUM_PRODUCTION_NEW_CHAT_NOT_SELECTED {mean:.4f}", file=sys.stderr)
    raise SystemExit(1)
print(f"CHATARIUM_PRODUCTION_NEW_CHAT_SELECTED {mean:.4f}")
