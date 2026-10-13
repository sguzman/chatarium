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
    # Display/advanced controls now live above CHATS. Search a bounded
    # sidebar band for the unchanged, narrow native New-chat button instead
    # of depending on its former fixed Y coordinate.
    # Require eight consecutive rows, distinguishing it from isolated text.
    consecutive = 0
    for y in range(120, min(460, image.height - 280)):
        button = [
            sum(image.getpixel((x, y))[:3]) / 3
            for x in (30, 50, 88)
        ]
        sidebar = sum(image.getpixel((120, y))[:3]) / 3
        if min(button) >= 38 and sidebar <= 35:
            consecutive += 1
            if consecutive >= 8:
                return
        else:
            consecutive = 0
    raise AssertionError(
        "native New local button not found within the bounded sidebar header band"
    )


def self_test():
    clean = Image.new("RGB", (1180, 760), (18, 19, 23))
    for y in range(270, 285):
        for x in range(20, 100):
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
