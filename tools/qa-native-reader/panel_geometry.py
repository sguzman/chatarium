#!/usr/bin/env python3
"""Find the real sidebar/content boundary in isolated Chatarium QA screenshots.

This deliberately relies on two stable panel background fills, not text/OCR
or hard-coded window coordinates. Fail closed when the UI cannot be located.
"""
from pathlib import Path
import sys
from PIL import Image

# Must match the native sidebar frame fill in apps/desktop/src/main.rs.
# The contrasting stroke is on the boundary, not sampled in the interior.
SIDEBAR = (20, 25, 34)
# Native center panel fill; boundary stroke is deliberately excluded.
CONTENT = (23, 27, 35)


def sidebar_edge(image):
    if image.width < 850 or image.height < 600:
        raise AssertionError(f"unsupported native QA screenshot size: {image.size}")
    # Check below either compact or expanded conversation headers. The live
    # connectivity row intentionally makes the header taller; a single y=110
    # probe would incorrectly sample that header instead of the reader.
    # The grey grab handle may sit immediately after the sidebar. Look beyond
    # it for the main-panel fill and require a solid sidebar run.
    for y in (140, 160, 190, 110):
        for x in range(260, min(650, image.width - 260)):
            if (
                all(image.getpixel((x - offset, y)) == SIDEBAR for offset in range(1, 7))
                and image.getpixel((x, y)) != SIDEBAR
                and image.getpixel((x + 40, y)) == CONTENT
            ):
                return x
    raise AssertionError("cannot locate Chatarium sidebar boundary; layout changed")


def main(argv):
    if len(argv) != 1:
        raise SystemExit("usage: panel_geometry.py SCREENSHOT.png")
    with Image.open(Path(argv[0])) as screenshot:
        x = sidebar_edge(screenshot.convert("RGB"))
    print(x)


if __name__ == "__main__":
    main(sys.argv[1:])
