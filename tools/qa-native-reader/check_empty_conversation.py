#!/usr/bin/env python3
"""Verify the actual production transcript has selected a new empty chat.

No OCR or account data; both screenshots are from the same disposable
Xvfb session. The sidebar can widen after the first committed conversation,
which shifts the title by ~22 px and the centered empty-state text by ~11 px.
Compare those two regions *independently* with bounded horizontal alignment.
"""
from pathlib import Path
from PIL import Image, ImageChops, ImageStat
from panel_geometry import sidebar_edge
import sys

if len(sys.argv) != 3:
    raise SystemExit("usage: check_empty_conversation.py INITIAL.png CANDIDATE.png")
before, after = [Image.open(Path(name)).convert("RGB") for name in sys.argv[1:]]
if before.size != after.size or before.width < 1100 or before.height < 700:
    raise AssertionError("production empty conversation reference size mismatch")

# Creating a conversation must not silently drag the user-resizable splitter.
# This used to grow by 172 pixels simply because the previous title got longer.
before_edge, after_edge = sidebar_edge(before), sidebar_edge(after)
if abs(after_edge - before_edge) > 6:
    raise AssertionError(
        f"native sidebar changed width without a drag: {before_edge} -> {after_edge}"
    )


def best_horizontal_alignment(box):
    reference = before.crop(box)
    scores = []
    for shift in range(-45, 46):
        compare_box = (box[0] + shift, box[1], box[2] + shift, box[3])
        difference = ImageChops.difference(reference, after.crop(compare_box))
        scores.append((sum(ImageStat.Stat(difference).mean) / 3, shift))
    return min(scores)


# The selected title and empty-state prompt are in separately aligned UI
# layouts; one uniform image shift would wrongly reject genuine empty chats.
title_score, title_shift = best_horizontal_alignment((482, 0, 900, 70))
prompt_score, prompt_shift = best_horizontal_alignment((580, 155, 1070, 270))
if title_score >= 1.0 or prompt_score >= 1.0:
    print(
        f"CHATARIUM_PRODUCTION_NEW_CHAT_NOT_SELECTED "
        f"title={title_score:.3f}/{title_shift:+d}px "
        f"prompt={prompt_score:.3f}/{prompt_shift:+d}px",
        file=sys.stderr,
    )
    raise SystemExit(1)
print(
    f"CHATARIUM_PRODUCTION_NEW_CHAT_SELECTED "
    f"title={title_score:.3f}/{title_shift:+d}px "
    f"prompt={prompt_score:.3f}/{prompt_shift:+d}px"
)
