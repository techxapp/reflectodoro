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
