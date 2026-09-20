#!/usr/bin/env bash
# Automated regression test for "the break overlay must interrupt a full-screen
# app on macOS". See README.md in this directory for the bug it covers.
#
# Verdict comes from CGWindowListCopyWindowInfo(.optionOnScreenOnly) -- the list
# of windows on the *active* Space -- sampled from outside the app, because
# every readback inside the app (isVisible, isOnActiveSpace, collectionBehavior,
# level) read back correct while the bug was live.
#
# Usage: ./run-test.sh [path-to-app-binary]
set -uo pipefail
cd "$(dirname "$0")"

APP_BIN="${1:-../../src-tauri/target/debug/reflectodoro}"
BUILD_DIR=".build"
TIMEOUT_S="${TIMEOUT_S:-45}"

fail() { echo "FAIL: $*" >&2; exit 1; }

[ -x "$APP_BIN" ] || fail "no app binary at $APP_BIN
  Build one first, e.g.:  (cd src-tauri && cargo build)
  Pass a different path as the first argument if yours lives elsewhere."

mkdir -p "$BUILD_DIR"
for src in fullscreen-victim onscreen-windows screen-size; do
  if [ ! -x "$BUILD_DIR/$src" ] || [ "$src.swift" -nt "$BUILD_DIR/$src" ]; then
    echo "building $src..."
    swiftc -O "$src.swift" -o "$BUILD_DIR/$src" || fail "swiftc failed for $src.swift"
  fi
done

SCREEN="$("$BUILD_DIR/screen-size")"
SCREEN_W="${SCREEN%x*}"; SCREEN_H="${SCREEN#*x}"
echo "main screen: ${SCREEN_W}x${SCREEN_H}"

APP_NAME="$(basename "$APP_BIN")"
cleanup() {
  pkill -x "$APP_NAME" 2>/dev/null
  pkill -x fullscreen-victim 2>/dev/null
}
trap cleanup EXIT INT TERM

# tauri-plugin-single-instance would otherwise hand our launch straight to an
# app that's already running (focusing it instead of starting the test's own
# process with POMODORO_FORCE_BREAK_ON_START set).
if pgrep -x "$APP_NAME" >/dev/null; then
  echo "note: stopping the already-running $APP_NAME (single-instance would redirect our launch to it)"
  pkill -x "$APP_NAME"; sleep 1
fi

# Order matters, and getting it wrong silently invalidates the test. Launching
# the app *after* the victim makes macOS switch away from the victim's
# full-screen Space to show the app's main window, so the break then opens over
# an ordinary desktop and passes for the wrong reason (observed while building
# this). The real sequence is: app already running in the background -> user
# goes full-screen -> break fires. POMODORO_FORCE_BREAK_DELAY_S is what buys
# room to set that up.
BREAK_DELAY_S="${BREAK_DELAY_S:-16}"
echo "launching $APP_NAME (break in ${BREAK_DELAY_S}s)..."
POMODORO_FORCE_BREAK_ON_START=1 \
  POMODORO_FORCE_BREAK_DELAY_S="$BREAK_DELAY_S" \
  POMODORO_FORCE_BREAK_MINUTES="${BREAK_MINUTES:-2}" \
  "$APP_BIN" > "$BUILD_DIR/app.log" 2>&1 &
break_at=$(( $(date +%s) + BREAK_DELAY_S ))
sleep 6   # let the app finish booting and showing its main window

echo "starting full-screen victim app..."
VICTIM_SECONDS="$TIMEOUT_S" "$BUILD_DIR/fullscreen-victim" > "$BUILD_DIR/victim.log" 2>&1 &
for _ in $(seq 1 40); do
  grep -q "victim: ready isFullScreen=true" "$BUILD_DIR/victim.log" 2>/dev/null && break
  sleep 0.25
done
grep -q "victim: ready isFullScreen=true" "$BUILD_DIR/victim.log" \
  || fail "victim never reached fullscreen ($(cat "$BUILD_DIR/victim.log"))"
echo "victim is full-screen and owns the active Space; waiting for the break..."

deadline=$(( break_at + 25 ))
overlay_line=""; victim_seen=0
while [ "$(date +%s)" -lt "$deadline" ]; do
  snapshot="$("$BUILD_DIR/onscreen-windows" 2>/dev/null)"
  echo "$snapshot" | grep -qi "owner=fullscreen-victim" && victim_seen=1 || victim_seen=0
  # The overlay: ours, raised to NSScreenSaverWindowLevel (1000), covering the
  # whole screen. The tray icon is also ours but sits at layer 25.
  overlay_line="$(echo "$snapshot" | awk -v app="$APP_NAME" -v w="$SCREEN_W" -v h="$SCREEN_H" '
    tolower($0) ~ tolower("owner=" app) {
      split($2,l,"="); split($5,ww,"="); split($6,hh,"=")
      if (l[2] >= 1000 && ww[2] >= w && hh[2] >= h) { print; exit }
    }')"
  [ -n "$overlay_line" ] && [ "$victim_seen" = 1 ] && break
  sleep 0.5
done

echo
echo "--- app log (overlay path) ---"
grep -E "force|accessory|with_accessory_policy|prepare_window_before_show|configure_window|reassert|phase transition" \
  "$BUILD_DIR/app.log" | tail -12
echo "--- on-screen windows (active Space) ---"
"$BUILD_DIR/onscreen-windows" | grep -iE "owner=($APP_NAME|fullscreen-victim)" || echo "  (neither app on the active Space)"
echo

if [ -z "$overlay_line" ]; then
  echo "RESULT: FAIL -- the break overlay never reached the victim's full-screen Space."
  echo "        (this is the shipped bug: the app reports the overlay visible, but the"
  echo "         WindowServer has it on a different Space, so the user sees nothing)"
  exit 1
fi
if [ "$victim_seen" != 1 ]; then
  echo "RESULT: INCONCLUSIVE -- overlay found, but the victim was not on the active Space,"
  echo "        so the run never actually tested the full-screen case."
  exit 2
fi
# Being on the right Space isn't the same as covering the screen: a regression
# in cover_current_monitor left the overlay at y=-480 on a 1080-tall screen
# (top 600px only), which every Space-level check still called a pass.
oy="$(echo "$overlay_line" | awk '{split($4,a,"="); print a[2]}')"
ox="$(echo "$overlay_line" | awk '{split($3,a,"="); print a[2]}')"
if [ "${oy:-0}" -ne 0 ] || [ "${ox:-0}" -ne 0 ]; then
  echo "RESULT: FAIL -- overlay is on the active Space but does not cover it:"
  echo "        $overlay_line"
  echo "        expected origin x=0 y=0 for a ${SCREEN_W}x${SCREEN_H} screen"
  exit 1
fi
echo "RESULT: PASS -- overlay covers the active Space *while* the full-screen app owns it:"
echo "        $overlay_line"
