# Reflectodoro

A Pomodoro app whose real point is forcing a short self-reflection ("what did I do?") at
the end of every break, enforced via a hard-to-dismiss overlay. Windows, macOS, Linux, and
Android, built with Tauri 2 + SvelteKit. iOS is planned on the same stack but not started.

[Download the latest release](https://github.com/techxapp/reflectodoro/releases/latest) &middot;
[Project page](https://reflectodoro.droplee.com)

See [CLAUDE.md](./CLAUDE.md) for architecture and data model.

## Pre-requisite

https://v2.tauri.app/start/prerequisites/

For Android, you additionally need the mobile prerequisites (Android Studio, an Android SDK
with API 36 + build-tools 36.0.0, the NDK, and a JDK 17): see
https://v2.tauri.app/start/prerequisites/#android. Set `ANDROID_HOME`/`NDK_HOME` per that
guide before running any `tauri android` command.

## Run it in development

```
npm install
npm run tauri dev
```

### Run the Android app in development

```
npm run tauri android dev
```

This launches the app in an Android emulator or a connected device over USB debugging.
Some behavior is native-only and can't be exercised this way from a desktop emulator alone
(the "Display over other apps" permission prompt, persistent break notifications, and audio
focus pausing) — see [CLAUDE.md](./CLAUDE.md)'s "Android" section for how those work.

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

The Android build produces signed release APKs (one per ABI: `arm64-v8a`, `armeabi-v7a`) via
CI when a `vX.Y.Z` tag is pushed, using a release keystore stored in repo secrets — see the
`android` job in `.github/workflows/release.yml`. To build a signed APK locally you need
your own keystore and to set `ANDROID_KEYSTORE_PATH`, `ANDROID_KEYSTORE_PASSWORD`,
`ANDROID_KEY_ALIAS`, and `ANDROID_KEY_PASSWORD` (see `src-tauri/gen/android/app/build.gradle.kts`'s
`signingConfigs` block); without those, the build falls back to an unsigned debug APK:

```
npm run tauri android build -- --apk --split-per-abi --target aarch64 armv7
```

Produces APKs under `src-tauri/gen/android/app/build/outputs/apk/`. On first install, the
app needs the "Display over other apps" permission granted manually in Android system
settings for the break screen to draw over other apps (it falls back to a notification
otherwise) — see [CLAUDE.md](./CLAUDE.md)'s "Android" section.

## Recommended IDE Setup

[VS Code](https://code.visualstudio.com/) + [Svelte](https://marketplace.visualstudio.com/items?itemName=svelte.svelte-vscode) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer).
