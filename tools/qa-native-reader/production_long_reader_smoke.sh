#!/usr/bin/env bash
# Test the actual desktop app against an isolated 257-message durable journal.
set -euo pipefail
ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"
# Check display isolation before creating fixtures or launching a window manager.
source "$ROOT/tools/qa-native-reader/require_isolated_xvfb.sh"
OUT="$ROOT/.qa-native-long-reader"
if [[ -e "$OUT" || -L "$OUT" ]]; then
    echo "refusing to reuse an existing disposable long-reader fixture" >&2
    exit 1
fi
mkdir -p "$OUT"/{home,config,cache,runtime}
chmod 700 "$OUT/runtime"
"$ROOT/target/debug/chatarium-reader-seed" \
    .qa-native-long-reader/data/journal.jsonl >"$OUT/seed.json"
jq -e '.schema == "chatarium-long-native-reader-fixture" and .events == 257 and .disposable == true' "$OUT/seed.json" >/dev/null

# xvfb-run can expose DISPLAY slightly before the X server accepts clients.
# Do not launch openbox or winit until a real X11 query succeeds.
for _ in $(seq 1 100); do
    if xdotool getdisplaygeometry >/dev/null 2>&1; then
        break
    fi
    sleep 0.1
done
if ! xdotool getdisplaygeometry >/dev/null 2>&1; then
    echo "disposable Xvfb display failed to become ready" >&2
    exit 1
fi

openbox >"$OUT/openbox.log" 2>&1 &
wm_pid=$!
app_pid=""
cleanup() {
    if [[ -n "$app_pid" ]]; then
        kill "$app_pid" 2>/dev/null || true
        wait "$app_pid" 2>/dev/null || true
    fi
    kill "$wm_pid" 2>/dev/null || true
    wait "$wm_pid" 2>/dev/null || true
}
trap cleanup EXIT

activate_window() {
    local target="$1"
    # Never wait unboundedly for a wedged window manager to grant focus.
    for _ in $(seq 1 40); do
        xdotool windowactivate "$target" >/dev/null 2>&1 || true
        if [[ "$(xdotool getactivewindow 2>/dev/null || true)" == "$target" ]]; then
            return 0
        fi
        sleep 0.25
    done
    echo "native QA window activation timed out: $target" >&2
    return 1
}

env -i \
    PATH="$PATH" \
    HOME="$OUT/home" \
    XDG_CONFIG_HOME="$OUT/config" \
    XDG_CACHE_HOME="$OUT/cache" \
    XDG_DATA_HOME="$OUT/data" \
    XDG_RUNTIME_DIR="$OUT/runtime" \
    DISPLAY="$DISPLAY" \
    XAUTHORITY="$(printenv XAUTHORITY || true)" \
    LIBGL_ALWAYS_SOFTWARE=1 \
    CHATARIUM_DATA_DIR="$OUT/data" \
    RUST_BACKTRACE=1 \
    "$ROOT/target/debug/chatarium-desktop" >"$OUT/production.log" 2>&1 &
app_pid=$!
window=""
for _ in $(seq 1 160); do
    window="$(xdotool search --onlyvisible --name '^Chatarium$' 2>/dev/null | head -n 1 || true)"
    if [[ -n "$window" ]]; then break; fi
    if ! kill -0 "$app_pid" 2>/dev/null; then
        echo "Native app could not load durable long-reader fixture" >&2
        tail -n 100 "$OUT/production.log" >&2 || true
        exit 1
    fi
    sleep 0.25
done
if [[ -z "$window" ]]; then
    echo "Native production window did not open" >&2
    tail -n 100 "$OUT/production.log" >&2 || true
    exit 1
fi
activate_window "$window"
sleep 1
import -window "$window" "$OUT/long-initial.png"
xdotool mousemove --window "$window" 740 250 click 1
# Startup screenshot can precede the initial sticky-bottom settle. Establish
# the tail reference through a real End key before testing navigation.
xdotool key --clearmodifiers End
sleep 0.8
import -window "$window" "$OUT/long-tail-reference.png"
xdotool key --clearmodifiers Home
sleep 0.8
import -window "$window" "$OUT/long-home.png"
xdotool key --clearmodifiers End
sleep 0.8
import -window "$window" "$OUT/long-end.png"

# The production control is available only for long archives. Switch it on
# through real X11 clicks rather than mutating egui state or faking a session.
# The sidebar is resizable and its size depends on the window and content.
# Click the checkbox relative to the *observed* production content boundary,
# never a stale absolute pixel from a different layout.
sidebar_edge="$(python3 tools/qa-native-reader/panel_geometry.py "$OUT/long-end.png")"
xdotool mousemove --window "$window" "$((sidebar_edge + 50))" 118 click 1
# Remove hover styling before comparing the persistent checkbox state.
xdotool mousemove --window "$window" 740 250
sleep 0.8
import -window "$window" "$OUT/long-continuous-enabled.png"
xdotool mousemove --window "$window" 740 250 click 1
xdotool key --clearmodifiers Home
sleep 0.8
import -window "$window" "$OUT/long-continuous-home.png"
xdotool key --clearmodifiers End
sleep 0.8
import -window "$window" "$OUT/long-continuous-end.png"

# Confirm the virtualized reader remains navigable after a genuine desktop
# resize, not merely inside the fixed-size synthetic reader QA shell.
xdotool windowsize "$window" 930 650
sleep 1
activate_window "$window"
xdotool key --clearmodifiers Home
sleep 0.8
import -window "$window" "$OUT/long-continuous-resized-home.png"
xdotool key --clearmodifiers End
sleep 0.8
import -window "$window" "$OUT/long-continuous-resized-end.png"

python3 - "$OUT" <<'PY'
from pathlib import Path
from PIL import Image, ImageChops
import json
import sys

out = Path(sys.argv[1])
fixture = json.loads((out / "seed.json").read_text())
journal = out / "data/journal.jsonl"
rows = [json.loads(line) for line in journal.read_text().splitlines()]
if len(rows) != 257:
    raise AssertionError(f"expected 257 durable rows, found {len(rows)}")
owners = set()
texts = []
for event in rows:
    kind = "".join(c.lower() for c in str(event["kind"]) if c.isalnum())
    if kind != "usermessagecommitted":
        raise AssertionError("unexpected event type in synthetic reader fixture")
    payload = event["payload"]
    if isinstance(payload, str):
        payload = json.loads(payload)
    if payload.get("schema") != "chatarium-user-message-commit":
        raise AssertionError("invalid typed message fixture")
    owners.add(payload["conversation_id"])
    texts.append(payload["text"])
if owners != {fixture["conversation_id"]}:
    raise AssertionError("synthetic long archive messages have mixed ownership")
if not texts[0].startswith(fixture["first_marker"]) or not texts[-1].startswith(fixture["last_marker"]):
    raise AssertionError("long fixture lost first or last marker")

images = {}
for label in ("initial", "tail-reference", "home", "end", "continuous-enabled", "continuous-home", "continuous-end", "continuous-resized-home", "continuous-resized-end"):
    with Image.open(out / f"long-{label}.png") as src:
        img = src.convert("RGB")
    if label.startswith("continuous-resized-"):
        if not (900 <= img.width < 1060 and 630 <= img.height < 700):
            raise AssertionError(f"native window failed to resize: {label} {img.size}")
    elif img.width < 1100 or img.height < 700:
        raise AssertionError(f"real production window not painted: {label} {img.size}")
    if len(img.resize((180, 120)).getcolors(180 * 120) or []) < 20:
        raise AssertionError(f"production screenshot unexpectedly blank: {label}")
    images[label] = img
region = (460, 85, 1150, 500)
from PIL import ImageStat
def aligned_difference(a, b):
    reference = images[a].crop((730, 135, 1060, 475))
    candidates = []
    for shift in range(-36, 37):
        candidate = images[b].crop((730 + shift, 135, 1060 + shift, 475))
        delta = ImageChops.difference(reference, candidate)
        candidates.append(sum(ImageStat.Stat(delta).mean) / 3)
    return min(candidates)

home_distance = aligned_difference("tail-reference", "home")
end_distance = aligned_difference("tail-reference", "end")
if home_distance < 8.0:
    raise AssertionError(
        f"Home did not leave the initial tail view: distance={home_distance:.3f}"
    )
if end_distance > 7.0:
    raise AssertionError(
        f"End did not restore the initial tail view: distance={end_distance:.3f}"
    )
# The checkbox itself must visibly change in the real window; merely
# injecting the click without entering continuous mode is not a pass.
# Checkbox lives just below the 91px header, relative to the
# dynamically allocated central panel. Catch unexpected sidebar growth too.
sys.path.insert(0, str(Path.cwd() / "tools/qa-native-reader"))
from panel_geometry import sidebar_edge
edge_before = sidebar_edge(images["end"])
edge_after = sidebar_edge(images["continuous-enabled"])
if abs(edge_before - edge_after) > 6:
    raise AssertionError(
        f"sidebar resized during checkbox interaction: {edge_before} -> {edge_after}"
    )
control_rect = (edge_before + 26, 104, edge_before + 210, 138)
control_delta = ImageChops.difference(
    images["end"].crop(control_rect), images["continuous-enabled"].crop(control_rect)
)
if control_delta.getbbox() is None:
    raise AssertionError("continuous reader checkbox did not respond to X11 click")
continuous_delta = ImageChops.difference(
    images["continuous-home"].crop(region), images["continuous-end"].crop(region)
)
if continuous_delta.getbbox() is None:
    raise AssertionError("continuous reader ignored Home/End navigation")
resized_region = (460, 85, 900, 500)
resized_delta = ImageChops.difference(
    images["continuous-resized-home"].crop(resized_region),
    images["continuous-resized-end"].crop(resized_region),
)
if resized_delta.getbbox() is None:
    raise AssertionError("continuous reader ignored Home/End after native resize")
summary = {
    "schema": "chatarium-long-native-reader-qa",
    "version": 1,
    "status": "pass",
    "mode": "actual-native-app",
    "durable_messages": 257,
    "single_conversation_owner": True,
    "first_and_last_markers_persisted": True,
    "home_leaves_tail": True,
    "end_restores_tail": True,
    "home_image_distance": round(home_distance, 3),
    "end_image_distance": round(end_distance, 3),
    "screenshots": 9,
    "resized_window_px": list(images["continuous-resized-end"].size),
    "continuous_resize_home_and_end_distinct": True,
    "continuous_reader_toggled_in_production": True,
    "continuous_home_and_end_distinct": True,
    "isolated_credentials_and_journal": True
}
(out / "long-summary.json").write_text(json.dumps(summary, indent=2) + "\n")
print("CHATARIUM_LONG_NATIVE_READER_QA " + json.dumps(summary))
PY
