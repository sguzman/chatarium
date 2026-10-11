#!/usr/bin/env bash
# Smoke the real ChatariumApp window, not the synthetic reader.
# HOME, XDG and journal are disposable, with no account credentials.
set -euo pipefail
ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"
OUT="$ROOT/.qa-native-production"
# Fail closed rather than sharing an earlier QA journal or following a stale link.
if [[ -e "$OUT" || -L "$OUT" ]]; then
    echo "refusing to reuse an existing disposable production QA directory" >&2
    exit 1
fi
mkdir -p "$OUT"/{home,config,data,cache,runtime}
chmod 700 "$OUT/runtime"
LOG="$OUT/production.log"


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
    XAUTHORITY="${XAUTHORITY:-}" \
    LIBGL_ALWAYS_SOFTWARE=1 \
    CHATARIUM_DATA_DIR="$OUT/data" \
    "$ROOT/target/debug/chatarium-desktop" >"$LOG" 2>&1 &
app_pid=$!
window=""
for _ in $(seq 1 160); do
    window="$(xdotool search --onlyvisible --name '^Chatarium$' 2>/dev/null | head -n 1 || true)"
    if [[ -n "$window" ]]; then break; fi
    if ! kill -0 "$app_pid" 2>/dev/null; then
        echo "Production app exited during isolated GUI startup" >&2
        tail -n 120 "$LOG" >&2 || true
        exit 1
    fi
    sleep 0.25
done
if [[ -z "$window" ]]; then
    echo "Production app never created its native window" >&2
    tail -n 120 "$LOG" >&2 || true
    exit 1
fi
activate_window "$window"
sleep 1
import -window "$window" "$OUT/production-initial.png"

# An actual user-authored local turn through the production composer and
# persistence worker. Wait for the durable UserMessageCommitted event rather
# than accepting a merely autosaved draft or counting a button click.
MARKER="CHATARIUM_QA_COMMITTED_TURN_314159"
JOURNAL="$OUT/data/journal.jsonl"
xdotool mousemove --window "$window" 750 620 click 1
xdotool type --clearmodifiers --delay 15 "$MARKER"

is_committed() {
    python3 - "$JOURNAL" "$1" <<'PY'
import json
from pathlib import Path
import sys

journal, marker = Path(sys.argv[1]), sys.argv[2]
if not journal.is_file():
    raise SystemExit(1)
matches = 0
for raw in journal.read_text().splitlines():
    try:
        event = json.loads(raw)
    except json.JSONDecodeError:
        continue
    kind = "".join(ch.lower() for ch in str(event.get("kind", "")) if ch.isalnum())
    if kind == "usermessagecommitted" and marker in raw:
        matches += 1
raise SystemExit(0 if matches == 1 else 1)
PY
}
for _ in $(seq 1 35); do
    if is_committed "$MARKER"; then break; fi
    # Shortcut submits only when the editor holds focus and draft is durable.
    xdotool key --clearmodifiers ctrl+Return
    sleep 0.4
done
if ! is_committed "$MARKER"; then
    echo "Production UI did not durably commit its synthetic message" >&2
    tail -n 100 "$LOG" >&2 || true
    exit 1
fi
sleep 0.7
import -window "$window" "$OUT/production-committed.png"

# Exercise real native conversation creation (Ctrl+Shift+N), independent
# durable message ownership, and forward/backward conversation navigation.
SECOND_MARKER="CHATARIUM_QA_SECOND_CONVERSATION_271828"
xdotool key --clearmodifiers ctrl+shift+n
sleep 0.9
import -window "$window" "$OUT/production-second-empty.png"
# A real X11 shortcut can occasionally be lost to the focused composer.
# Never type the second message unless an empty conversation is visibly
# selected. Compare with the previously painted initial empty view, rather
# than guessing from elapsed time or accepting a successful key injection.
if ! python3 tools/qa-native-reader/check_empty_conversation.py "$OUT/production-initial.png" "$OUT/production-second-empty.png"; then
    echo "Ctrl+Shift+N did not open an empty conversation; retrying via UI button" >&2
    xdotool mousemove --window "$window" 55 276 click 1
    sleep 0.9
    import -window "$window" "$OUT/production-second-empty.png"
fi
python3 tools/qa-native-reader/check_empty_conversation.py "$OUT/production-initial.png" "$OUT/production-second-empty.png"
xdotool mousemove --window "$window" 750 620 click 1
xdotool type --clearmodifiers --delay 15 "$SECOND_MARKER"
for _ in $(seq 1 35); do
    if is_committed "$SECOND_MARKER"; then break; fi
    xdotool key --clearmodifiers ctrl+Return
    sleep 0.4
done
if ! is_committed "$SECOND_MARKER"; then
    echo "Second local conversation did not durably commit its synthetic message" >&2
    tail -n 100 "$LOG" >&2 || true
    exit 1
fi

# Typed commit schema explicitly owns the conversation id; the two text
# markers must belong to different conversations, not merely different turns.
python3 - "$JOURNAL" "$MARKER" "$SECOND_MARKER" <<'PY'
import json
from pathlib import Path
import sys

rows = Path(sys.argv[1]).read_text().splitlines()
markers = sys.argv[2:]
owners = {}
for raw in rows:
    event = json.loads(raw)
    kind = "".join(ch.lower() for ch in str(event.get("kind", "")) if ch.isalnum())
    if kind != "usermessagecommitted":
        continue
    payload = event.get("payload", "")
    if isinstance(payload, str):
        payload = json.loads(payload)
    if not isinstance(payload, dict):
        continue
    if payload.get("schema") != "chatarium-user-message-commit":
        continue
    for marker in markers:
        if payload.get("text") == marker:
            owners.setdefault(marker, []).append(payload.get("conversation_id"))
if any(len(owners.get(marker, [])) != 1 for marker in markers):
    raise AssertionError(f"expected one typed durable commit per marker: {owners}")
if not all(owners[marker][0] for marker in markers):
    raise AssertionError("typed commit missing owning conversation id")
if owners[markers[0]][0] == owners[markers[1]][0]:
    raise AssertionError("native new-chat shortcut did not establish distinct ownership")
print("CHATARIUM_PRODUCTION_CONVERSATION_OWNERSHIP PASS")
PY

sleep 0.7
import -window "$window" "$OUT/production-second-committed.png"
xdotool key --clearmodifiers ctrl+shift+Tab
sleep 0.8
import -window "$window" "$OUT/production-first-returned.png"
xdotool key --clearmodifiers ctrl+Tab
sleep 0.8
import -window "$window" "$OUT/production-second-returned.png"

xdotool windowsize "$window" 930 650
sleep 1
import -window "$window" "$OUT/production-resized.png"

# Capture matched first/second conversation screenshots at precisely the
# same window dimensions that the restarted app will use for comparison.
xdotool key --clearmodifiers ctrl+shift+Tab
sleep 0.8
import -window "$window" "$OUT/production-resized-first.png"
xdotool key --clearmodifiers ctrl+Tab
sleep 0.8
import -window "$window" "$OUT/production-resized-second.png"

# Snapshot typed durable ownership, terminate the real app, and relaunch
# with EXACTLY the same empty-home allowlist. This is a separate process,
# not merely a conversation switch within an already-populated projection.
python3 tools/qa-native-reader/check_replay.py snapshot "$JOURNAL" "$OUT/production-before-restart.json" "$MARKER" "$SECOND_MARKER"
kill "$app_pid"
wait "$app_pid" 2>/dev/null || true
app_pid=""
sleep 0.5

env -i \
    PATH="$PATH" \
    HOME="$OUT/home" \
    XDG_CONFIG_HOME="$OUT/config" \
    XDG_CACHE_HOME="$OUT/cache" \
    XDG_DATA_HOME="$OUT/data" \
    XDG_RUNTIME_DIR="$OUT/runtime" \
    DISPLAY="$DISPLAY" \
    XAUTHORITY="${XAUTHORITY:-}" \
    LIBGL_ALWAYS_SOFTWARE=1 \
    CHATARIUM_DATA_DIR="$OUT/data" \
    "$ROOT/target/debug/chatarium-desktop" >"$OUT/production-restarted.log" 2>&1 &
app_pid=$!
window=""
for _ in $(seq 1 160); do
    window="$(xdotool search --onlyvisible --name '^Chatarium$' 2>/dev/null | head -n 1 || true)"
    if [[ -n "$window" ]]; then break; fi
    if ! kill -0 "$app_pid" 2>/dev/null; then
        echo "Production app failed to reopen its isolated journal" >&2
        tail -n 120 "$OUT/production-restarted.log" >&2 || true
        exit 1
    fi
    sleep 0.25
done
if [[ -z "$window" ]]; then
    echo "Production app did not reopen a native window" >&2
    tail -n 120 "$OUT/production-restarted.log" >&2 || true
    exit 1
fi
activate_window "$window"
xdotool windowsize "$window" 930 650
sleep 1.2
import -window "$window" "$OUT/production-restarted.png"
xdotool key --clearmodifiers ctrl+shift+Tab
sleep 0.8
import -window "$window" "$OUT/production-restarted-switched.png"
xdotool key --clearmodifiers ctrl+Tab
sleep 0.8
import -window "$window" "$OUT/production-restarted-returned.png"
python3 tools/qa-native-reader/check_replay.py verify "$JOURNAL" "$OUT/production-before-restart.json" "$MARKER" "$SECOND_MARKER"

python3 - "$OUT" <<'PY'
import json
from pathlib import Path
from PIL import Image, ImageChops
import sys

out = Path(sys.argv[1])
images = []
for stem in (
    "production-initial", "production-committed", "production-second-empty",
    "production-second-committed", "production-first-returned",
    "production-second-returned", "production-resized",
    "production-resized-first", "production-resized-second",
    "production-restarted", "production-restarted-switched",
    "production-restarted-returned",
):
    file = out / (stem + ".png")
    with Image.open(file) as src:
        img = src.convert("RGB")
    if img.width < 860 or img.height < 560:
        raise AssertionError(f"production window too small or absent: {stem} {img.size}")
    if len(img.resize((180, 120)).getcolors(180 * 120) or []) < 20:
        raise AssertionError(f"production window did not paint plausible UI: {stem}")
    images.append(img)
if images[0].width - images[6].width < 120:
    raise AssertionError("production window failed native resize")
for idx in range(7, len(images)):
    if images[idx].size != images[6].size:
        raise AssertionError(f"restart screenshot did not preserve requested size: {idx}")
# The message itself must materially change the central transcript area.
# Comparing only the title would miss a failure to display the committed turn.
chat_rect = (460, 75, min(images[0].width, images[1].width), 520)
if ImageChops.difference(images[0].crop(chat_rect), images[1].crop(chat_rect)).getbbox() is None:
    raise AssertionError("committed turn did not change the production transcript")
for old, new in ((1, 2), (2, 3), (3, 4), (4, 5)):
    w = min(images[old].width, images[new].width)
    h = min(images[old].height, images[new].height)
    body = (460, 75, w, min(520, h))
    if ImageChops.difference(images[old].crop(body), images[new].crop(body)).getbbox() is None:
        raise AssertionError(f"production conversation UI failed to change: {old} -> {new}")
w = min(images[5].width, images[6].width)
h = min(images[5].height, images[6].height)
if ImageChops.difference(images[5].crop((0, 0, w, h)), images[6].crop((0, 0, w, h))).getbbox() is None:
    raise AssertionError("production UI did not respond visually to resize")

# The two persisted message surfaces must reappear in the new *process*.
# Compare the message+title area to pre-restart references; no OCR or account
# data. Permit either selection order, but require both distinct matches.
from PIL import ImageStat
def distance(left, right):
    box = (max(320, left.width // 2), 0, min(left.width, right.width) - 20, 250)
    if box[2] <= box[0]:
        raise AssertionError("insufficient production screenshot transcript width")
    diff = ImageChops.difference(left.crop(box), right.crop(box))
    return sum(ImageStat.Stat(diff).mean) / 3
reference_first, reference_second = images[7], images[8]
reference_gap = distance(reference_first, reference_second)
if reference_gap < 0.12:
    raise AssertionError(f"two production conversations not visually distinct: {reference_gap:.3f}")
classifications = []
comparisons = []
for image in images[9:12]:
    first = distance(image, reference_first)
    second = distance(image, reference_second)
    comparisons.append([round(first, 4), round(second, 4)])
    if min(first, second) >= max(0.25, reference_gap * 0.65):
        classifications.append("none")
    else:
        classifications.append("first" if first < second else "second")
if set(classifications) != {"first", "second"}:
    raise AssertionError(f"restart did not reproject both committed conversations: {classifications}, distances={comparisons}, reference_gap={reference_gap:.4f}")
(out / "production-summary.json").write_text(json.dumps({
    "schema": "chatarium-isolated-production-window",
    "version": 1,
    "status": "pass",
    "scenario": "fresh-offline-workspace",
    "initial_px": list(images[0].size),
    "committed_px": list(images[1].size),
    "resized_px": list(images[-1].size),
    "screenshots": 12,
    "durable_user_messages": 2,
    "restarted_process": True,
    "replayed_conversation_screens": classifications,
    "replay_reference_gap": round(reference_gap, 4),
    "replay_image_distances": comparisons,
    "distinct_message_conversations": True,
    "keyboard_conversation_switching": True,
    "isolated_home": True,
    "isolated_data_dir": True,
}, indent=2) + "\n")
print("CHATARIUM_PRODUCTION_GUI_SMOKE PASS")
PY
test -d "$OUT/home"
test -d "$OUT/data"
test -s "$OUT/production-initial.png"
test -s "$OUT/production-committed.png"
test -s "$OUT/production-second-committed.png"
test -s "$OUT/production-first-returned.png"
test -s "$OUT/production-second-returned.png"
test -s "$OUT/production-resized.png"
test -s "$OUT/production-restarted.png"
test -s "$OUT/production-restarted-switched.png"
test -s "$OUT/production-restarted-returned.png"
