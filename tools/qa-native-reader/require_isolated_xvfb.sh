#!/usr/bin/env bash
# Source this before opening a GUI or touching disposable fixture directories.
# An opt-in flag alone is not sufficient: the display must belong to a local
# Xvfb process, not a user's XWayland/Xorg desktop or a remote X server.
if [[ "${CHATARIUM_QA_ISOLATED_XVFB:-}" != "1" ]]; then
    echo "native reader QA requires explicit isolated-Xvfb opt-in" >&2
    exit 1
fi
if [[ ! "${DISPLAY:-}" =~ ^:([0-9]+)(\.[0-9]+)?$ ]]; then
    echo "native reader QA refuses nonlocal or malformed DISPLAY" >&2
    exit 1
fi
qa_display=":${BASH_REMATCH[1]}"
qa_xvfb_found=0
while IFS= read -r qa_pid; do
    [[ -r "/proc/$qa_pid/cmdline" ]] || continue
    qa_args=()
    mapfile -d '' -t qa_args < "/proc/$qa_pid/cmdline" || continue
    for qa_arg in "${qa_args[@]:1}"; do
        if [[ "$qa_arg" == "$qa_display" ]]; then
            qa_xvfb_found=1
            break 2
        fi
    done
done < <(pgrep -x Xvfb || true)
unset qa_pid qa_args qa_arg qa_display
if [[ "$qa_xvfb_found" != "1" ]]; then
    echo "native reader QA refuses DISPLAY without a matching local Xvfb process" >&2
    exit 1
fi
unset qa_xvfb_found
