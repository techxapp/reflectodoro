#!/usr/bin/env bash
#
# fix-appimage.sh -- strip host-incompatible bundled libraries out of the
# AppImage(s) linuxdeploy just produced, then repackage into a single file.
#
# BACKGROUND
# ----------
# Tauri's Linux bundler (linuxdeploy + its GTK plugin) copies whatever
# libwayland-client/cursor/egl/server it finds on the *build machine* into
# the AppDir, even though this app forces GDK_BACKEND=x11 and never talks to
# a compositor through them directly. Those libraries have to match the
# Wayland protocol/ABI of the machine actually running the app -- bundling
# the build machine's copy means a different host (different Mesa/Wayland
# stack) can fail with things like:
#
#   libEGL_mesa.so.0: symbol lookup error: undefined symbol: wl_fixes_interface
#   Could not create default EGL display: EGL_BAD_PARAMETER. Aborting...
#
# because the AppImage's AppRun puts the bundled lib dir first on
# LD_LIBRARY_PATH, so the *host's* libEGL_mesa.so.0 (or another host library
# not bundled) resolves its Wayland symbols against the *bundled*
# libwayland-client.so.0 instead of the host's own, and the two don't agree
# on ABI. Deleting the bundled Wayland client libraries lets everything that
# needs them (GTK's own Wayland backend, Mesa, etc.) fall through to the
# host's matched set instead -- confirmed as the reliable fix in manual
# testing before this script existed.
#
# We deliberately do NOT touch bundled GLib/GIO/gio-modules here: those also
# showed version-mismatch symptoms (undefined g_variant_builder_init_static,
# g_module_check_init) in testing, but forcing the host's libglib-2.0 via
# LD_PRELOAD segfaulted, and this app's tray icon depends on GIO's dbus
# module (libayatana-appindicator talks to the tray over D-Bus via GIO) --
# so blanket-removing gio/modules risks breaking the tray instead. The
# GVFS "Failed to load module" warnings are cosmetic (GIO just skips that
# backend) and not the thing that was actually blocking startup. If real
# GIO-related crashes show up later, treat that as a separate, narrower fix.
#
# WHAT THIS SCRIPT DOES
# ----------------------
#   1. Finds every *.AppImage under the bundle output directory.
#   2. Extracts each one (--appimage-extract) into a scratch dir.
#   3. Deletes libwayland-{client,cursor,egl,server}.so* wherever they
#      appear under the AppDir's lib directories.
#   4. Repackages the AppDir back into a single-file AppImage (appimagetool
#      if available, else Tauri's own cached linuxdeploy AppImage) and
#      replaces the original file in place.
#   5. If TAURI_SIGNING_PRIVATE_KEY is set, re-signs the new file with the
#      Tauri CLI (`npm run tauri signer sign`) so tauri-plugin-updater's
#      signature matches the modified bytes -- the original .sig from
#      `tauri build` was computed before this script ran and is now stale.
#
# SCOPE / KNOWN LIMITATION (see chat with the user, 2026-08-31): this script
# is for local/manual `tauri build` runs right now. It does NOT update
# latest.json's signature/size fields, and it is not yet wired into
# .github/workflows/release.yml -- a real release still needs that done as
# a follow-up before this fix applies to published auto-update artifacts.
#
# USAGE
#   bash src-tauri/scripts/fix-appimage.sh [bundle-dir]
#
# bundle-dir defaults to src-tauri/target/release/bundle/appimage relative
# to the repo root this script lives under (src-tauri/scripts/..).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BUNDLE_DIR="${1:-$REPO_ROOT/src-tauri/target/release/bundle/appimage}"
ARCH="${ARCH:-$(uname -m)}"

# Libraries that must come from the host, never the build machine -- see
# the BACKGROUND comment above for why.
STRIP_PATTERNS=(
  "libwayland-client.so*"
  "libwayland-cursor.so*"
  "libwayland-egl.so*"
  "libwayland-server.so*"
)

log() { printf '[fix-appimage] %s\n' "$*" >&2; }
die() { log "ERROR: $*"; exit 1; }

[ -d "$BUNDLE_DIR" ] || die "bundle directory not found: $BUNDLE_DIR (build the AppImage first with 'npm run tauri build')"

command -v patchelf >/dev/null 2>&1 || log "warning: patchelf not found on PATH -- repackaging tools may need it"

mapfile -t APPIMAGES < <(find "$BUNDLE_DIR" -maxdepth 1 -type f -name '*.AppImage' | sort)
[ "${#APPIMAGES[@]}" -gt 0 ] || die "no *.AppImage files found in $BUNDLE_DIR"

find_repackager() {
  if command -v appimagetool >/dev/null 2>&1; then
    echo "appimagetool"
    return
  fi
  local cached="$HOME/.cache/tauri/linuxdeploy-$ARCH.AppImage"
  if [ -x "$cached" ]; then
    echo "$cached"
    return
  fi
  echo ""
}

REPACKAGER="$(find_repackager)"
[ -n "$REPACKAGER" ] || die "no repackaging tool found: install appimagetool (https://github.com/AppImage/appimagetool) or ensure \$HOME/.cache/tauri/linuxdeploy-$ARCH.AppImage exists (created by a prior 'tauri build')"
log "using repackager: $REPACKAGER"

for APPIMAGE in "${APPIMAGES[@]}"; do
  log "processing $APPIMAGE"
  ORIG_SIZE=$(stat -c%s "$APPIMAGE")

  WORKDIR="$(mktemp -d)"
  trap 'rm -rf "$WORKDIR"' EXIT

  chmod +x "$APPIMAGE"
  (
    cd "$WORKDIR"
    "$APPIMAGE" --appimage-extract >/dev/null
  )
  APPDIR="$WORKDIR/squashfs-root"
  [ -d "$APPDIR" ] || die "extraction failed, no squashfs-root produced for $APPIMAGE"

  REMOVED=()
  for pattern in "${STRIP_PATTERNS[@]}"; do
    while IFS= read -r -d '' f; do
      REMOVED+=("${f#"$APPDIR"/}")
      rm -f "$f"
    done < <(find "$APPDIR" -type f -name "$pattern" -print0)
  done

  if [ "${#REMOVED[@]}" -eq 0 ]; then
    log "  no bundled Wayland client libraries found -- nothing to strip (already fixed, or linuxdeploy stopped bundling them)"
  else
    log "  removed:"
    for f in "${REMOVED[@]}"; do log "    $f"; done
  fi

  NEW_APPIMAGE="$WORKDIR/$(basename "$APPIMAGE")"
  if [ "$REPACKAGER" = "appimagetool" ]; then
    ARCH="$ARCH" appimagetool "$APPDIR" "$NEW_APPIMAGE" >/dev/null
  else
    (
      cd "$WORKDIR"
      ARCH="$ARCH" "$REPACKAGER" --appimage-extract-and-run --appdir "$APPDIR" --output appimage >/dev/null
      # linuxdeploy names its output after the .desktop file; pick up
      # whatever *.AppImage it produced in WORKDIR.
      produced="$(find "$WORKDIR" -maxdepth 1 -type f -name '*.AppImage' ! -name "$(basename "$APPIMAGE")" | head -n1)"
      [ -n "$produced" ] && mv "$produced" "$NEW_APPIMAGE"
    )
  fi
  [ -f "$NEW_APPIMAGE" ] || die "repackaging did not produce an AppImage for $APPIMAGE"

  chmod +x "$NEW_APPIMAGE"
  mv "$NEW_APPIMAGE" "$APPIMAGE"
  NEW_SIZE=$(stat -c%s "$APPIMAGE")
  log "  repackaged: $ORIG_SIZE -> $NEW_SIZE bytes"

  if [ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ]; then
    log "  re-signing (TAURI_SIGNING_PRIVATE_KEY is set) -- old .sig from 'tauri build' is stale after repackaging"
    SIGN_ARGS=(signer sign -k "$TAURI_SIGNING_PRIVATE_KEY")
    if [ -n "${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}" ]; then
      SIGN_ARGS+=(-p "$TAURI_SIGNING_PRIVATE_KEY_PASSWORD")
    fi
    SIGN_ARGS+=("$APPIMAGE")
    ( cd "$REPO_ROOT" && npx tauri "${SIGN_ARGS[@]}" )
  else
    log "  TAURI_SIGNING_PRIVATE_KEY not set -- skipping re-sign. Any existing .sig / latest.json entry for this"
    log "  file is now stale; do not publish this artifact through the updater until it's re-signed."
  fi

  rm -rf "$WORKDIR"
  trap - EXIT
done

log "done: fixed ${#APPIMAGES[@]} AppImage(s) in $BUNDLE_DIR"
