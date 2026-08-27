# Reflectodoro

A Pomodoro app whose real point is forcing a short self-reflection ("what did I do?") at
the end of every break, enforced via a hard-to-dismiss overlay. Windows and macOS, built
with Tauri 2 + SvelteKit.

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

Produces a `.exe` installer under `src-tauri/target/release/bundle/` (or a `.dmg`/`.app`
on macOS). Pushing a `vX.Y.Z` tag also triggers `.github/workflows/release.yml`, which
builds both platforms and publishes them automatically to the same GitHub Release.

The macOS build is currently unsigned/non-notarized (no Apple Developer account yet), so
Gatekeeper will flag it on first launch — right-click the app and choose "Open", or run
`xattr -cr /Applications/Reflectodoro.app` in Terminal.

## Recommended IDE Setup

[VS Code](https://code.visualstudio.com/) + [Svelte](https://marketplace.visualstudio.com/items?itemName=svelte.svelte-vscode) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer).
