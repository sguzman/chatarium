#!/usr/bin/env bash
# Smoke the real ChatariumApp window, not the synthetic reader.
# HOME, XDG and journal are disposable, with no account credentials.
set -euo pipefail
ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"
OUT="$ROOT/.qa-native-production"
mkdir -p "$OUT"/{home,config,data,cache,runtime}
chmod 700 "$OUT/runtime"
LOG="$OUT/production.log"

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
xdotool windowactivate --sync "$window"
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
    python3 - "$JOURNAL" "$MARKER" <<'PY'
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
    if is_committed; then break; fi
    # Shortcut submits only when the editor holds focus and draft is durable.
    xdotool key --clearmodifiers ctrl+Return
    sleep 0.4
done
if ! is_committed; then
    echo "Production UI did not durably commit its synthetic message" >&2
    tail -n 100 "$LOG" >&2 || true
    exit 1
fi
sleep 0.7
import -window "$window" "$OUT/production-committed.png"

xdotool windowsize "$window" 930 650
sleep 1
import -window "$window" "$OUT/production-resized.png"

python3 - "$OUT" <<'PY'
import json
from pathlib import Path
from PIL import Image, ImageChops
import sys

out = Path(sys.argv[1])
images = []
for stem in ("production-initial", "production-committed", "production-resized"):
    file = out / (stem + ".png")
    with Image.open(file) as src:
        img = src.convert("RGB")
    if img.width < 860 or img.height < 560:
        raise AssertionError(f"production window too small or absent: {stem} {img.size}")
    if len(img.resize((180, 120)).getcolors(180 * 120) or []) < 20:
        raise AssertionError(f"production window did not paint plausible UI: {stem}")
    images.append(img)
if images[0].width - images[2].width < 120:
    raise AssertionError("production window failed native resize")
# The message itself must materially change the central transcript area.
# Comparing only the title would miss a failure to display the committed turn.
chat_rect = (460, 75, min(images[0].width, images[1].width), 520)
if ImageChops.difference(images[0].crop(chat_rect), images[1].crop(chat_rect)).getbbox() is None:
    raise AssertionError("committed turn did not change the production transcript")
w = min(images[1].width, images[2].width)
h = min(images[1].height, images[2].height)
if ImageChops.difference(images[1].crop((0, 0, w, h)), images[2].crop((0, 0, w, h))).getbbox() is None:
    raise AssertionError("production UI did not respond visually to resize")
(out / "production-summary.json").write_text(json.dumps({
    "schema": "chatarium-isolated-production-window",
    "version": 1,
    "status": "pass",
    "scenario": "fresh-offline-workspace",
    "initial_px": list(images[0].size),
    "committed_px": list(images[1].size),
    "resized_px": list(images[2].size),
    "screenshots": 3,
    "durable_user_message": True,
    "isolated_home": True,
    "isolated_data_dir": True,
}, indent=2) + "\n")
print("CHATARIUM_PRODUCTION_GUI_SMOKE PASS")
PY
test -d "$OUT/home"
test -d "$OUT/data"
test -s "$OUT/production-initial.png"
test -s "$OUT/production-committed.png"
test -s "$OUT/production-resized.png"
