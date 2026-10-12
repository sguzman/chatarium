#!/usr/bin/env python3
"""Verify that both the selected conversation title and transcript repainted.

Only operates on disposable synthetic Chatarium GUI QA screenshots. Require
independent changes in title and message regions, not a mean over blank pixels.
"""
import sys
from pathlib import Path
from PIL import Image, ImageChops


def changed_pixels(left, right, region, contrast=30):
    delta = ImageChops.difference(left.crop(region), right.crop(region)).convert("L")
    return sum(delta.histogram()[contrast + 1:])


def assess(before, after):
    if before.size != after.size:
        return (False, 0, 0)
    width, height = before.size
    if width < 710 or height < 300:
        return (False, 0, 0)
    # The fixed sidebar ends before x=460. Compare the title and first visible
    # message independently, excluding the status bar, composer and cursor.
    right = width - 20
    header = (480, 5, right, 65)
    bubble = (480, 88, right, min(230, height))
    title_count = changed_pixels(before, after, header)
    body_count = changed_pixels(before, after, bubble)
    return (title_count >= 100 and body_count >= 80, title_count, body_count)


def self_test():
    from PIL import ImageDraw

    first = Image.new("RGB", (1180, 760), (22, 23, 29))
    second = first.copy()
    for image, title, message in [
        (first, "First conversation", "CHATARIUM_QA_FIRST_314159"),
        (second, "Second conversation", "CHATARIUM_QA_SECOND_271828"),
    ]:
        draw = ImageDraw.Draw(image)
        draw.text((485, 15), title, fill=(240, 240, 240))
        draw.text((650, 115), message, fill=(230, 230, 230))
    matched, header, body = assess(first, second)
    assert matched, (header, body)
    matched, header, body = assess(first, first)
    assert not matched and header == 0 and body == 0
    cosmetic = first.copy()
    ImageDraw.Draw(cosmetic).rectangle((1000, 600, 1100, 650), fill="white")
    assert not assess(first, cosmetic)[0]
    only_title = first.copy()
    ImageDraw.Draw(only_title).text((550, 15), "More words", fill="white")
    assert not assess(first, only_title)[0]
    print("CHATARIUM_QA_SWITCH_CHECKER_SELF_TEST PASS")


def main():
    if sys.argv[1:] == ["--self-test"]:
        self_test()
        return 0
    if len(sys.argv) != 3:
        print("usage: check_conversation_switch.py before.png after.png", file=sys.stderr)
        return 2
    with Image.open(Path(sys.argv[1])) as initial, Image.open(Path(sys.argv[2])) as final:
        matched, header, body = assess(initial.convert("RGB"), final.convert("RGB"))
    print(
        f"CHATARIUM_QA_SWITCH_REPAINT title_changed_pixels={header} "
        f"bubble_changed_pixels={body} pass={str(matched).lower()}"
    )
    return 0 if matched else 1


if __name__ == "__main__":
    sys.exit(main())
