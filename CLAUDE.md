# Reflectodoro

A Pomodoro app whose real point is forcing a short self-reflection ("what did I do?") at the end of every break, enforced via a hard-to-dismiss overlay. This is the **Windows MVP**; macOS/Android/iOS are planned later on the same stack, but not yet started. Sync across devices and auth are explicitly deferred — this build is fully local, single-device, no server/DB service of any kind.

Full original design exploration/rationale (superseded in details by this file where they conflict): `C:\Users\gursi\.claude\plans\i-want-to-create-glowing-abelson.md`.

## Stack

- **Shell**: Tauri 2.0 (Rust backend, `src-tauri/`)
- **Frontend**: SvelteKit (Svelte 5 runes) + TypeScript + Vite, `adapter-static` in SPA mode (`ssr = false` at the root layout)
- **Storage**: SQLite via `tauri-plugin-sql`, fully local — no server, no cloud, no serverless functions in this MVP
- Chosen deliberately over Electron+React Native: Tauri 2 supports Android/iOS from the same codebase, keeping the door open for the other platforms without committing to that work now.

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

On entering a break slot, the Rust backend spawns a second Tauri window (label `"overlay"`): fullscreen, undecorated, always-on-top, skip-taskbar. Its `close-requested` event is intercepted (`api.prevent_close()`) so Alt+F4 / the close button don't work. On Windows, a low-level keyboard hook (`src-tauri/src/hook.rs`, `WH_KEYBOARD_LL`) also suppresses Alt+Tab and the Windows key while it's open — but deliberately **not** Ctrl+Shift+Esc or Ctrl+Alt+Del, so Task Manager always stays reachable (see Kill switches below). This is a "strong deterrent, not an absolute lock" by design — Windows won't let an unprivileged app fully block Task Manager without admin/driver-level hooks, which also risks getting flagged as lockdown/malware-like software.

**Unlock formula** (`OverlayState::unlocked()` in `src-tauri/src/state.rs`), confirmed explicitly with the user and non-obvious from the feature list alone:

```
(time_expired AND reflection_entered) OR (breakit_matched AND reflection_entered)
```

`reflection_entered` (the "what did I do" textarea, submitted) is **always required** — there is no pure-timeout escape. The overlay can, by design, stay open well past the nominal 5-minute break if the user just doesn't reflect. `breakit_matched` is the only early-exit accelerant, and it still requires reflection too.

### breakit: random challenge, not a fixed phrase

Originally designed as a fixed word ("breakit") typed a configurable number of times. **Changed by the user** to: a random string (`src-tauri/src/breakit.rs::generate_challenge`), default 15 characters from `[a-zA-Z0-9]`, optionally including special characters, generated fresh for every new overlay and shown in the UI. The user must type it exactly (no paste — blocked via `paste`/`drop`/`contextmenu` handlers and Ctrl+V/Shift+Insert interception in the overlay page) to flip `breakit_matched`. Configurable in Settings (`breakit_length`, `breakit_include_special` in `app_setting`); being random rather than fixed means it can't become muscle memory.

## Inactivity / merge behavior

Because the overlay never auto-closes without a reflection, "what did I do in the last N pomodoros" can end up covering more than one slot. This is **not** special-cased in Rust — the frontend (`src/routes/overlay/+page.svelte`) queries SQLite on mount/update for whether the *previous* break slot's timestamp is already covered by a saved `reflection` row (`isSlotCovered` in `src/lib/db.ts`, a plain `slot_start_at = $1` equality check). If not, `saveReflection` inserts one `reflection` row per covered slot (same `created_at`/`text`, different `slot_start_at`) rather than bundling them into one row. This one check handles both real triggers: the grid reaching the next break boundary while unresolved, and the app being killed/crashed and relaunched with a pending slot.

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
  value TEXT                  -- breakit_length, breakit_include_special (work/break durations are fixed constants, not stored/configurable yet)
```

Migrations live in `src-tauri/src/db.rs` (`tauri-plugin-sql` migration list). Applied versions are tracked per-database in `_sqlx_migrations` and never re-run — so editing an already-shipped migration silently skips on any db that already applied it. Before the first tagged release, squashing/rewriting migrations freely is fine (nothing but local dev dbs has run them). From the first tagged release onward, always add a new versioned migration for schema/default changes instead.

## Kill switches (must always work, tested explicitly)

1. **Task Manager** (Ctrl+Shift+Esc) — never blocked, by design.
2. **Tray → Quit** — `std::process::exit(0)`, deliberately *not* `app.exit()` or a window `.close()`, so it can't get caught by the overlay's close-requested prevention.
3. **Ctrl+Alt+Shift+F12** — global shortcut, force-destroys the overlay, registered unconditionally (not just in dev builds).
4. **Dev mode** (`cfg!(debug_assertions)` by default, override with `POMODORO_DEV_MODE=0/1`): shows a visible "Close (DEV)" button on the overlay and skips installing the keyboard hook entirely.

## Known gotchas already hit in this codebase

- **`sql:default` (tauri-plugin-sql) does not include `execute`** — only `select`/`load`/`close`. Every INSERT/UPDATE needs the capability to also list `sql:allow-execute` explicitly, or writes silently no-op with no visible error.
- **Every window label needs to be in a capability's `"windows"` array, or it gets zero permissions** — not reduced, zero, including for the app's own custom commands. The default Tauri template scopes `capabilities/*.json` to `["main"]` only; the dynamically-created `"overlay"` window had to be added explicitly (`src-tauri/capabilities/default.json`).

Both bugs were silent (no thrown error visible to the user) — diagnosed by querying the SQLite file directly with the `sqlite3` CLI to confirm writes weren't landing, then reading the plugin's actual `permissions/*.toml` from the cargo registry source rather than assuming from memory.

## Not built yet / explicitly deferred

- Sync across devices, auth/pairing — deferred by user decision. If revisited, the recommended direction (not designed in detail) is an end-to-end-encrypted, short-TTL serverless mailbox rather than a permanent central DB.
- Configurable work/break durations (currently fixed 25/5 constants).
- macOS/Android/iOS builds.


## Best practices

- Refer best_pratices.md file best_pratices and checks for code changes.
- Try to make variable values configurable wherever feasible and appropriate.