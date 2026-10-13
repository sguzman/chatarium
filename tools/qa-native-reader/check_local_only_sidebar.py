#!/usr/bin/env python3
"""Verify native-only Chatarium's local New action stays above a seeded legacy account catalog.

No OCR, remote account, personal screenshot or login; CI uses isolated Xvfb.
The legacy archive search previously pushed the New local button far below the
viewport, replacing this visible button with a dark archive-search text field.
"""
from pathlib import Path
import sys
from PIL import Image


def assert_native_only_sidebar(image):
    if image.width != 1180 or image.height != 760:
        raise AssertionError(f"unexpected production QA geometry: {image.size}")
    # Pixel samples inside the real, production-styled New local button.
    # Account-catalog UI instead paints an almost-black search box here.
    points = ((30, 132), (34, 143), (88, 143))
    brightness = [
        sum(image.getpixel(point)[:3]) / 3
        for point in points
    ]
    if min(brightness) < 38:
        raise AssertionError(
            f"native New local button hidden behind legacy account catalog: {brightness}"
        )


def self_test():
    clean = Image.new("RGB", (1180, 760), (18, 19, 23))
    for x, y in ((30, 132), (34, 143), (88, 143)):
        clean.putpixel((x, y), (60, 60, 60))
    assert_native_only_sidebar(clean)
    broken = Image.new("RGB", (1180, 760), (18, 19, 23))
    try:
        assert_native_only_sidebar(broken)
    except AssertionError:
        pass
    else:
        raise AssertionError("legacy archive appearance was not rejected")
    print("CHATARIUM_QA_LOCAL_ONLY_SIDEBAR_SELF_TEST PASS")


if __name__ == "__main__":
    if sys.argv[1:] == ["--self-test"]:
        self_test()
    elif len(sys.argv) == 2:
        with Image.open(Path(sys.argv[1])) as source:
            assert_native_only_sidebar(source.convert("RGB"))
        print("CHATARIUM_QA_LOCAL_ONLY_SIDEBAR PASS")
    else:
        raise SystemExit("usage: check_local_only_sidebar.py --self-test|SCREENSHOT.png")
