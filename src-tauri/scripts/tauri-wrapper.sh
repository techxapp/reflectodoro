#!/usr/bin/env bash
#
# tauri-wrapper.sh -- transparent wrapper around the real Tauri CLI.
#
# Forwards every argument straight through to `tauri` (via npx, so it picks
# up the locally installed @tauri-apps/cli). The ONLY thing this adds: when
# the forwarded command included `build` and we're running on Linux, it
# runs fix-appimage.sh (same directory) against whatever AppImage(s) that
# build just produced -- see that script's own header comment for why:
# linuxdeploy bundles the build machine's libwayland-client/cursor/egl/server
# into the AppImage, which then conflicts with a different host's Wayland/
# Mesa stack (EGL_BAD_PARAMETER / undefined symbol: wl_fixes_interface).
#
# This exists so `npm run tauri build` -- the command people actually type,
# muscle memory from every other Tauri project -- gets the fix for free,
# instead of relying on remembering a separate `tauri:build:linux` script.
# `npm run tauri dev`, `npm run tauri icon`, etc. all pass through unchanged
# and untouched; the extra step only fires for a Linux `build`.
#
# Wired in via package.json's "tauri" script pointing here instead of at
# the raw `tauri` binary.

set -uo pipefail  # deliberately no -e: we need $? from the tauri call below,
                   # which `set -e` would short-circuit past.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

npx tauri "$@"
STATUS=$?

is_build=false
for arg in "$@"; do
  if [ "$arg" = "build" ]; then
    is_build=true
    break
  fi
done

if [ "$STATUS" -eq 0 ] && [ "$is_build" = true ] && [ "$(uname -s)" = "Linux" ]; then
  if ! bash "$SCRIPT_DIR/fix-appimage.sh"; then
    echo "[tauri-wrapper] fix-appimage.sh failed -- the AppImage in" \
         "src-tauri/target/release/bundle/appimage may still bundle" \
         "conflicting host libraries (libwayland-client etc.). The build" \
         "itself succeeded; only the post-processing step failed." >&2
    exit 1
  fi
fi

exit "$STATUS"
