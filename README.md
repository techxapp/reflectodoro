# Reflectodoro

A Pomodoro app whose real point is forcing a short self-reflection ("what did I do?") at
the end of every break, enforced via a hard-to-dismiss overlay. Windows MVP, built with
Tauri 2 + SvelteKit.

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

Produces a `.msi` and `.exe` installer under `src-tauri/target/release/bundle/`.
Pushing a `vX.Y.Z` tag also triggers `.github/workflows/release.yml`, which builds and
publishes these automatically as a GitHub Release.

## Recommended IDE Setup

[VS Code](https://code.visualstudio.com/) + [Svelte](https://marketplace.visualstudio.com/items?itemName=svelte.svelte-vscode) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer).
