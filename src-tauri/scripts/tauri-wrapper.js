#!/usr/bin/env node
//
// tauri-wrapper.js -- transparent wrapper around the real Tauri CLI.
//
// Forwards every argument straight through to `tauri` (via npx, so it picks
// up the locally installed @tauri-apps/cli). The ONLY thing this adds: when
// the forwarded command included `build` and we're running on Linux, it
// runs fix-appimage.sh (same directory) against whatever AppImage(s) that
// build just produced -- see that script's own header comment for why.
//
// Written in Node, not bash: npm's "tauri" script used to shell out to
// `bash tauri-wrapper.sh`, which broke `npm run tauri dev` from PowerShell
// on any Windows machine with WSL installed -- WSL ships its own
// C:\Windows\System32\bash.exe launcher stub, which can shadow Git Bash on
// PATH and silently redirect the whole command into WSL/Linux. There, npx
// resolves the Linux-native @tauri-apps/cli binding against a node_modules
// that only has the Windows one installed, failing with a confusing
// "Cannot find native binding" error. Node has no such ambiguity: this
// script always runs as the very process npm just spawned.
//
// Only the desktop `build` subcommand qualifies (checked via argv[0], not a
// scan of every arg) -- `tauri android build` / `tauri ios build` also
// contain the word "build" but produce no AppImage, so a match-anywhere
// check would run fix-appimage.sh against a bundle/appimage directory that
// was never created and fail the whole mobile build for no reason.
//
// Wired in via package.json's "tauri" script pointing here instead of at
// the raw `tauri` binary.

import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);

const result = spawnSync("npx", ["tauri", ...args], {
  stdio: "inherit",
  shell: true,
});
const status = result.status ?? 1;

const isBuild = args[0] === "build";

if (status === 0 && isBuild && process.platform === "linux") {
  const fix = spawnSync("bash", [path.join(SCRIPT_DIR, "fix-appimage.sh")], {
    stdio: "inherit",
  });
  if (fix.status !== 0) {
    console.error(
      "[tauri-wrapper] fix-appimage.sh failed -- the AppImage in " +
        "src-tauri/target/release/bundle/appimage may still bundle " +
        "conflicting host libraries (libwayland-client etc.). The build " +
        "itself succeeded; only the post-processing step failed."
    );
    process.exit(1);
  }
}

process.exit(status);
