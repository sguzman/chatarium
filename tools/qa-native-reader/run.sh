#!/usr/bin/env bash
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"
: "${CHATARIUM_READER_QA_OUTPUT:=.qa-native-reader}"
OUT="$ROOT/$CHATARIUM_READER_QA_OUTPUT"
mkdir -p "$OUT"
export CHATARIUM_READER_QA_DIR="$OUT"
STATE="$OUT/reader-state.json"

# All data is synthetic. The ordinary journal and personal Chromium profile
# are never read. Openbox supplies deterministic X11 window focus/resize.
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

"$ROOT/target/debug/chatarium-desktop" --qa-reader >"$OUT/reader.log" 2>&1 &
app_pid=$!
window=""
for _ in $(seq 1 120); do
    window="$(xdotool search --onlyvisible --name '^Chatarium Reader QA$' 2>/dev/null | head -n 1 || true)"
    if [[ -n "$window" && -s "$STATE" ]]; then break; fi
    if ! kill -0 "$app_pid" 2>/dev/null; then
        cat "$OUT/reader.log"
        echo "QA app exited before opening its disposable window" >&2
        exit 1
    fi
    sleep 0.25
done
if [[ -z "$window" || ! -s "$STATE" ]]; then
    cat "$OUT/reader.log"
    echo "Reader QA window/status did not become ready" >&2
    exit 1
fi

xdotool windowactivate --sync "$window"
xdotool mousemove --window "$window" 510 440

wait_state() {
    local condition="$1"
    for _ in $(seq 1 100); do
        if jq -e "$condition" "$STATE" >/dev/null 2>&1; then
            return 0
        fi
        if ! kill -0 "$app_pid" 2>/dev/null; then
            echo "Reader QA process ended" >&2
            cat "$OUT/reader.log"
            exit 1
        fi
        sleep 0.2
    done
    echo "Timed out waiting for state predicate: $condition" >&2
    cat "$STATE" >&2 || true
    tail -n 100 "$OUT/reader.log" >&2 || true
    return 1
}

save_frame() {
    local label="$1"
    sleep 0.5
    import -window "$window" "$OUT/$label.png"
    cp "$STATE" "$OUT/$label.json"
    test -s "$OUT/$label.png"
    jq -e '.source_count == 4096 and .rendered_count > 0 and .rendered_count < 48 and .persistent_user_journal_access == false and .remote_network_access == false' "$OUT/$label.json" >/dev/null
}

wait_state '.phase == "top" and .frame >= 3 and .render_start == 0'
save_frame top

xdotool key --clearmodifiers F2
wait_state '.phase == "middle" and .render_start > 1000 and .rendered_count > 0'
save_frame middle

original_offset="$(jq -r '.scroll_offset' "$STATE")"
xdotool mousemove --window "$window" 500 460
xdotool click --repeat 6 --delay 70 5
wait_state ".phase == \"middle\" and .scroll_offset > ($original_offset + 30)"
save_frame wheel

xdotool key --clearmodifiers F3
wait_state '.phase == "bottom" and .render_end > 4050'
save_frame bottom

xdotool key --clearmodifiers F4
wait_state '.phase == "search" and .rendered_search_hit == true'
save_frame search

xdotool windowsize "$window" 970 740
wait_state '.viewport_width < 1000'
xdotool key --clearmodifiers F4
wait_state '.phase == "search" and .viewport_width < 1000 and .rendered_search_hit == true'
save_frame resized

python3 tools/qa-native-reader/check_screenshots.py "$OUT"
jq -n --arg window "$window" \
    --slurpfile initial "$OUT/top.json" \
    --slurpfile middle "$OUT/middle.json" \
    --slurpfile wheel "$OUT/wheel.json" \
    --slurpfile bottom "$OUT/bottom.json" \
    --slurpfile search "$OUT/search.json" \
    --slurpfile resized "$OUT/resized.json" \
    '{schema:"chatarium-native-reader-visual-qa-summary",version:1,status:"pass",
      screenshots:6, window_id:$window,
      first:$initial[0].render_start,middle:$middle[0].render_start,
      wheel_offset:$wheel[0].scroll_offset,end:$bottom[0].render_end,
      search_hit:$search[0].rendered_search_hit,
      resized_hit:$resized[0].rendered_search_hit}' >"$OUT/summary.json"
echo "CHATARIUM_NATIVE_VISUAL_QA $(cat "$OUT/summary.json")"
