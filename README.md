# Reflectodoro

A Pomodoro app whose real point is forcing a short self-reflection ("what did I do?") at
the end of every break, enforced via a hard-to-dismiss overlay. Windows, macOS, and Linux,
built with Tauri 2 + SvelteKit.

[Download the latest release](https://github.com/techxapp/reflectodoro/releases/latest) &middot;
[Project page](https://reflectodoro.droplee.com)

See [CLAUDE.md](./CLAUDE.md) for architecture, the unlock formula, and data model.

## Run it in development

```
npm install
npm run tauri dev
```

## Build a production installer

```
npm run tauri build
```

Produces a `.exe` installer under `src-tauri/target/release/bundle/` (a `.dmg`/`.app` on
macOS, or an `.AppImage` on Linux). Pushing a `vX.Y.Z` tag also triggers
`.github/workflows/release.yml`, which builds all platforms and publishes them
automatically to the same GitHub Release.

The macOS build is currently unsigned/non-notarized (no Apple Developer account yet), so
Gatekeeper will flag it on first launch — right-click the app and choose "Open", or run
`xattr -cr /Applications/Reflectodoro.app` in Terminal.

The Linux build ships as a single `.AppImage` (not `.deb`/`.rpm`, so the app's built-in
updater keeps working the same way it does on Windows/macOS — see CLAUDE.md). Make it
executable before first run (`chmod +x Reflectodoro*.AppImage`); some distros also need
`libfuse2` installed for the AppImage runtime to mount itself. There's no
Gatekeeper-equivalent signing gate on Linux, so this is comparatively simpler than macOS's
workaround. Note the tray icon needs the AppIndicator/KStatusNotifierItem extension to
appear at all on a vanilla GNOME (especially under Wayland) — KDE/XFCE work out of the
box.

## Recommended IDE Setup

[VS Code](https://code.visualstudio.com/) + [Svelte](https://marketplace.visualstudio.com/items?itemName=svelte.svelte-vscode) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer).
