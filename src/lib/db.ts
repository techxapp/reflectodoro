import Database from "@tauri-apps/plugin-sql";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

let dbPromise: ReturnType<typeof Database.load> | null = null;

function getDb() {
  if (!dbPromise) {
    dbPromise = Database.load("sqlite:pomodoro.db");
  }
  return dbPromise;
}

export interface ReflectionEntry {
  created_at: string;
  text: string;
  slot_count: number;
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
     WHERE date(created_at) = $1`,
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

export async function getReflectionsForDate(dateStamp: string): Promise<ReflectionEntry[]> {
  const db = await getDb();
  return db.select<ReflectionEntry[]>(
    `SELECT created_at, text, COUNT(*) as slot_count FROM reflection
     WHERE date(created_at) = $1
     GROUP BY created_at, text
     ORDER BY created_at ASC`,
    [dateStamp],
  );
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
    length: Number(map.breakit_length ?? DEFAULT_BREAKIT.length),
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
  return Number(rows[0]?.value ?? DEFAULT_OVERLAY_AUTO_CLOSE_MINUTES);
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
  return Number(rows[0]?.value ?? DEFAULT_CHECKIN_AUTO_CLOSE_MINUTES);
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

interface ReflectionRow {
  id: number;
  created_at: string;
  slot_start_at: string;
  text: string;
}

interface TaskListRow {
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
    app_setting: SettingRow[];
    wellness_check: WellnessCheckRow[];
  };
}

export async function exportAllData(includeSettings: boolean = true): Promise<ExportPayload> {
  const db = await getDb();
  const [reflection, daily_task_list, app_setting, wellness_check] = await Promise.all([
    db.select<ReflectionRow[]>(`SELECT id, created_at, slot_start_at, text FROM reflection`),
    db.select<TaskListRow[]>(`SELECT date, content, updated_at FROM daily_task_list`),
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
    data: { reflection, daily_task_list, app_setting, wellness_check },
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
    data: { reflection, daily_task_list, app_setting, wellness_check },
  };
}

export type ImportMode = "replace" | "merge";

export interface ImportResult {
  reflectionCount: number;
  taskListCount: number;
  settingCount: number;
  wellnessCheckCount: number;
}

/**
 * Applies a validated export payload to the DB. "replace" wipes all four
 * tables first; "merge" upserts daily_task_list/app_setting (imported wins
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
 * Not wrapped in a SQL transaction: tauri-plugin-sql's `execute()` pulls an
 * arbitrary connection from its pool per call, so a BEGIN/COMMIT spanning
 * multiple execute() calls wouldn't provide real atomicity here. Safety
 * instead comes from parseAndValidateExport() fully validating the payload
 * (including that every wellness_check.reflection_id resolves within the
 * same file) before this function is ever called.
 */
export async function importData(
  payload: ExportPayload,
  mode: ImportMode,
  includeSettings: boolean = true,
): Promise<ImportResult> {
  const db = await getDb();
  const { reflection, daily_task_list, app_setting, wellness_check } = payload.data;

  if (mode === "replace") {
    // Child table first: wellness_check references reflection(id).
    await db.execute(`DELETE FROM wellness_check`);
    await db.execute(`DELETE FROM reflection`);
    await db.execute(`DELETE FROM daily_task_list`);
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
    settingCount: includeSettings ? app_setting.length : 0,
    wellnessCheckCount: wellness_check.length,
  };
}
