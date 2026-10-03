#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
# A private headless compositor: no windows or network peers from the user's session.
export XDG_RUNTIME_DIR
XDG_RUNTIME_DIR=$(mktemp -d)
chmod 700 "$XDG_RUNTIME_DIR"
export WAYLAND_DISPLAY=localsend-test
export GDK_BACKEND=wayland
export GSK_RENDERER=cairo
export GTK_A11Y=none
export G_DEBUG=fatal-criticals
weston --backend=headless-backend.so --socket="$WAYLAND_DISPLAY" --width=1400 --height=1000 --idle-time=0 --no-config --log="$XDG_RUNTIME_DIR/weston.log" &
compositor=$!
trap 'kill "$compositor" 2>/dev/null || true; rm -rf -- "$XDG_RUNTIME_DIR"' EXIT
for attempt in $(seq 1 50); do
    test -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" && break
    sleep 0.1
done
test -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" || { cat "$XDG_RUNTIME_DIR/weston.log"; exit 1; }
if (($#)); then
    dbus-run-session -- "$@"
else
    dbus-run-session -- cargo test --locked wayland_views_and_selection -- --ignored --test-threads=1 --nocapture
fi
