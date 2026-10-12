#!/usr/bin/env bash
# Real desktop unsent-draft isolation/restart regression. No login, no network.
set -euo pipefail
ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"
source "$ROOT/tools/qa-native-reader/require_isolated_xvfb.sh"
OUT="$ROOT/.qa-native-draft-recovery"
if [[ -e "$OUT" || -L "$OUT" ]]; then
    echo "refusing existing disposable draft QA directory" >&2
    exit 1
fi
mkdir -p "$OUT"/{home,config,data,cache,runtime}
chmod 700 "$OUT/runtime"
for _ in $(seq 1 100); do
    if xdotool getdisplaygeometry >/dev/null 2>&1; then break; fi
    sleep 0.1
done
xdotool getdisplaygeometry >/dev/null 2>&1 || { echo "Xvfb not ready" >&2; exit 1; }
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

launch_app() {
    env -i PATH="$PATH" HOME="$OUT/home" XDG_CONFIG_HOME="$OUT/config" \
        XDG_DATA_HOME="$OUT/data" XDG_CACHE_HOME="$OUT/cache" \
        XDG_RUNTIME_DIR="$OUT/runtime" DISPLAY="$DISPLAY" \
        XAUTHORITY="$(printenv XAUTHORITY || true)" LIBGL_ALWAYS_SOFTWARE=1 \
        CHATARIUM_DATA_DIR="$OUT/data" \
        "$ROOT/target/debug/chatarium-desktop" >"$OUT/app.log" 2>&1 &
    app_pid=$!
    window=""
    for _ in $(seq 1 160); do
        window="$(xdotool search --onlyvisible --name '^Chatarium$' 2>/dev/null | head -n 1 || true)"
        if [[ -n "$window" ]]; then break; fi
        if ! kill -0 "$app_pid" 2>/dev/null; then
            tail -n 60 "$OUT/app.log" >&2 || true
            echo "draft QA desktop exited before opening" >&2
            exit 1
        fi
        sleep 0.25
    done
    if [[ -z "$window" ]]; then
        echo "draft QA desktop window not found" >&2
        exit 1
    fi
    for _ in $(seq 1 40); do
        xdotool windowactivate "$window" >/dev/null 2>&1 || true
        if [[ "$(xdotool getactivewindow 2>/dev/null || true)" == "$window" ]]; then
            sleep 0.5
            return 0
        fi
        sleep 0.25
    done
    echo "draft QA window activation timed out" >&2
    exit 1
}

# A durable draft must be scoped, not a user-message commit.
draft_is_durable() {
    python3 tools/qa-native-reader/check_draft_recovery.py saved \
        "$OUT/data/journal.jsonl" "$1"
}
wait_for_draft() {
    local marker="$1"
    for _ in $(seq 1 50); do
        if draft_is_durable "$marker" >/dev/null 2>&1; then return 0; fi
        sleep 0.2
    done
    echo "unsent draft failed to persist" >&2
    tail -n 80 "$OUT/app.log" >&2 || true
    return 1
}
capture() {
    sleep 0.6
    import -window "$window" "$OUT/$1.png"
}

ALPHA="CHATARIUM_DRAFT_ALPHA_PERSIST_WITHOUT_SENDING_314159"
BRAVO="CHATARIUM_DRAFT_BRAVO_OTHER_CONVERSATION_271828"
launch_app
capture "empty"
xdotool mousemove --window "$window" 740 620 click 1
xdotool type --clearmodifiers --delay 13 "$ALPHA"
wait_for_draft "$ALPHA"
capture "alpha"

# Only fully durable drafts may cross a conversation switch.
xdotool key --clearmodifiers ctrl+shift+n
sleep 0.9
capture "second-empty"
# A missed new-chat shortcut must not overwrite the first draft.
python3 tools/qa-native-reader/check_draft_recovery.py distinct-empty \
    "$OUT/empty.png" "$OUT/second-empty.png" "$OUT/alpha.png"
xdotool mousemove --window "$window" 740 620 click 1
xdotool type --clearmodifiers --delay 13 "$BRAVO"
wait_for_draft "$BRAVO"
python3 tools/qa-native-reader/check_draft_recovery.py owners \
    "$OUT/data/journal.jsonl" "$ALPHA" "$BRAVO"
capture "bravo"

xdotool key --clearmodifiers ctrl+shift+Tab
capture "alpha-return"
xdotool key --clearmodifiers ctrl+Tab
capture "bravo-return"

kill "$app_pid"
wait "$app_pid" 2>/dev/null || true
app_pid=""
sleep 0.5
launch_app
capture "bravo-restarted"
xdotool key --clearmodifiers ctrl+shift+Tab
capture "alpha-restarted"

python3 tools/qa-native-reader/check_draft_recovery.py verify \
    "$OUT" "$ALPHA" "$BRAVO"
test -s "$OUT/draft-recovery-summary.json"
