#!/usr/bin/env python3
"""Validate actual screenshots without OCR, fonts, account data or snapshots."""
from pathlib import Path
from PIL import Image, ImageChops
import json
import sys

folder = Path(sys.argv[1])
labels = ("top", "middle", "wheel", "bottom", "search", "resized")
images = {}
for label in labels:
    src = folder / f"{label}.png"
    state = json.loads((folder / f"{label}.json").read_text())
    with Image.open(src) as raw:
        image = raw.convert("RGB")
    if image.width < 850 or image.height < 550:
        raise AssertionError(f"small/incomplete X11 screenshot {label}: {image.size}")
    colors = image.resize((180, 120)).getcolors(maxcolors=180 * 120)
    if colors is None or len(colors) < 20:
        raise AssertionError(f"blank or implausibly uniform screenshot: {label}")
    if state["rendered_count"] <= 0 or state["rendered_count"] >= 48:
        raise AssertionError(f"unbounded/missing virtual rows: {label}")
    images[label] = image

for before, after in (("top", "middle"), ("middle", "wheel"), ("middle", "bottom"), ("bottom", "search")):
    left, right = images[before], images[after]
    area = (0, 100, min(left.width, right.width), min(left.height, right.height) - 15)
    delta = ImageChops.difference(left.crop(area), right.crop(area))
    if delta.getbbox() is None:
        raise AssertionError(f"native transcript screenshot did not change: {before} -> {after}")

wide = images["search"].width
narrow = images["resized"].width
if narrow >= wide - 100:
    raise AssertionError(f"X11 resize not reflected in screenshots: {wide} -> {narrow}")
print("CHATARIUM_NATIVE_READER_SCREENSHOT_CHECK PASS: six nonblank, different X11 screenshots")
