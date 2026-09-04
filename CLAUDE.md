# Reflectodoro

A Pomodoro app whose real point is forcing a short self-reflection ("what did I do?") at the end of every break, enforced via a hard-to-dismiss overlay. Ships as signed releases for **Windows, macOS, Linux, and Android** (see "Android" below); iOS is planned on the same stack but not yet started. The macOS build is currently unsigned/non-notarized (no Apple Developer account yet) and uses a best-effort media-pause toggle instead of Windows'/Linux's state-aware pause. Alt-Tab/Win-key suppression is Windows-only plus X11-only-on-Linux — never on macOS, never on Linux-under-Wayland (a protocol-level limitation, not a scope cut), and has no equivalent on Android — see "Media pause-on-break", "Android", and "Not built yet / explicitly deferred" below. Sync across devices and auth are explicitly deferred — this build is fully local, single-device, no server/DB service of any kind.


## Keeping this file current

This file drifts from the code easily — several sections have gone stale before (Android status, breakit's character set, where the media-pause toggle lives in the UI). **After any change that alters behavior described here** (a new platform capability, a moved/renamed UI control, a changed data model, a new deferred/not-deferred item, a new kill switch or gotcha), update the relevant section of this file in the same session/PR as the code change — don't let it wait for a later audit. When in doubt about whether a change is "major" enough to warrant an update, err toward updating; a short, accurate line beats a stale paragraph.

## Keeping the landing page's development journey current

`docs/index.html` has a "Development journey" section: a vertical timeline (`.timeline`) grouped **by week** (Monday–Sunday, ISO week), summarizing real work from git history. Each `.timeline-week` block's `.timeline-date` reads "Week of *start* &ndash; *end*, YYYY" (cross-month weeks read "Week of Aug 31 &ndash; Sep 6, 2026"; use just the day range plus one year when both ends fall in the same month, e.g. "Week of Aug 24 &ndash; 30, 2026").

**Whenever you make a commit (or a batch of commits in one session) that a user-facing changelog would care about** — new features, fixes, platform support changes, UI changes — update `docs/index.html` in the same session:

1. Compute the Monday–Sunday week today's date falls in (see the `currentDate` context).
2. If the *topmost* `.timeline-week` in `.timeline` already covers that week, merge into it: fold the new work into its existing bullets by theme (don't just append a raw commit-message list), rewriting bullets as needed so the week reads as one cohesive summary rather than a per-commit log.
3. If it covers an earlier week, insert a new `.timeline-week` block at the *top* of `.timeline` (most recent week first) for the current week, with merged/themed bullets under `.timeline-items`.

Skip noise: "Bump version to X.Y.Z" commits and `Merge branch ...` commits are deliberately excluded — don't add them. Keep each week to a handful of merged bullets (roughly 4-8), grouped by theme (e.g. "Android release: ...", "Fixed X, Y, and Z") rather than one bullet per commit — the existing weeks in the file are the pattern to match.

## Stack

- **Shell**: Tauri 2.0 (Rust backend, `src-tauri/`)
- **Frontend**: SvelteKit (Svelte 5 runes) + TypeScript + Vite, `adapter-static` in SPA mode (`ssr = false` at the root layout)
- **Storage**: SQLite via `tauri-plugin-sql`, fully local — no server, no cloud, no serverless functions in this MVP
- Chosen deliberately over Electron+React Native: Tauri 2 supports Android/iOS from the same codebase. Android is now a real signed release target built on this (see "Android" below); iOS remains not started, kept open by the same choice.

## Run it

```
npm run tauri dev
```

Rust changes trigger a rebuild automatically. Capability/permission JSON changes (`src-tauri/capabilities/*.json`) also require a rebuild to take effect (they're compiled in via `tauri-build`, not read at runtime).

## Core mechanic: wall-clock grid, not a rolling timer

Sessions are pinned to fixed clock boundaries in the user's local timezone, not "25 minutes from whenever the app started":

```
:00–:25 work   :25–:30 break   :30–:55 work   :55–:00 break
```

Implemented as a pure function `grid::slot_for(now) -> Slot` (`src-tauri/src/grid.rs`), unit tested. The scheduler (`run_scheduler` in `src-tauri/src/lib.rs`) computes the exact `Duration` until the next boundary and sleeps precisely to it — no polling loop.

## The overlay and its unlock formula

On entering a break slot, the Rust backend spawns a second Tauri window (label `"overlay"`): fullscreen, undecorated, always-on-top, skip-taskbar. Its `close-requested` event is intercepted (`api.prevent_close()`) so Alt+F4 / the close button don't work. On Windows, a low-level keyboard hook (`src-tauri/src/hook.rs`, `WH_KEYBOARD_LL`) also suppresses Alt+Tab and the Windows key while it's open; on Linux, the same file's `linux_impl` does the equivalent via an X11 `XGrabKey` — but only on X11 sessions, since Wayland gives no client a portable way to suppress global shortcuts (a deliberate protocol-level security property, not something this app can work around) — deliberately **not** Ctrl+Shift+Esc or Ctrl+Alt+Del, so Task Manager always stays reachable (see Kill switches below). This is a "strong deterrent, not an absolute lock" by design — Windows won't let an unprivileged app fully block Task Manager without admin/driver-level hooks, which also risks getting flagged as lockdown/malware-like software.

**Unlock formula** (`OverlayState::unlocked()` in `src-tauri/src/state.rs`), confirmed explicitly with the user and non-obvious from the feature list alone:

```
(time_expired AND reflection_entered) OR (breakit_matched AND reflection_entered)
```

`reflection_entered` (the "what did I do" textarea, submitted) is **always required** — there is no pure-timeout escape. The overlay can, by design, stay open well past the nominal 5-minute break if the user just doesn't reflect. `breakit_matched` is the only early-exit accelerant, and it still requires reflection too.

### breakit: random challenge, not a fixed phrase

Originally designed as a fixed word ("breakit") typed a configurable number of times. **Changed by the user** to: a random string (`src-tauri/src/breakit.rs::generate_challenge`), default 15 characters from a reduced alphanumeric set (`ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789`) that deliberately excludes visually ambiguous characters — `0`/`O` and `1`/`l`/`I` — which render near-identically in several monospace fonts, including the Android overlay's plain `monospace` fallback; optionally also including special characters (`!@#$%^&*()-_=+[]{}`). Generated fresh for every new overlay and shown in the UI. The user must type it exactly (no paste — blocked via `paste`/`drop`/`contextmenu` handlers and Ctrl+V/Shift+Insert interception in the overlay page) to flip `breakit_matched`. Configurable in Settings (`breakit_length`, `breakit_include_special` in `app_setting`, length clamped to `[4, 64]`); being random rather than fixed means it can't become muscle memory.

### Reflection field pre-fill

The overlay's reflection textarea pre-fills with the text of the most recently saved reflection (`prefillReflection()` in `src/routes/overlay/+page.svelte`, via `getLastReflectionText` in `src/lib/db.ts`) whenever a new break slot starts and no reflection has been entered for it yet — a UI convenience only, shown with a "Pre-filled with your last entry" hint; nothing is written to the database until the user actually submits.

## Inactivity / merge behavior

Because the overlay never auto-closes without a reflection, "what did I do in the last N pomodoros" can end up covering more than one slot. This is **not** special-cased in Rust — the frontend (`src/routes/overlay/+page.svelte`) queries SQLite on mount/update for whether the *previous* break slot's timestamp is already covered by a saved `reflection` row (`isSlotCovered` in `src/lib/db.ts`, a plain `slot_start_at = $1` equality check). If not, `saveReflection` inserts one `reflection` row per covered slot (same `created_at`/`text`, different `slot_start_at`) rather than bundling them into one row. This one check handles both real triggers: the grid reaching the next break boundary while unresolved, and the app being killed/crashed and relaunched with a pending slot.

Android's native overlay (see "Android" below) has no DB access of its own, so it can't run this check client-side — `src-tauri/src/native_overlay.rs` mirrors `findMissedSlots`/`saveReflection` in Rust (`find_missed_slots`/`save_reflection`, byte-for-byte same ISO-string/lookback-cap logic) and runs it directly against a second SQLite connection at submit time. The heading's "last N pomodoros" wording is likewise mirrored rather than shared: computed once into `AppState.missed_slot_count` right before the overlay is shown (`native_overlay::refresh_missed_slot_count`, called from `overlay::spawn_or_update_overlay`'s Android arm) and read by `native_overlay.html`'s `render()` — since nothing else can write to `reflection` while the overlay has the screen, the count computed at open time is guaranteed to match what `find_missed_slots` recomputes at actual submit time.

## Media pause-on-break

`src-tauri/src/media.rs`, gated on `app_setting.media_pause_on_break_enabled`. Toggled from a button on the **main window** (`src/routes/+page.svelte`, below the Pomodoro mode toggle) — moved out of Settings since it's a frequently-adjusted control, not a one-time preference. Four entirely different mechanisms per platform, all "strong deterrent, not an absolute lock" like the rest of the overlay:

- **Windows**: queries System Media Transport Controls (SMTC) for every registered session and pauses only the ones actually playing (`GlobalSystemMediaTransportControlsSessionManager`). State-aware — never resumes something that was already paused.
- **macOS**: no public API can query playback state the way SMTC does (only the private, undocumented `MediaRemote.framework` can), so this posts a synthetic hardware Play/Pause media-key event instead (`NX_KEYTYPE_PLAY`, via an AppKit `NSEvent` bridge — see `media.rs`'s doc comment for why that needs AppKit and not plain Core Graphics). This is a **blind toggle**: it can incorrectly *resume* media that was already paused before the break started. Explicit, confirmed tradeoff — not a bug.
- **Linux**: queries MPRIS (Media Player Remote Interfacing Specification, over the session D-Bus) via the `mpris` crate and pauses only players actually playing (`PlaybackStatus::Playing`). State-aware like Windows, not a toggle like macOS — MPRIS doesn't have macOS's private-API wall, so Linux reaches better parity with Windows here than macOS currently can.
- **Android**: no cross-app API exists to list playback sessions and their state the way SMTC/MPRIS do, so this requests transient audio focus (`AudioManager.requestAudioFocus`, `AUDIOFOCUS_GAIN_TRANSIENT`) via the Android bridge (`NativeBridgePlugin.kt::pauseAudioFocus`/`resumeAudioFocus`, called from `media.rs`'s `android_impl` module) instead of a query-then-pause. Any well-behaved playing app receives `AUDIOFOCUS_LOSS_TRANSIENT` and pauses itself per the platform's audio-focus contract. Not a blind toggle like macOS: `close_overlay` abandons the same `AudioFocusRequest` on break-end, which only signals apps that actually ducked for *that* request — it can't resume media that was already paused before the break started.

**Guard, macOS only**: narrows (does not eliminate) the toggle's failure mode for the specific case of "we already toggled it paused this break, and toggling again would resume it." Tracks `LAST_MEDIA_TOGGLE_AT` (in-memory, set the instant the toggle fires, persisted to `app_setting.last_toggle_time` via the `media-toggle://recorded` event so it survives a crash/relaunch mid-break) against `LAST_WELLNESS_CHECK_AT` (most recent `wellness_check.created_at`, synced from the frontend on boot and after every completed check-in). Skips the toggle iff a toggle already happened and no completed check-in has happened since. Uses `wellness_check.created_at` as the "cycle completed" signal even though check-in is skippable (closing or auto-closing it saves nothing) — a user who routinely skips check-ins will see the guard's reset stop firing after their first break. Confirmed with the user as an accepted tradeoff (the skip-proof alternative, `reflection.created_at`, was considered and explicitly not chosen).

## Data model

No `pomodoro_session` table — deliberately. Slot identity/boundaries are fully determined by computing from a timestamp against the fixed grid, so there's nothing session-shaped to persist.

```sql
reflection
  id INTEGER PK
  created_at TEXT
  slot_start_at TEXT          -- one row per covered slot; usually 1 row per reflection, sometimes 2 when merged
  text TEXT

daily_task_list              -- "Most Important Tasks Today", shared between main window and overlay
  date TEXT PK                -- local date, 'YYYY-MM-DD'
  content TEXT
  updated_at TEXT

app_setting
  key TEXT PK
  value TEXT                  -- breakit_length, breakit_include_special, last_toggle_time (macOS media-toggle guard, see below; row absent until first toggle) (work/break durations are fixed constants, not stored/configurable yet)
```

Migrations live in `src-tauri/src/db.rs` (`tauri-plugin-sql` migration list). Applied versions are tracked per-database in `_sqlx_migrations` and never re-run — so editing an already-shipped migration silently skips on any db that already applied it. Before the first tagged release, squashing/rewriting migrations freely is fine (nothing but local dev dbs has run them). From the first tagged release onward, always add a new versioned migration for schema/default changes instead.

## Kill switches (must always work, tested explicitly)

These are desktop-specific (Windows/macOS/Linux); Android has no tray, no `Alt+F4`-style close, and no global-shortcut plugin (see "Android" below) — its enforcement relies on the OS's own notification/overlay-permission controls instead.

1. **Task Manager** (Ctrl+Shift+Esc on Windows; Activity Monitor / Cmd+Option+Esc Force Quit on macOS) — never blocked, by design.
2. **Tray → Quit** — `std::process::exit(0)`, deliberately *not* `app.exit()` or a window `.close()`, so it can't get caught by the overlay's close-requested prevention.
3. **Ctrl+Alt+Shift+F12** (Windows/Linux) / **Cmd+Option+Shift+F12** (macOS) — global shortcut, force-destroys the overlay, registered unconditionally (not just in dev builds). Platform-selected at runtime via `cfg!(target_os = "macos")` in `setup_dev_kill_switch` (`src-tauri/src/lib.rs`); the Settings UI label is driven by the `current_os` command so it always matches what's actually registered.
4. **Dev mode** (`cfg!(debug_assertions)` by default, override with `POMODORO_DEV_MODE=0/1`): shows a visible "Close (DEV)" button on the overlay and skips installing the keyboard hook entirely.
5. **Desktop-only: the reflection-save escape hatch.** If `saveReflection`/`mark_reflection_entered` fails twice in a row for the current overlay occurrence (a locked or full DB, most concretely), the overlay reveals a "Close break screen anyway" button (`src/routes/overlay/+page.svelte`). It calls `close_after_save_failure` (`commands.rs`), which is gated server-side on `OverlayState.save_failure_count` actually having reached that threshold — not reachable by a single direct `invoke` call the way `mark_reflection_entered` is, though (like every other command here) it's not a real boundary against a devtools-capable user. Exists so a genuinely broken DB write can't leave the user with no way out short of #1-#4 above; behaves like a force-close (no reflection recorded, no check-in opens).

## Android

Android is a real, signed release target, not experimental scaffolding — `src-tauri/gen/android/` is fully generated and committed, with custom Kotlin sources beyond the Tauri template. Because the generated Android WebView shell doesn't get the same low-level window control as desktop (no separate undecorated always-on-top window, no `WH_KEYBOARD_LL`-style hook), break enforcement is implemented natively rather than by reusing the desktop overlay window:

- **Native overlay + notification fallback**: `NativeOverlayManager.kt` draws a system overlay over other apps when the user has granted "Display over other apps" permission (`canDrawOverlays`/`requestDrawOverlaysPermission`, exposed via `NativeBridgePlugin.kt`); without that permission it falls back to a break notification instead.
- **`NativeBridgePlugin.kt`** (`src-tauri/gen/android/app/src/main/java/com/reflectodoro/app/`, a `@TauriPlugin`) is the Rust↔Kotlin bridge, wrapped on the Rust side by `src-tauri/src/android_bridge.rs::AndroidBridge<Wry>`. It exposes `ping`, `startForegroundService`/`stopForegroundService`, `triggerBreakScreen`, `cancelBreakNotification`, `updateNativeOverlay`, `initNativeOverlayChannel`, `canDrawOverlays`/`requestDrawOverlaysPermission`, `canScheduleExactAlarms`/`requestScheduleExactAlarmPermission`, and `pauseAudioFocus`/`resumeAudioFocus` (see "Media pause-on-break" above).
- **Scheduling survives process death and reboot**: `BreakSchedulerService.kt` runs as a foreground service; `BreakScheduling.kt` independently mirrors the `grid::slot_for` boundary rule in Kotlin to compute the next wake time via `AlarmManager.setAlarmClock` — deliberately *not* `setExactAndAllowWhileIdle`, since only `setAlarmClock` is exempt from Android 10+ background-activity-launch restrictions (confirmed on-device). Side effect: a persistent alarm-clock icon in the status bar whenever Pomodoro mode is on. AOSP docs describe `setAlarmClock` as exempt from the Android 12+ `SCHEDULE_EXACT_ALARM` permission requirement entirely, but that's been disproven on a real device (a HONOR/MagicOS Android 12 unit threw a `SecurityException` from it, crashing the app on every launch) — so `scheduleNextAlarm` now gates the call on `AlarmManager.canScheduleExactAlarms()` and falls back to a plain inexact `alarmManager.set()` (losing the foreground-launch exemption, but not crashing) when it isn't granted. `SCHEDULE_EXACT_ALARM` is declared in the manifest, and onboarding (`src/routes/onboarding/+page.svelte`) and Settings both surface a grant/re-check card for it, mirroring the overlay-permission one. `BootCompletedReceiver.kt` restarts the scheduler service on `ACTION_BOOT_COMPLETED` — this is Android's own reboot-recovery mechanism and is unrelated to the desktop login-item autostart below. Pomodoro-mode-enabled state itself still isn't persisted in Rust's `POMODORO_ENABLED` (it resets to its default of `true` on every fresh process start, same as desktop), but `commands::set_enabled` also mirrors the toggle into a plain Android `SharedPreferences` flag (`NativeBridgePlugin.kt::persistPomodoroEnabled`) purely so this receiver — which runs before any Tauri/Rust runtime exists in a freshly booted process — has something to check first; `BootCompletedReceiver` reads it (`PomodoroEnabledPref.isEnabled`, defaulting to enabled) and skips restarting the service entirely for a user who had deliberately turned Pomodoro mode off. `BreakAlarmReceiver`'s own "was the scheduler already running" check (`MainActivity.isSchedulerAlive()`) is a genuine liveness signal, not a one-time flag: `run_scheduler` reports a heartbeat (`reportSchedulerHeartbeat`) on every loop iteration (at least every `ANDROID_POLL_INTERVAL`), so a scheduler task that dies without taking the whole process down with it is still detected as dead rather than being permanently mistaken for alive because *some* Activity existed once.
- **`POST_NOTIFICATIONS`** (denied by default on API 33+; without it, the break notification and the process-recovery wake notification both silently don't show) is requested through `@tauri-apps/plugin-notification`'s own `isPermissionGranted`/`requestPermission` JS API — deliberately *not* a custom `NativeBridgePlugin.kt` command the way the overlay/exact-alarm grants are, since the standard plugin already handles this runtime permission correctly. Onboarding offers the grant on first run; Settings mirrors it with its own check/request pair (same `isAndroid`-gated card pattern as the overlay/exact-alarm/persistent-notification controls) for anyone who skipped it there or revoked it later.
- **A second, direct SQLite connection on Android only**: the native overlay is a plain WebView with no Tauri IPC bridge, so it needs its own path to write a reflection row directly from Rust. `Cargo.toml`'s `[target.'cfg(target_os = "android")'.dependencies]` pulls in `sqlx` for this.
- **`android:allowBackup="false"`** (`AndroidManifest.xml`) is explicit, not left at the platform default (`true`) — this app is "fully local, single-device" by design (see the top of this file), and `pomodoro.db` is the entire reason that matters: every reflection ever written. Auto Backup would otherwise silently upload it to the user's Google Drive and restore it onto any device signed into that account.
- **Break-end detection polls wall-clock time on Android**: `run_scheduler` (`src-tauri/src/lib.rs`) normally waits for the next grid boundary with a single `tokio::time::sleep`, which schedules against `Instant`/`CLOCK_MONOTONIC` — a clock that does not advance while the CPU is actually suspended (unlike `CLOCK_BOOTTIME`). A break spanning a real Doze/deep-suspend period could leave that sleep never firing at the right wall-clock instant, since `BreakAlarmReceiver`'s `AlarmManager.setAlarmClock` chain only guarantees a brief wake window, not one long enough for a large monotonic deficit to fully elapse — symptom: a reflection submitted mid-break, but the overlay never auto-closes once time is actually up. `ANDROID_POLL_INTERVAL` (20s) caps each sleep on Android so the loop re-checks `grid::slot_for(Local::now())` from the wall clock frequently instead, self-correcting on whatever brief wake windows the alarm chain provides. Desktop doesn't need this for the same wake-cadence reason — an actual laptop suspend is instead caught by `SUSPEND_GAP_THRESHOLD` (`lib.rs`) once its single long sleep does eventually return. That threshold (120s) only governs re-evaluating the current phase, though; whether an *unresolved* overlay actually gets force-closed after a suspend gap is separately bounded by `OVERLAY_AUTO_CLOSE_MINUTES` (the same user-configured grace period `schedule_auto_close` applies to an ordinary unresolved break) — same rule on both platforms now, so a suspend can't force-close on a shorter fuse than just sitting at the desk not responding would. `OVERLAY_AUTO_CLOSE_MINUTES` and the check-in window's own `checkin_auto_close_minutes` are both clamped to `[1, 60]` (Rust's `set_overlay_auto_close_minutes` and the Settings page for both) — an unbounded value would otherwise defeat the documented last-resort force-close outright (`u32::MAX` minutes), and on the check-in side a large-enough value silently fires its plain JS `setTimeout` *immediately* instead of waiting, since it wraps past `setTimeout`'s ~24.8-day (2³¹ ms) ceiling.
- **The native overlay carries its own wall-clock watchdog, independent of the Rust channel**: `NativeOverlayManager.kt`'s `show()` schedules a fixed 20-minute self-removal (`WATCHDOG_TIMEOUT_MS`) the moment the WindowManager view is added, checked by instance identity so it can't tear down a later break's overlay. It deliberately does not go through `overlayChannel`/`native_overlay.rs::install_channel` to trigger this — if that channel setup silently failed (every `OverlayJsBridge` method becomes a no-op in that case), routing the watchdog through the same broken channel would be defeated by the exact failure it exists to guard against. Rust's `OverlayState` never learns this fired (no reflection submitted, no check-in opens), same as any other force-close with nothing submitted.
- **Desktop-only plugins are excluded on mobile**: `tauri-plugin-global-shortcut`, `tauri-plugin-autostart`, `tauri-plugin-updater`, `tauri-plugin-process`, and `tauri-plugin-single-instance` are all gated out via `[target.'cfg(not(any(target_os = "android", target_os = "ios")))'.dependencies]` in `Cargo.toml`. So desktop login-item autostart (`commands.rs::set_autostart_enabled`, which returns "not supported on this platform" off-desktop) and the Ctrl+Alt+Shift+F12 kill switch have no Android equivalent — `settings/+page.svelte` hides the force-close-shortcut section on Android (`isAndroid`) and instead shows Android-only controls for the overlay-permission, exact-alarm-permission, and persistent-break-notification settings. `about/+page.svelte`'s "Check for updates" button is gated the same way (`current_os` → `isAndroid`), since calling the excluded updater plugin's `check()` throws `"updater.check not allowed. Plugin not found"` at runtime rather than failing at build time — the static JS import itself doesn't error, only invoking it does.
- **Signed CI releases**: `.github/workflows/release.yml`'s `android` job sets up JDK 17 + the Android SDK/NDK, decodes a signing keystore from the `ANDROID_KEYSTORE_BASE64` secret, runs `npm run tauri android build -- --apk --split-per-abi --target aarch64 armv7`, and uploads signed release APKs to the GitHub Release.
- **iOS has none of the above** — it's excluded from the same Cargo `cfg` groups as Android but has zero implementation behind it. "Planned but not yet started" (the phrase this doc used to apply to Android as a whole) is accurate only for iOS now.

## Known gotchas already hit in this codebase

- **`sql:default` (tauri-plugin-sql) does not include `execute`** — only `select`/`load`/`close`. Every INSERT/UPDATE needs the capability to also list `sql:allow-execute` explicitly, or writes silently no-op with no visible error.
- **Every window label needs to be in a capability's `"windows"` array, or it gets zero permissions** — not reduced, zero, including for the app's own custom commands. The default Tauri template scopes `capabilities/*.json` to `["main"]` only; the dynamically-created `"overlay"` window had to be added explicitly (`src-tauri/capabilities/default.json`).

Both bugs were silent (no thrown error visible to the user) — diagnosed by querying the SQLite file directly with the `sqlite3` CLI to confirm writes weren't landing, then reading the plugin's actual `permissions/*.toml` from the cargo registry source rather than assuming from memory.

- **Linux tray icon needs a desktop-environment extension to appear at all** — vanilla GNOME (especially under Wayland) has no tray/status-icon area by default; it requires the "AppIndicator and KStatusNotifierItem Support" extension. KDE and XFCE show the tray icon out of the box. Nothing in `setup_tray` (`src-tauri/src/lib.rs`) can fix this from the app side — it's a desktop-environment configuration gap, not a bug.

## Not built yet / explicitly deferred

- Sync across devices, auth/pairing — deferred by user decision. If revisited, the recommended direction (not designed in detail) is an end-to-end-encrypted, short-TTL serverless mailbox rather than a permanent central DB.
- Configurable work/break durations (currently fixed 25/5 constants).
- iOS builds — Android ships as a signed release APK already (see "Android" above); iOS has no implementation at all yet, only `cfg` exclusions alongside Android's.
- **macOS Alt-Tab/Win-key-equivalent suppression** — `src-tauri/src/hook.rs`'s `WH_KEYBOARD_LL` hook is Windows-only. On macOS this is a no-op: the nearest equivalent (`CGEventTap`) requires the user to grant Accessibility permission, real UX friction Apple scrutinizes apps for. The overlay's unlock formula still fully enforces itself without it — this was always a deterrent, not the mechanism holding the lock together.
- **Wayland Alt-Tab/Win-key-equivalent suppression** — `hook.rs`'s `linux_impl` implements real suppression via `XGrabKey`, but only on X11 sessions (detected at runtime via `XDG_SESSION_TYPE`/`WAYLAND_DISPLAY`). Wayland sessions get the same no-op as macOS, deliberately: no client can suppress global shortcuts under Wayland by design, so an X11-only implementation is the ceiling here, not a gap to fast-follow on. Same "deterrent, not the lock itself" reasoning as macOS applies.
- **State-aware macOS media pause** — `src-tauri/src/media.rs` pauses media on Windows (via SMTC) and Linux (via MPRIS) with true query-then-pause, but the macOS path is a best-effort toggle (see "Media pause-on-break" below). A state-aware macOS implementation is possible later via the private, undocumented `MediaRemote.framework` (the technique tools like `nowplaying-cli` use), which would eliminate the toggle's known limitation — not built yet: real scope (a new Rust module) and depends on an API Apple could change without notice.
- **macOS code signing / notarization** — no Apple Developer account yet, so the macOS build is unsigned. Gatekeeper blocks it on first launch (see README's workaround). Revisit if/when an account is obtained; the release pipeline (`.github/workflows/release.yml`) is structured so signing env vars can be added to the macOS matrix leg without restructuring it.
- **Linux `.deb`/`.rpm` packages** — the Linux release deliberately ships AppImage only (`src-tauri/tauri.linux.conf.json`). `tauri-plugin-updater` only auto-updates AppImage on Linux, so adding deb/rpm would need a separate, unsigned distribution/update story via system package managers before it's worth doing — not a technical blocker, just undone.


## Best practices

- Refer to `best_practices.md` (repo root) for the Tauri/Svelte async-and-race-condition checklist — treat `invoke`/`listen`/Svelte reactivity as concurrent and unordered, check it before and after touching async code.
- Try to make variable values configurable wherever feasible and appropriate.
- Update this file (`CLAUDE.md`) alongside any major change — see "Keeping this file current" at the top.