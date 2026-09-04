import Database from "@tauri-apps/plugin-sql";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

let dbPromise: ReturnType<typeof Database.load> | null = null;

function getDb() {
  if (!dbPromise) {
    const attempt = Database.load("sqlite:pomodoro.db");
    dbPromise = attempt;
    // If this load rejects (most commonly: a pending migration can't apply,
    // e.g. an app_setting row from a cross-version import colliding with a
    // migration seeding the same key -- see db.rs's migration comments),
    // clear the cache instead of leaving a rejected promise memoized here
    // forever. Without this, EVERY function in this module throws for the
    // rest of the process's life after the first failure, with the app
    // never even retrying -- turning one transient or fixable failure into
    // a permanent, silent brick. Clearing it means the next call at least
    // gets a fresh attempt (and a fresh, catchable rejection) instead of the
    // same cached one forever.
    //
    // Only clear the cache if `attempt` is still the current `dbPromise` --
    // a later call to `getDb()` could already have replaced it with a fresh
    // attempt of its own by the time this rejection handler runs, and this
    // must not clobber that newer attempt out from under it.
    attempt.catch(() => {
      if (dbPromise === attempt) dbPromise = null;
    });
  }
  return dbPromise;
}

export interface ReflectionRow {
  id: number;
  created_at: string;
  slot_start_at: string;
  text: string;
}

/** A run of rows for display purposes: consecutive break slots (30 minutes
 * apart, no gap) carrying identical text. Computed client-side from the flat
 * row list rather than stored -- deliberately NOT keyed off `created_at`
 * (which rows share when saved together by saveReflection's slot-merge
 * logic), because editing one row's text should immediately split it off
 * from unedited neighbors, not stay bundled under the original save event. */
export interface ReflectionCluster {
  rows: ReflectionRow[];
}

export function localDateStamp(d: Date = new Date()): string {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

/**
 * Slot identity is compared by canonical (UTC) ISO string. Rust hands us
 * `current_slot_start` as an RFC3339 string with a local offset (e.g.
 * "...+05:30"); re-serializing through Date always yields the same UTC form
 * regardless of which side (Rust or JS date-math) produced the original
 * string, so equality checks against the DB are reliable.
 */
export function canonicalIso(iso: string): string {
  return new Date(iso).toISOString();
}

export function previousSlotIso(slotIso: string): string {
  return new Date(new Date(slotIso).getTime() - 30 * 60 * 1000).toISOString();
}

/** Was the given slot (by its canonical ISO start timestamp) already reflected on? */
export async function isSlotCovered(slotStartIso: string): Promise<boolean> {
  const db = await getDb();
  const rows = await db.select<{ n: number }[]>(
    `SELECT COUNT(*) as n FROM reflection WHERE slot_start_at = $1`,
    [slotStartIso],
  );
  return (rows[0]?.n ?? 0) > 0;
}

/**
 * First-ever-run timestamp, written once and never updated. Bounds how far
 * back `findMissedSlots` will cascade -- without it, a brand new install's
 * very first break would walk back a full day asking about pomodoros that
 * never happened, before the app even existed on the machine.
 */
async function ensureFirstRunMarker(): Promise<string> {
  const db = await getDb();
  const rows = await db.select<{ value: string }[]>(
    `SELECT value FROM app_setting WHERE key = 'first_run_at'`,
  );
  if (rows[0]?.value) return rows[0].value;
  const now = new Date().toISOString();
  await db.execute(
    `INSERT INTO app_setting (key, value) VALUES ('first_run_at', $1) ON CONFLICT(key) DO NOTHING`,
    [now],
  );
  return now;
}

/**
 * Android-only: whether the user has been through the permissions
 * onboarding screen (src/routes/onboarding/+page.svelte) at least once.
 * Written once and never updated, same pattern as the first-run marker
 * above -- set on "Continue" regardless of whether the permissions were
 * actually granted, so a user who declines isn't nagged on every launch.
 */
export async function isOnboardingCompleted(): Promise<boolean> {
  const db = await getDb();
  const rows = await db.select<{ value: string }[]>(
    `SELECT value FROM app_setting WHERE key = 'android_onboarding_completed'`,
  );
  return rows[0]?.value === "1";
}

export async function markOnboardingCompleted(): Promise<void> {
  const db = await getDb();
  await db.execute(
    `INSERT INTO app_setting (key, value) VALUES ('android_onboarding_completed', '1') ON CONFLICT(key) DO NOTHING`,
  );
}

/**
 * Off by default -- the OS registration state (Windows Run key) is the sole
 * record of the user's choice, toggled only from the Settings page. Nothing
 * auto-enables this on first run.
 */
export async function getAutostartEnabled(): Promise<boolean> {
  return invoke<boolean>("get_autostart_enabled");
}

export async function setAutostartEnabled(enabled: boolean): Promise<void> {
  await invoke("set_autostart_enabled", { enabled });
}

/**
 * Walks backward 30 minutes at a time from `currentSlotIso` (inclusive),
 * collecting every consecutive slot that has no reflection yet -- this is
 * what lets "what did I do" cascade across any number of missed pomodoros,
 * including overnight/previous-day gaps (sleep included), not just a single
 * prior slot. Bounded by the first-run marker (see above) and, defensively,
 * a hard cap so nothing can loop back forever.
 */
const MAX_MISSED_SLOT_LOOKBACK = 96; // ~2 days of break slots, defensive-only

export async function findMissedSlots(currentSlotIso: string): Promise<string[]> {
  const firstRunAtMs = new Date(await ensureFirstRunMarker()).getTime();
  const slots: string[] = [canonicalIso(currentSlotIso)];
  let cursor = currentSlotIso;
  for (let i = 0; i < MAX_MISSED_SLOT_LOOKBACK; i++) {
    const prev = previousSlotIso(cursor);
    if (new Date(prev).getTime() < firstRunAtMs) break;
    if (await isSlotCovered(prev)) break;
    slots.unshift(prev);
    cursor = prev;
  }
  return slots;
}

/** One DB row per covered slot (same created_at/text) -- so "missed" pomodoros are individually recorded, not bundled into one array field. */
export async function saveReflection(coveredSlots: string[], text: string): Promise<void> {
  const db = await getDb();
  const createdAt = new Date().toISOString();
  for (const slot of coveredSlots) {
    await db.execute(
      `INSERT INTO reflection (created_at, slot_start_at, text) VALUES ($1, $2, $3)`,
      [createdAt, slot, text],
    );
  }
}

/** Text of the most recently saved reflection row, across all slots -- used
 * to pre-populate (never auto-save) the overlay's reflection textarea so the
 * user can see/tweak what they wrote last time instead of starting blank.
 * Returns null when no reflection has ever been saved (fresh install). */
export async function getLastReflectionText(): Promise<string | null> {
  const db = await getDb();
  const rows = await db.select<{ text: string }[]>(
    `SELECT text FROM reflection ORDER BY id DESC LIMIT 1`,
  );
  return rows[0]?.text ?? null;
}

/** The reflection row most recently saved for a given slot -- throws rather than returning null/undefined so a missing row (which shouldn't happen; the check-in popup only ever opens after a reflection was saved) fails loudly instead of silently no-opping. */
export async function getReflectionIdForSlot(slotStartIso: string): Promise<number> {
  const db = await getDb();
  const rows = await db.select<{ id: number }[]>(
    `SELECT id FROM reflection WHERE slot_start_at = $1 ORDER BY id DESC LIMIT 1`,
    [slotStartIso],
  );
  if (!rows[0]) throw new Error(`no reflection row found for slot ${slotStartIso}`);
  return rows[0].id;
}

export interface WellnessCheckValues {
  relaxedEyes: boolean;
  exercise: boolean;
  drankWater: boolean;
  washroom: boolean;
}

/** Returns the `created_at` it saved, so callers needing that exact value
 * (e.g. syncing the macOS media-toggle guard) don't take a second, possibly
 * drifting, timestamp reading of their own. */
export async function saveWellnessCheck(
  reflectionId: number,
  values: WellnessCheckValues,
): Promise<string> {
  const db = await getDb();
  const createdAt = new Date().toISOString();
  await db.execute(
    `INSERT INTO wellness_check (reflection_id, relaxed_eyes, exercise, drank_water, washroom, created_at)
     VALUES ($1, $2, $3, $4, $5, $6)`,
    [
      reflectionId,
      values.relaxedEyes ? 1 : 0,
      values.exercise ? 1 : 0,
      values.drankWater ? 1 : 0,
      values.washroom ? 1 : 0,
      createdAt,
    ],
  );
  return createdAt;
}

export interface WellnessSummary {
  total: number;
  relaxedEyes: number;
  exercise: number;
  drankWater: number;
  washroom: number;
}

/** Totals for each wellness check-in item on a given day, keyed off wellness_check's own created_at (when it was saved), same grouping basis getReflectionsForDate uses for reflections. */
export async function getWellnessSummaryForDate(dateStamp: string): Promise<WellnessSummary> {
  const db = await getDb();
  const rows = await db.select<
    {
      total: number;
      relaxed_eyes: number | null;
      exercise: number | null;
      drank_water: number | null;
      washroom: number | null;
    }[]
  >(
    `SELECT COUNT(*) as total,
            SUM(relaxed_eyes) as relaxed_eyes,
            SUM(exercise) as exercise,
            SUM(drank_water) as drank_water,
            SUM(washroom) as washroom
     FROM wellness_check
     WHERE date(created_at, 'localtime') = $1`,
    [dateStamp],
  );
  const row = rows[0];
  return {
    total: row?.total ?? 0,
    relaxedEyes: row?.relaxed_eyes ?? 0,
    exercise: row?.exercise ?? 0,
    drankWater: row?.drank_water ?? 0,
    washroom: row?.washroom ?? 0,
  };
}

/**
 * `date(created_at, 'localtime')`, not bare `date(created_at)`: `created_at`
 * is stored as a UTC ISO string (`new Date().toISOString()`, see
 * saveReflection), so a bare `date()` returns its *UTC* calendar date --
 * while `dateStamp` here always comes from `localDateStamp()`, the *local*
 * calendar date. In UTC+5:30, every reflection written before 05:30 local
 * was filed under the previous day; in UTC-5, every evening reflection
 * jumped to tomorrow. `getWellnessSummaryForDate` above had the identical
 * bug -- its wellness tiles were computed over a different row set than the
 * reflections shown beside them on the same Entries page. SQLite's
 * `'localtime'` modifier delegates to the platform's own DST-aware
 * timezone conversion (`localtime_r`/equivalent), so this stays correct
 * across DST transitions too, not just a fixed current-offset shift.
 */
export async function getReflectionsForDate(dateStamp: string): Promise<ReflectionRow[]> {
  const db = await getDb();
  return db.select<ReflectionRow[]>(
    `SELECT id, created_at, slot_start_at, text FROM reflection
     WHERE date(created_at, 'localtime') = $1
     ORDER BY slot_start_at ASC`,
    [dateStamp],
  );
}

const SLOT_INTERVAL_MS = 30 * 60 * 1000;

/** Groups a flat, slot_start_at-ordered row list into runs of consecutive
 * slots sharing identical (current) text. Pure and re-run on every render
 * (see the Entries page's $derived), so editing a row's text re-clusters
 * immediately -- an edited row that no longer matches its neighbor's text
 * splits into its own cluster without any explicit "ungroup" step. */
export function clusterReflectionRows(rows: ReflectionRow[]): ReflectionCluster[] {
  const clusters: ReflectionCluster[] = [];
  for (const row of rows) {
    const current = clusters[clusters.length - 1];
    const prevRow = current?.rows[current.rows.length - 1];
    const contiguous =
      prevRow !== undefined &&
      new Date(row.slot_start_at).getTime() - new Date(prevRow.slot_start_at).getTime() === SLOT_INTERVAL_MS;
    if (current && contiguous && prevRow!.text === row.text) {
      current.rows.push(row);
    } else {
      clusters.push({ rows: [row] });
    }
  }
  return clusters;
}

/** Edits one covered slot's text in place, independent of any sibling slots
 * that were saved together in the same saveReflection call -- keyed by id,
 * not created_at, so editing one slot never touches the others (and, per
 * clusterReflectionRows above, immediately splits it out of its display
 * cluster if the new text no longer matches). */
export async function updateReflectionText(id: number, text: string): Promise<void> {
  const db = await getDb();
  await db.execute(`UPDATE reflection SET text = $1 WHERE id = $2`, [text, id]);
}

export async function getTaskList(dateStamp: string): Promise<string> {
  const db = await getDb();
  const rows = await db.select<{ content: string }[]>(
    `SELECT content FROM daily_task_list WHERE date = $1`,
    [dateStamp],
  );
  return rows[0]?.content ?? "";
}

export interface TaskListUpdate {
  date: string;
  content: string;
  sourceLabel: string;
}

export async function saveTaskList(dateStamp: string, content: string): Promise<void> {
  const db = await getDb();
  await db.execute(
    `INSERT INTO daily_task_list (date, content, updated_at) VALUES ($1, $2, $3)
     ON CONFLICT(date) DO UPDATE SET content = excluded.content, updated_at = excluded.updated_at`,
    [dateStamp, content, new Date().toISOString()],
  );
  // Broadcast so the Timer/overlay/catch-up windows (whichever are open)
  // pick up the edit live instead of showing stale content until their next
  // remount. Tagged with the sending window's label so a window doesn't
  // clobber its own in-progress typing when it receives its own broadcast.
  const sourceLabel = getCurrentWindow().label;
  await emit("tasklist://updated", { date: dateStamp, content, sourceLabel } satisfies TaskListUpdate);
}

/**
 * Subscribes to task-list edits made in other windows for today's date,
 * ignoring the window's own broadcasts (see `saveTaskList`). Call from
 * `onMount` in any window that displays the task list; call the returned
 * unlisten function from `onDestroy`.
 */
export async function listenForTaskListUpdates(
  onUpdate: (content: string) => void,
): Promise<UnlistenFn> {
  const selfLabel = getCurrentWindow().label;
  return listen<TaskListUpdate>("tasklist://updated", (event) => {
    const { date, content, sourceLabel } = event.payload;
    if (sourceLabel === selfLabel) return;
    if (date === localDateStamp()) onUpdate(content);
  });
}

// --- "Not to do" list (mirrors the Most Important Tasks list above) ---

export async function getNotToDoList(dateStamp: string): Promise<string> {
  const db = await getDb();
  const rows = await db.select<{ content: string }[]>(
    `SELECT content FROM not_to_do_list WHERE date = $1`,
    [dateStamp],
  );
  return rows[0]?.content ?? "";
}

export interface NotToDoUpdate {
  date: string;
  content: string;
  sourceLabel: string;
}

export async function saveNotToDoList(dateStamp: string, content: string): Promise<void> {
  const db = await getDb();
  await db.execute(
    `INSERT INTO not_to_do_list (date, content, updated_at) VALUES ($1, $2, $3)
     ON CONFLICT(date) DO UPDATE SET content = excluded.content, updated_at = excluded.updated_at`,
    [dateStamp, content, new Date().toISOString()],
  );
  const sourceLabel = getCurrentWindow().label;
  await emit("nottodolist://updated", { date: dateStamp, content, sourceLabel } satisfies NotToDoUpdate);
}

/**
 * Subscribes to not-to-do-list edits made in other windows for today's date,
 * ignoring the window's own broadcasts (see `saveNotToDoList`). Call from
 * `onMount` in any window that displays the list; call the returned unlisten
 * function from `onDestroy`.
 */
export async function listenForNotToDoListUpdates(
  onUpdate: (content: string) => void,
): Promise<UnlistenFn> {
  const selfLabel = getCurrentWindow().label;
  return listen<NotToDoUpdate>("nottodolist://updated", (event) => {
    const { date, content, sourceLabel } = event.payload;
    if (sourceLabel === selfLabel) return;
    if (date === localDateStamp()) onUpdate(content);
  });
}

/**
 * `Number(value)` with a fallback for anything that doesn't parse to a
 * finite number -- guards every numeric app_setting read below against a
 * corrupt stored value (a hand-edited DB, a future bug, or an imported
 * export whose app_setting.value didn't survive round-tripping as a clean
 * number). Without this, a single bad row turns into `NaN` flowing into a
 * `u32`-typed Tauri command (sync_breakit_config, set_overlay_auto_close_minutes),
 * which serde rejects -- and since none of the onMount chains that call
 * these are wrapped in try/catch, that rejection silently aborts the rest
 * of that chain (dead listeners, a frozen clock on the Timer page). Falling
 * back to a sane default here is what keeps a bad value from ever reaching
 * that invoke call in the first place.
 */
function numberOr(value: string | undefined, fallback: number): number {
  if (value === undefined) return fallback;
  const n = Number(value);
  return Number.isFinite(n) ? n : fallback;
}

export interface BreakitSettings {
  length: number;
  includeSpecial: boolean;
}

const DEFAULT_BREAKIT: BreakitSettings = { length: 15, includeSpecial: false };

export async function getBreakitSettings(): Promise<BreakitSettings> {
  const db = await getDb();
  const rows = await db.select<{ key: string; value: string }[]>(
    `SELECT key, value FROM app_setting WHERE key IN ('breakit_length', 'breakit_include_special')`,
  );
  const map = Object.fromEntries(rows.map((r) => [r.key, r.value]));
  return {
    length: numberOr(map.breakit_length, DEFAULT_BREAKIT.length),
    includeSpecial: (map.breakit_include_special ?? "false") === "true",
  };
}

export async function saveBreakitSettings(settings: BreakitSettings): Promise<void> {
  const db = await getDb();
  await db.execute(
    `INSERT INTO app_setting (key, value) VALUES ('breakit_length', $1)
     ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
    [String(settings.length)],
  );
  await db.execute(
    `INSERT INTO app_setting (key, value) VALUES ('breakit_include_special', $1)
     ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
    [String(settings.includeSpecial)],
  );
  await syncBreakitConfigToBackend(settings);
}

export async function syncBreakitConfigToBackend(settings: BreakitSettings): Promise<void> {
  await invoke("sync_breakit_config", {
    length: settings.length,
    includeSpecial: settings.includeSpecial,
  });
}

/** Call once on app boot (main window) so Rust's in-memory copy matches SQLite. */
export async function loadAndSyncBreakitSettings(): Promise<BreakitSettings> {
  const settings = await getBreakitSettings();
  await syncBreakitConfigToBackend(settings);
  return settings;
}

// --- Force-close shortcut (Ctrl+Alt+Shift+F12) toggle (Settings) ------

const FORCE_CLOSE_SHORTCUT_KEY = "force_close_shortcut_enabled";

/**
 * Defaults to enabled if the row is somehow missing (should not happen --
 * migration v3 inserts it for both fresh installs and upgrades -- but a
 * missing setting should never silently disable a kill switch).
 */
export async function getForceCloseShortcutEnabled(): Promise<boolean> {
  const db = await getDb();
  const rows = await db.select<{ value: string }[]>(
    `SELECT value FROM app_setting WHERE key = $1`,
    [FORCE_CLOSE_SHORTCUT_KEY],
  );
  return (rows[0]?.value ?? "true") === "true";
}

export async function saveForceCloseShortcutEnabled(enabled: boolean): Promise<void> {
  const db = await getDb();
  await db.execute(
    `INSERT INTO app_setting (key, value) VALUES ($1, $2)
     ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
    [FORCE_CLOSE_SHORTCUT_KEY, String(enabled)],
  );
  await syncForceCloseShortcutToBackend(enabled);
}

export async function syncForceCloseShortcutToBackend(enabled: boolean): Promise<void> {
  await invoke("set_force_close_shortcut_enabled", { enabled });
}

/** Call once on app boot (main window) so Rust's in-memory flag matches SQLite. */
export async function loadAndSyncForceCloseShortcutSetting(): Promise<boolean> {
  const enabled = await getForceCloseShortcutEnabled();
  await syncForceCloseShortcutToBackend(enabled);
  return enabled;
}

// --- Media pause-on-break toggle (Settings) ----------------------------

const MEDIA_PAUSE_ON_BREAK_KEY = "media_pause_on_break_enabled";

export async function getMediaPauseOnBreakEnabled(): Promise<boolean> {
  const db = await getDb();
  const rows = await db.select<{ value: string }[]>(
    `SELECT value FROM app_setting WHERE key = $1`,
    [MEDIA_PAUSE_ON_BREAK_KEY],
  );
  return (rows[0]?.value ?? "true") === "true";
}

export async function saveMediaPauseOnBreakEnabled(enabled: boolean): Promise<void> {
  const db = await getDb();
  await db.execute(
    `INSERT INTO app_setting (key, value) VALUES ($1, $2)
     ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
    [MEDIA_PAUSE_ON_BREAK_KEY, String(enabled)],
  );
  await syncMediaPauseOnBreakToBackend(enabled);
}

export async function syncMediaPauseOnBreakToBackend(enabled: boolean): Promise<void> {
  await invoke("set_media_pause_on_break_enabled", { enabled });
}

/** Call once on app boot (main window) so Rust's in-memory flag matches SQLite. */
export async function loadAndSyncMediaPauseOnBreakSetting(): Promise<boolean> {
  const enabled = await getMediaPauseOnBreakEnabled();
  await syncMediaPauseOnBreakToBackend(enabled);
  return enabled;
}

// --- Android break-notification persistence toggle (Settings) ----------
// Android only in effect (see overlay.rs's spawn_or_update_overlay /
// BreakScheduling.kt's postBreakNotification) -- whether the break
// notification can be swiped away or only clears once the break itself
// resolves. Read/settable cross-platform like the other toggles so the
// Settings page doesn't need its own platform branching just to persist a
// value.

const BREAK_NOTIFICATION_PERSISTENT_KEY = "break_notification_persistent_enabled";

export async function getBreakNotificationPersistentEnabled(): Promise<boolean> {
  const db = await getDb();
  const rows = await db.select<{ value: string }[]>(
    `SELECT value FROM app_setting WHERE key = $1`,
    [BREAK_NOTIFICATION_PERSISTENT_KEY],
  );
  return (rows[0]?.value ?? "true") === "true";
}

export async function saveBreakNotificationPersistentEnabled(enabled: boolean): Promise<void> {
  const db = await getDb();
  await db.execute(
    `INSERT INTO app_setting (key, value) VALUES ($1, $2)
     ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
    [BREAK_NOTIFICATION_PERSISTENT_KEY, String(enabled)],
  );
  await syncBreakNotificationPersistentToBackend(enabled);
}

export async function syncBreakNotificationPersistentToBackend(enabled: boolean): Promise<void> {
  await invoke("set_break_notification_persistent_enabled", { enabled });
}

/** Call once on app boot (main window) so Rust's in-memory flag matches SQLite. */
export async function loadAndSyncBreakNotificationPersistentSetting(): Promise<boolean> {
  const enabled = await getBreakNotificationPersistentEnabled();
  await syncBreakNotificationPersistentToBackend(enabled);
  return enabled;
}

// --- macOS media-toggle guard (see media.rs's macos_impl module) -------
//
// Windows' media pause queries actual playback state, so it never needs
// this; macOS's toggle is blind, so this narrows (does not eliminate) the
// risk of a second toggle within the same break cycle resuming media we
// already paused. Not a user-facing setting -- no Settings UI for this.

const LAST_TOGGLE_TIME_KEY = "last_toggle_time";

/** No row until the first macOS toggle ever fires -- absence means null,
 * matching every other app_setting getter's default-when-missing pattern.
 * (app_setting.value is TEXT NOT NULL, so there's no seeded-NULL row to find.) */
export async function getLastMediaToggleTime(): Promise<string | null> {
  const db = await getDb();
  const rows = await db.select<{ value: string }[]>(
    `SELECT value FROM app_setting WHERE key = $1`,
    [LAST_TOGGLE_TIME_KEY],
  );
  return rows[0]?.value ?? null;
}

export async function saveLastMediaToggleTime(at: string): Promise<void> {
  const db = await getDb();
  await db.execute(
    `INSERT INTO app_setting (key, value) VALUES ($1, $2)
     ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
    [LAST_TOGGLE_TIME_KEY, at],
  );
}

export async function getLastWellnessCheckTime(): Promise<string | null> {
  const db = await getDb();
  const rows = await db.select<{ created_at: string }[]>(
    `SELECT created_at FROM wellness_check ORDER BY created_at DESC LIMIT 1`,
  );
  return rows[0]?.created_at ?? null;
}

export async function syncLastWellnessCheckAtToBackend(at: string): Promise<void> {
  await invoke("sync_last_wellness_check_at", { at });
}

/** Call once on app boot (main window) so Rust's media-toggle guard matches SQLite. */
export async function loadAndSyncMediaToggleGuard(): Promise<void> {
  const [lastToggleAt, lastWellnessCheckAt] = await Promise.all([
    getLastMediaToggleTime(),
    getLastWellnessCheckTime(),
  ]);
  await invoke("sync_media_toggle_guard", { lastToggleAt, lastWellnessCheckAt });
}

/** Persists Rust's media-toggle timestamp (emitted right after it actually
 * fires the macOS toggle) so the guard survives a crash/relaunch mid-break.
 * Call once from the main window; unlisten in onDestroy like every other
 * listener in this app. */
export async function listenForMediaToggleRecorded(): Promise<UnlistenFn> {
  return listen<string>("media-toggle://recorded", (event) => {
    void saveLastMediaToggleTime(event.payload);
  });
}

// --- Overlay auto-close timeout (Settings) -----------------------------

const OVERLAY_AUTO_CLOSE_KEY = "overlay_auto_close_minutes";
const DEFAULT_OVERLAY_AUTO_CLOSE_MINUTES = 5;

/**
 * Minutes after a break ends before the overlay force-closes even without a
 * reflection. Enforced in Rust (see OVERLAY_AUTO_CLOSE_MINUTES/
 * schedule_auto_close), so this value also needs pushing into backend state
 * via syncOverlayAutoCloseToBackend -- unlike the checkin timeout below,
 * which is frontend-only.
 */
export async function getOverlayAutoCloseMinutes(): Promise<number> {
  const db = await getDb();
  const rows = await db.select<{ value: string }[]>(
    `SELECT value FROM app_setting WHERE key = $1`,
    [OVERLAY_AUTO_CLOSE_KEY],
  );
  return numberOr(rows[0]?.value, DEFAULT_OVERLAY_AUTO_CLOSE_MINUTES);
}

export async function saveOverlayAutoCloseMinutes(minutes: number): Promise<void> {
  const db = await getDb();
  await db.execute(
    `INSERT INTO app_setting (key, value) VALUES ($1, $2)
     ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
    [OVERLAY_AUTO_CLOSE_KEY, String(minutes)],
  );
  await syncOverlayAutoCloseToBackend(minutes);
}

export async function syncOverlayAutoCloseToBackend(minutes: number): Promise<void> {
  await invoke("set_overlay_auto_close_minutes", { minutes });
}

/** Call once on app boot (main window) so Rust's in-memory value matches SQLite. */
export async function loadAndSyncOverlayAutoClose(): Promise<number> {
  const minutes = await getOverlayAutoCloseMinutes();
  await syncOverlayAutoCloseToBackend(minutes);
  return minutes;
}

// --- Wellness check-in auto-close timeout (Settings) --------------------

const CHECKIN_AUTO_CLOSE_KEY = "checkin_auto_close_minutes";
const DEFAULT_CHECKIN_AUTO_CLOSE_MINUTES = 5;

/**
 * Minutes after the wellness check-in popup opens before it auto-closes if
 * left untouched. Enforced entirely by the checkin page itself (a plain
 * setTimeout), so unlike the overlay timeout there's no Rust state to sync.
 */
export async function getCheckinAutoCloseMinutes(): Promise<number> {
  const db = await getDb();
  const rows = await db.select<{ value: string }[]>(
    `SELECT value FROM app_setting WHERE key = $1`,
    [CHECKIN_AUTO_CLOSE_KEY],
  );
  return numberOr(rows[0]?.value, DEFAULT_CHECKIN_AUTO_CLOSE_MINUTES);
}

export async function saveCheckinAutoCloseMinutes(minutes: number): Promise<void> {
  const db = await getDb();
  await db.execute(
    `INSERT INTO app_setting (key, value) VALUES ($1, $2)
     ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
    [CHECKIN_AUTO_CLOSE_KEY, String(minutes)],
  );
}

// --- Wellness check-in "nudge text" exclusions (Settings) -------------

const WELLNESS_TEXT_EXCLUSIONS_KEY = "wellness_text_exclusions";

/**
 * Raw comma-separated list of wellness check-in item labels (e.g.
 * "Exercise, Washroom") for which the "Let's Try Next Time :)" nudge should
 * stay hidden when that item is switched off. Stored as the user typed it --
 * matching is done case-insensitively by the checkin page via
 * parseWellnessTextExclusions, not here, so this stays a thin get/save pair
 * like the rest of app_setting.
 */
export async function getWellnessTextExclusions(): Promise<string> {
  const db = await getDb();
  const rows = await db.select<{ value: string }[]>(
    `SELECT value FROM app_setting WHERE key = $1`,
    [WELLNESS_TEXT_EXCLUSIONS_KEY],
  );
  return rows[0]?.value ?? "";
}

export async function saveWellnessTextExclusions(csv: string): Promise<void> {
  const db = await getDb();
  await db.execute(
    `INSERT INTO app_setting (key, value) VALUES ($1, $2)
     ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
    [WELLNESS_TEXT_EXCLUSIONS_KEY, csv],
  );
}

/** Splits/trims/lowercases the raw CSV into a lookup-ready list of labels. */
export function parseWellnessTextExclusions(csv: string): string[] {
  return csv
    .split(",")
    .map((s) => s.trim().toLowerCase())
    .filter((s) => s.length > 0);
}

// --- Data export/import (Settings > Data) -----------------------------

export async function readTextFile(path: string): Promise<string> {
  return invoke<string>("read_text_file", { path });
}

export async function writeTextFile(path: string, contents: string): Promise<void> {
  await invoke("write_text_file", { path, contents });
}

export const EXPORT_FORMAT_VERSION = 1;

interface TaskListRow {
  date: string;
  content: string;
  updated_at: string;
}

interface NotToDoRow {
  date: string;
  content: string;
  updated_at: string;
}

interface SettingRow {
  key: string;
  value: string;
}

interface WellnessCheckRow {
  id: number;
  reflection_id: number;
  relaxed_eyes: number;
  exercise: number;
  drank_water: number;
  washroom: number;
  created_at: string;
}

export interface ExportPayload {
  app: "reflectodoro";
  export_format_version: number;
  exported_at: string;
  data: {
    reflection: ReflectionRow[];
    daily_task_list: TaskListRow[];
    not_to_do_list: NotToDoRow[];
    app_setting: SettingRow[];
    wellness_check: WellnessCheckRow[];
  };
}

export async function exportAllData(includeSettings: boolean = true): Promise<ExportPayload> {
  const db = await getDb();
  const [reflection, daily_task_list, not_to_do_list, app_setting, wellness_check] = await Promise.all([
    db.select<ReflectionRow[]>(`SELECT id, created_at, slot_start_at, text FROM reflection`),
    db.select<TaskListRow[]>(`SELECT date, content, updated_at FROM daily_task_list`),
    db.select<NotToDoRow[]>(`SELECT date, content, updated_at FROM not_to_do_list`),
    includeSettings
      ? db.select<SettingRow[]>(`SELECT key, value FROM app_setting`)
      : Promise.resolve([]),
    db.select<WellnessCheckRow[]>(
      `SELECT id, reflection_id, relaxed_eyes, exercise, drank_water, washroom, created_at FROM wellness_check`,
    ),
  ]);
  return {
    app: "reflectodoro",
    export_format_version: EXPORT_FORMAT_VERSION,
    exported_at: new Date().toISOString(),
    data: { reflection, daily_task_list, not_to_do_list, app_setting, wellness_check },
  };
}

function assertString(value: unknown, label: string): string {
  if (typeof value !== "string") throw new Error(`${label} is missing or not a string`);
  return value;
}

function assertNumber(value: unknown, label: string): number {
  if (typeof value !== "number") throw new Error(`${label} is missing or not a number`);
  return value;
}

/**
 * Throws if `values` (a table's worth of one PRIMARY KEY column) contains a
 * duplicate. Import applies rows one `INSERT` at a time -- see importData's
 * doc comment for why this plugin can't make that loop a single atomic
 * transaction -- so a duplicate key reaching that loop hits SQLite's
 * PRIMARY KEY constraint only after any preceding `DELETE`s (replace mode)
 * have already committed. Rejecting the whole file up front, before
 * importData touches the database at all, is what actually prevents that
 * data loss.
 */
function assertNoDuplicates(values: (string | number)[], table: string, column: string): void {
  const seen = new Set<string | number>();
  for (const value of values) {
    if (seen.has(value)) {
      throw new Error(`data.${table} has more than one row with ${column} = ${JSON.stringify(value)}`);
    }
    seen.add(value);
  }
}

/**
 * JSON.parse plus a full shape/version check, throwing a specific Error on
 * the first problem found. Deliberately never returns a partially-valid
 * object -- importData() is only ever called with output from here, so the
 * DB is guaranteed untouched by anything this function rejects.
 */
export function parseAndValidateExport(raw: string): ExportPayload {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    throw new Error("File is not valid JSON");
  }
  if (typeof parsed !== "object" || parsed === null) {
    throw new Error("File does not contain a JSON object");
  }
  const obj = parsed as Record<string, unknown>;
  if (obj.app !== "reflectodoro") {
    throw new Error("File is not a Reflectodoro export");
  }
  if (obj.export_format_version !== EXPORT_FORMAT_VERSION) {
    throw new Error(
      `Unsupported export file version: got ${String(obj.export_format_version)}, expected ${EXPORT_FORMAT_VERSION}`,
    );
  }
  if (typeof obj.exported_at !== "string") {
    throw new Error("exported_at is missing or not a string");
  }
  if (typeof obj.data !== "object" || obj.data === null) {
    throw new Error("data is missing or not an object");
  }
  const data = obj.data as Record<string, unknown>;

  const reflectionRaw = data.reflection;
  if (!Array.isArray(reflectionRaw)) throw new Error("data.reflection is missing or not an array");
  const reflection: ReflectionRow[] = reflectionRaw.map((row, i) => {
    if (typeof row !== "object" || row === null) throw new Error(`reflection[${i}] is not an object`);
    const r = row as Record<string, unknown>;
    if (typeof r.id !== "number") throw new Error(`reflection[${i}].id is missing or not a number`);
    return {
      id: r.id,
      created_at: assertString(r.created_at, `reflection[${i}].created_at`),
      slot_start_at: assertString(r.slot_start_at, `reflection[${i}].slot_start_at`),
      text: assertString(r.text, `reflection[${i}].text`),
    };
  });
  // A duplicate `id` here would silently overwrite an earlier row's entry
  // in importData's idMap (file id -> newly-inserted id), misrouting
  // whichever reflection lost the collision's wellness_check rows onto a
  // different reflection entirely. exportAllData can never produce this
  // (SQLite's own AUTOINCREMENT PK is unique by construction) -- this only
  // guards a hand-edited or corrupted file.
  assertNoDuplicates(reflection.map((r) => r.id), "reflection", "id");

  const taskListRaw = data.daily_task_list;
  if (!Array.isArray(taskListRaw)) throw new Error("data.daily_task_list is missing or not an array");
  const daily_task_list: TaskListRow[] = taskListRaw.map((row, i) => {
    if (typeof row !== "object" || row === null) throw new Error(`daily_task_list[${i}] is not an object`);
    const r = row as Record<string, unknown>;
    return {
      date: assertString(r.date, `daily_task_list[${i}].date`),
      content: assertString(r.content, `daily_task_list[${i}].content`),
      updated_at: assertString(r.updated_at, `daily_task_list[${i}].updated_at`),
    };
  });
  // Duplicate `date` rows would hit daily_task_list's PRIMARY KEY partway
  // through importData's replace-mode insert loop -- by then the DELETEs
  // have already committed (see importData's own comment on why this
  // plugin can't wrap the whole operation in one real transaction), so a
  // late constraint violation here means the user's existing task lists are
  // already gone. Catching it before any DELETE runs at all is the only
  // way this validator can actually prevent that loss.
  assertNoDuplicates(daily_task_list.map((r) => r.date), "daily_task_list", "date");

  const notToDoListRaw = data.not_to_do_list;
  if (!Array.isArray(notToDoListRaw)) throw new Error("data.not_to_do_list is missing or not an array");
  const not_to_do_list: NotToDoRow[] = notToDoListRaw.map((row, i) => {
    if (typeof row !== "object" || row === null) throw new Error(`not_to_do_list[${i}] is not an object`);
    const r = row as Record<string, unknown>;
    return {
      date: assertString(r.date, `not_to_do_list[${i}].date`),
      content: assertString(r.content, `not_to_do_list[${i}].content`),
      updated_at: assertString(r.updated_at, `not_to_do_list[${i}].updated_at`),
    };
  });
  assertNoDuplicates(not_to_do_list.map((r) => r.date), "not_to_do_list", "date");

  const settingRaw = data.app_setting;
  if (!Array.isArray(settingRaw)) throw new Error("data.app_setting is missing or not an array");
  const app_setting: SettingRow[] = settingRaw.map((row, i) => {
    if (typeof row !== "object" || row === null) throw new Error(`app_setting[${i}] is not an object`);
    const r = row as Record<string, unknown>;
    return {
      key: assertString(r.key, `app_setting[${i}].key`),
      value: assertString(r.value, `app_setting[${i}].value`),
    };
  });
  assertNoDuplicates(app_setting.map((r) => r.key), "app_setting", "key");
  // Defense-in-depth alongside numberOr's read-side fallback (db.ts's numeric
  // getters): reject a non-numeric value for a key this app treats as
  // numeric right here, rather than letting it into the DB where it would
  // only surface later as a silently-defaulted read or (before numberOr) a
  // NaN reaching a u32-typed Tauri command.
  const NUMERIC_SETTING_KEYS = new Set([
    "breakit_length",
    "overlay_auto_close_minutes",
    "checkin_auto_close_minutes",
  ]);
  for (const row of app_setting) {
    if (NUMERIC_SETTING_KEYS.has(row.key) && !Number.isFinite(Number(row.value))) {
      throw new Error(`data.app_setting has a non-numeric value for "${row.key}": ${JSON.stringify(row.value)}`);
    }
  }

  const reflectionIds = new Set(reflection.map((r) => r.id));
  const wellnessRaw = data.wellness_check;
  if (!Array.isArray(wellnessRaw)) throw new Error("data.wellness_check is missing or not an array");
  const wellness_check: WellnessCheckRow[] = wellnessRaw.map((row, i) => {
    if (typeof row !== "object" || row === null) throw new Error(`wellness_check[${i}] is not an object`);
    const r = row as Record<string, unknown>;
    const reflection_id = assertNumber(r.reflection_id, `wellness_check[${i}].reflection_id`);
    if (!reflectionIds.has(reflection_id)) {
      throw new Error(`wellness_check[${i}].reflection_id ${reflection_id} has no matching reflection in this file`);
    }
    return {
      id: assertNumber(r.id, `wellness_check[${i}].id`),
      reflection_id,
      relaxed_eyes: assertNumber(r.relaxed_eyes, `wellness_check[${i}].relaxed_eyes`),
      exercise: assertNumber(r.exercise, `wellness_check[${i}].exercise`),
      drank_water: assertNumber(r.drank_water, `wellness_check[${i}].drank_water`),
      washroom: assertNumber(r.washroom, `wellness_check[${i}].washroom`),
      created_at: assertString(r.created_at, `wellness_check[${i}].created_at`),
    };
  });

  return {
    app: "reflectodoro",
    export_format_version: obj.export_format_version,
    exported_at: obj.exported_at,
    data: { reflection, daily_task_list, not_to_do_list, app_setting, wellness_check },
  };
}

export type ImportMode = "replace" | "merge";

export interface ImportResult {
  reflectionCount: number;
  taskListCount: number;
  notToDoListCount: number;
  settingCount: number;
  wellnessCheckCount: number;
}

/**
 * Applies a validated export payload to the DB. "replace" wipes all five
 * tables first; "merge" upserts daily_task_list/not_to_do_list/app_setting (imported wins
 * on key conflict, untouched rows keep their existing value) and always
 * appends reflection/wellness_check rows fresh -- reflection has no natural
 * dedupe key, and any timestamp-based heuristic risks silently discarding a
 * real reflection, which a backup/restore feature must never do (see plan
 * doc). Because every imported reflection gets a brand-new autoincrement id
 * (the file's own `id` is never reused), wellness_check.reflection_id is
 * remapped through `idMap` (old file id -> newly-inserted id) rather than
 * copied verbatim -- otherwise it would silently point at the wrong
 * reflection or a row that no longer exists.
 *
 * Not wrapped in a SQL transaction: confirmed against tauri-plugin-sql
 * 2.4.0's source (its `execute` Tauri command calls `pool.execute(query)`
 * directly, once per invocation, with no session/connection pinned across
 * calls) that a `BEGIN`/`COMMIT` sent as separate `db.execute()` calls
 * is not guaranteed to land on the same underlying SQLite connection --
 * the pool defaults to up to 10 connections, so it can't be relied on to
 * serialize onto one. A transaction wrapper here would be a false promise
 * of atomicity, not a real one.
 *
 * Safety instead comes from parseAndValidateExport() fully validating the
 * payload before this function is ever called -- including that every
 * wellness_check.reflection_id resolves within the same file, and that
 * daily_task_list/not_to_do_list/app_setting each have no duplicate
 * PRIMARY KEY. That duplicate-key check specifically is what stands between
 * "replace" mode and its worst failure mode: without it, a malformed file
 * could pass validation, let the DELETEs below commit, and only then hit a
 * PRIMARY KEY collision partway through the INSERT loop -- by which point
 * the user's previous data is already gone and the thrown error (caught and
 * shown by Settings' runImport) can't bring it back. Validating hard enough
 * that a well-formed file can never reach that constraint violation is the
 * only atomicity substitute this plugin's API leaves available from here.
 * A different class of failure -- a genuine I/O error, or the DB locked by
 * a concurrent write from another window -- can still interrupt this loop
 * mid-way and isn't something front-end validation can rule out; that
 * residual risk is real and unresolved, not something this function papers
 * over.
 */
export async function importData(
  payload: ExportPayload,
  mode: ImportMode,
  includeSettings: boolean = true,
): Promise<ImportResult> {
  const db = await getDb();
  const { reflection, daily_task_list, not_to_do_list, app_setting, wellness_check } = payload.data;

  if (mode === "replace") {
    // Child table first: wellness_check references reflection(id).
    await db.execute(`DELETE FROM wellness_check`);
    await db.execute(`DELETE FROM reflection`);
    await db.execute(`DELETE FROM daily_task_list`);
    await db.execute(`DELETE FROM not_to_do_list`);
    if (includeSettings) await db.execute(`DELETE FROM app_setting`);
  }

  const idMap = new Map<number, number>();
  for (const row of reflection) {
    const result = await db.execute(
      `INSERT INTO reflection (created_at, slot_start_at, text) VALUES ($1, $2, $3)`,
      [row.created_at, row.slot_start_at, row.text],
    );
    if (result.lastInsertId !== undefined) idMap.set(row.id, result.lastInsertId);
  }

  for (const row of wellness_check) {
    const newReflectionId = idMap.get(row.reflection_id);
    if (newReflectionId === undefined) continue; // guarded by parseAndValidateExport; defensive only
    await db.execute(
      `INSERT INTO wellness_check (reflection_id, relaxed_eyes, exercise, drank_water, washroom, created_at)
       VALUES ($1, $2, $3, $4, $5, $6)`,
      [newReflectionId, row.relaxed_eyes, row.exercise, row.drank_water, row.washroom, row.created_at],
    );
  }

  for (const row of daily_task_list) {
    await db.execute(
      mode === "merge"
        ? `INSERT INTO daily_task_list (date, content, updated_at) VALUES ($1, $2, $3)
           ON CONFLICT(date) DO UPDATE SET content = excluded.content, updated_at = excluded.updated_at`
        : `INSERT INTO daily_task_list (date, content, updated_at) VALUES ($1, $2, $3)`,
      [row.date, row.content, row.updated_at],
    );
  }

  for (const row of not_to_do_list) {
    await db.execute(
      mode === "merge"
        ? `INSERT INTO not_to_do_list (date, content, updated_at) VALUES ($1, $2, $3)
           ON CONFLICT(date) DO UPDATE SET content = excluded.content, updated_at = excluded.updated_at`
        : `INSERT INTO not_to_do_list (date, content, updated_at) VALUES ($1, $2, $3)`,
      [row.date, row.content, row.updated_at],
    );
  }

  if (includeSettings) {
    for (const row of app_setting) {
      await db.execute(
        mode === "merge"
          ? `INSERT INTO app_setting (key, value) VALUES ($1, $2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value`
          : `INSERT INTO app_setting (key, value) VALUES ($1, $2)`,
        [row.key, row.value],
      );
    }

    // Rust's in-memory breakit config, force-close-shortcut flag, and overlay
    // auto-close minutes are all caches of app_setting -- resync so an
    // imported value takes effect immediately, not just after the next app
    // restart.
    await loadAndSyncBreakitSettings();
    await loadAndSyncForceCloseShortcutSetting();
    await loadAndSyncOverlayAutoClose();
    await loadAndSyncMediaPauseOnBreakSetting();
  }

  // Unconditional (unlike the block above): wellness_check rows -- half of
  // the macOS media-toggle guard's state -- import regardless of
  // includeSettings, so this needs to resync even when settings are excluded.
  await loadAndSyncMediaToggleGuard();

  return {
    reflectionCount: reflection.length,
    taskListCount: daily_task_list.length,
    notToDoListCount: not_to_do_list.length,
    settingCount: includeSettings ? app_setting.length : 0,
    wellnessCheckCount: wellness_check.length,
  };
}
