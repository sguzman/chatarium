#!/usr/bin/env bash
# Test the actual desktop app against an isolated 257-message durable journal.
set -euo pipefail
ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"
OUT="$ROOT/.qa-native-long-reader"
if [[ -e "$OUT" ]]; then
    echo "refusing to reuse an existing disposable long-reader fixture" >&2
    exit 1
fi
mkdir -p "$OUT"/{home,config,cache,runtime}
chmod 700 "$OUT/runtime"
"$ROOT/target/debug/chatarium-reader-seed" \
    .qa-native-long-reader/data/journal.jsonl >"$OUT/seed.json"
jq -e '.schema == "chatarium-long-native-reader-fixture" and .events == 257 and .disposable == true' "$OUT/seed.json" >/dev/null
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
    XAUTHORITY="$(printenv XAUTHORITY || true)" \
    LIBGL_ALWAYS_SOFTWARE=1 \
    CHATARIUM_DATA_DIR="$OUT/data" \
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
xdotool windowactivate --sync "$window"
sleep 1
import -window "$window" "$OUT/long-initial.png"
xdotool mousemove --window "$window" 740 250 click 1
xdotool key --clearmodifiers Home
sleep 0.8
import -window "$window" "$OUT/long-home.png"
xdotool key --clearmodifiers End
sleep 0.8
import -window "$window" "$OUT/long-end.png"

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
for label in ("initial", "home", "end"):
    with Image.open(out / f"long-{label}.png") as src:
        img = src.convert("RGB")
    if img.width < 1100 or img.height < 700:
        raise AssertionError(f"real production window not painted: {label} {img.size}")
    if len(img.resize((180, 120)).getcolors(180 * 120) or []) < 20:
        raise AssertionError(f"production screenshot unexpectedly blank: {label}")
    images[label] = img
region = (460, 85, 1150, 500)
if ImageChops.difference(images["home"].crop(region), images["end"].crop(region)).getbbox() is None:
    raise AssertionError("production reader failed to change between Home and End")
summary = {
    "schema": "chatarium-long-native-reader-qa",
    "version": 1,
    "status": "pass",
    "mode": "actual-native-app",
    "durable_messages": 257,
    "single_conversation_owner": True,
    "first_and_last_markers_persisted": True,
    "home_and_end_change_transcript": True,
    "screenshots": 3,
    "isolated_credentials_and_journal": True
}
(out / "long-summary.json").write_text(json.dumps(summary, indent=2) + "\n")
print("CHATARIUM_LONG_NATIVE_READER_QA " + json.dumps(summary))
PY
