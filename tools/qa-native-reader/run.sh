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
    jq -e '(.source_count == 4096 or .source_count == 4097 or .source_count == 256) and .rendered_count > 0 and .rendered_count < 48 and .persistent_user_journal_access == false and .remote_network_access == false' "$OUT/$label.json" >/dev/null
    if [[ "${CHATARIUM_QA_REQUIRE_NOTO:-0}" == "1" ]]; then
        jq -e '(.font_fallbacks | index("chatarium.noto.cjk")) != null and (.font_fallbacks | index("chatarium.noto.arabic")) != null' "$OUT/$label.json" >/dev/null
    fi
}

wait_state '.phase == "top" and .frame >= 3 and .render_start == 0'
save_frame top

xdotool key --clearmodifiers F2
wait_state '.phase == "middle" and .render_start > 1000 and .rendered_count > 0'
save_frame middle

# Real keyboard events delivered to egui's ScrollArea: mouse hovers the
# transcript, but no synthetic F-key navigation is used for these two frames.
middle_offset="$(jq -r '.scroll_offset' "$STATE")"
xdotool mousemove --window "$window" 500 460
xdotool key --clearmodifiers Page_Down
wait_state ".phase == \"middle\" and .scroll_offset > ($middle_offset + 100)"
save_frame page-down

down_offset="$(jq -r '.scroll_offset' "$STATE")"
xdotool key --clearmodifiers Page_Up
wait_state ".phase == \"middle\" and .scroll_offset < ($down_offset - 100)"
save_frame page-up

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

# A true reflow regression: do not reissue F4. The selected hit should
# remain visible automatically when its estimated and measured heights change.
xdotool windowsize "$window" 970 740
wait_state '.phase == "search" and .viewport_width < 1000 and .rendered_search_hit == true'
save_frame resized

# Retain the selected search hit across mutation of a much earlier row.
# Never press F4 again: invalidation and target recovery belong to the
# shipping VirtualReaderLayout::sync/new_search_offset state machine.
previous_anchor="$(jq -r '.search_anchor_offset' "$STATE")"
xdotool key --clearmodifiers F8
wait_state ".phase == \"edited\" and .prefix_edited == true and .search_anchor_offset > ($previous_anchor + 500) and .rendered_search_hit == true"
save_frame edited

xdotool key --clearmodifiers F5
wait_state '.phase == "append" and .conversation == "primary" and .source_count == 4097 and .appended_message_visible == true'
save_frame appended

xdotool key --clearmodifiers F6
wait_state '.phase == "alternate" and .conversation == "alternate" and .source_count == 256 and .render_start == 0'
save_frame alternate

xdotool key --clearmodifiers F7
wait_state '.phase == "return" and .conversation == "primary" and .source_count == 4097 and .appended_message_visible == true'
# Require that restoration survives subsequent paint and height measurements,
# not just that the bubble was momentarily visible on the first return frame.
sleep 0.7
wait_state '.phase == "return" and .conversation == "primary" and .source_count == 4097 and .appended_message_visible == true'
save_frame returned

# Independent bookmark regression: returning near the tail is insufficient.
# Restore a saved mid-archive pixel offset after a conversation switch.
xdotool key --clearmodifiers F2
wait_state '.phase == "middle" and .conversation == "primary" and .source_count == 4097 and .render_start > 1000 and .render_end < 3500'
sleep 0.4
save_frame bookmarked
bookmark_offset="$(jq -r '.scroll_offset' "$STATE")"
xdotool key --clearmodifiers F6
wait_state '.phase == "alternate" and .conversation == "alternate" and .source_count == 256'
xdotool key --clearmodifiers F7
wait_state ".phase == \"return\" and .conversation == \"primary\" and .source_count == 4097 and (.scroll_offset - $bookmark_offset | abs) < 80"
sleep 0.7
wait_state ".phase == \"return\" and .conversation == \"primary\" and .source_count == 4097 and (.scroll_offset - $bookmark_offset | abs) < 80"
save_frame bookmark-returned

python3 tools/qa-native-reader/check_screenshots.py "$OUT"
jq -n --arg window "$window" \
    --slurpfile initial "$OUT/top.json" \
    --slurpfile middle "$OUT/middle.json" \
    --slurpfile page_down "$OUT/page-down.json" \
    --slurpfile page_up "$OUT/page-up.json" \
    --slurpfile wheel "$OUT/wheel.json" \
    --slurpfile bottom "$OUT/bottom.json" \
    --slurpfile search "$OUT/search.json" \
    --slurpfile resized "$OUT/resized.json" \
    --slurpfile edited "$OUT/edited.json" \
    --slurpfile appended "$OUT/appended.json" \
    --slurpfile alternate "$OUT/alternate.json" \
    --slurpfile returned "$OUT/returned.json" \
    --slurpfile bookmarked "$OUT/bookmarked.json" \
    --slurpfile bookmark_returned "$OUT/bookmark-returned.json" \
    '{schema:"chatarium-native-reader-visual-qa-summary",version:1,status:"pass",
      screenshots:14, window_id:$window,
      first:$initial[0].render_start,middle:$middle[0].render_start,
      page_down_offset:$page_down[0].scroll_offset,
      page_up_offset:$page_up[0].scroll_offset,
      wheel_offset:$wheel[0].scroll_offset,end:$bottom[0].render_end,
      search_hit:$search[0].rendered_search_hit,
      search_visible_height:$search[0].search_bubble_visible_height,
      resized_hit:$resized[0].rendered_search_hit,
      resized_search_visible_height:$resized[0].search_bubble_visible_height,
      edited_search_visible_height:$edited[0].search_bubble_visible_height,
      edited_search_anchor_offset:$edited[0].search_anchor_offset,
      appended_count:$appended[0].source_count,
      alternate_count:$alternate[0].source_count,
      returned_count:$returned[0].source_count,
      returned_append_visible:$returned[0].appended_message_visible,
      bookmarked_scroll_offset:$bookmarked[0].scroll_offset,
      restored_bookmark_scroll_offset:$bookmark_returned[0].scroll_offset}' >"$OUT/summary.json"
echo "CHATARIUM_NATIVE_VISUAL_QA $(cat "$OUT/summary.json")"
