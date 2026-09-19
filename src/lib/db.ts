import Database from "@tauri-apps/plugin-sql";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { error as logError } from "@tauri-apps/plugin-log";

let dbPromise: ReturnType<typeof Database.load> | null = null;

function getDb() {
  if (!dbPromise) {
    const attempt = Database.load("sqlite:pomodoro.db");
    dbPromise = attempt;
    // If this load rejects (the database file being unreadable or locked --
    // no migration runs here anymore; Rust creates and verifies the schema
    // before any webview boots, see db.rs's ensure_schema), clear the cache
    // instead of leaving a rejected promise memoized here
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
    attempt.catch((e) => {
      // Logged, not swallowed: a failed load otherwise makes every DB-backed
      // feature silently do nothing, with the only visible symptom being
      // missing data much later.
      // plugin-log's Webview target puts this in the same file as the Rust
      // side's, which is what makes it diagnosable from a user's log export.
      void logError(`db: failed to open pomodoro.db: ${e instanceof Error ? e.message : String(e)}`);
      if (dbPromise === attempt) dbPromise = null;
    });
  }
  return dbPromise;
}

// --- Encryption at rest -------------------------------------------------
//
// `reflection.text`, `daily_task_list.content` and `not_to_do_list.content`
// are stored encrypted (see src-tauri/src/crypto.rs for the format, key
// storage and threat model). Every write through this module encrypts on the
// way in and every read decrypts on the way out, so the rest of the app --
// and every Svelte component -- keeps working in plaintext and never sees a
// ciphertext blob.
//
// All key material and cipher work stays in Rust (and, on Android, inside the
// OS Keystore): the webview only ever hands plaintext across and gets
// ciphertext back, never a key.
//
// Batched deliberately. The read side is list-shaped (a day's Entries view
// decrypts up to 48 rows, an export decrypts the whole history) and each
// `invoke` is its own IPC round trip, so doing these one value at a time
// would turn one call into dozens.

async function encryptFields(values: string[]): Promise<string[]> {
  if (values.length === 0) return [];
  return invoke<string[]>("encrypt_fields", { values });
}

async function decryptFields(values: string[]): Promise<string[]> {
  if (values.length === 0) return [];
  return invoke<string[]>("decrypt_fields", { values });
}

async function encryptField(value: string): Promise<string> {
  return (await encryptFields([value]))[0];
}

/** A value with no `enc1:` marker comes back unchanged rather than throwing
 * -- see crypto.rs's marker passthrough. Nothing writes one anymore; it's a
 * safety net for a stray row, not an expected case. */
async function decryptField(value: string): Promise<string> {
  return (await decryptFields([value]))[0];
}

/** `screen_time_session.app_id_hash` -- a deterministic HMAC-SHA256 "blind
 * index" of `app_id` (see crypto.rs's `FieldCipher::blind_index_many`), used
 * only for `GROUP BY`/duplicate-check equality once `app_id` itself is
 * `encryptField`-ed ciphertext and therefore never equal to itself across
 * two writes. Unlike `encryptFields`, calling this twice on the same value
 * is expected to return the same hash both times -- that's the whole point. */
async function hashAppIds(values: string[]): Promise<string[]> {
  if (values.length === 0) return [];
  return invoke<string[]>("hash_app_ids", { values });
}

/** The full stored/exported shape. `created_at` is ciphertext in the
 * database and plaintext in an export file -- `exportAllData` decrypts it on
 * the way out. */
export interface ReflectionRow {
  id: number;
  created_at: string;
  slot_start_at: string;
  text: string;
}

/** What the Entries view actually reads. `created_at` is deliberately absent
 * rather than carried along as an unused ciphertext blob: nothing in the UI
 * displays it, and having it in the type invites someone to render it. */
export type ReflectionDisplayRow = Omit<ReflectionRow, "created_at">;

/** A run of rows for display purposes: consecutive break slots (30 minutes
 * apart, no gap) carrying identical text. Computed client-side from the flat
 * row list rather than stored -- deliberately NOT keyed off `created_at`
 * (which rows share when saved together by saveReflection's slot-merge
 * logic), because editing one row's text should immediately split it off
 * from unedited neighbors, not stay bundled under the original save event. */
export interface ReflectionCluster {
  rows: ReflectionDisplayRow[];
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

/**
 * Rust hands the overlay `current_slot_start` as the *break* slot's start
 * (`:25`/`:55` -- see grid.rs::slot_for), but `reflection.slot_start_at`
 * should record the *work* slot it follows (`:00`/`:30`), so entries display
 * as starting when the pomodoro actually began. Both break-slot gaps are
 * exactly 25 minutes, so this is a fixed offset, not a grid recomputation --
 * mirrors `grid::preceding_work_slot_start_iso` on the Rust side.
 */
export function precedingWorkSlotStartIso(breakSlotStartIso: string): string {
  return new Date(new Date(breakSlotStartIso).getTime() - 25 * 60 * 1000).toISOString();
}

/**
 * Mirror of `precedingWorkSlotStartIso` in the other direction: the work slot
 * that begins the moment the current break ends (every break is a fixed 5
 * minutes -- `:25`-`:30`, `:55`-`:00`). Used for the overlay's "Coming next"
 * preview of whatever's already saved for the upcoming slot (e.g. entered
 * ahead of time via the Entries page's bulk edit) -- see
 * grid::next_work_slot_start_iso for the Rust-side equivalent used by the
 * Android native overlay.
 */
export function nextWorkSlotStartIso(breakSlotStartIso: string): string {
  return new Date(new Date(breakSlotStartIso).getTime() + 5 * 60 * 1000).toISOString();
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
 * Text already saved for a given slot (by its canonical ISO start), or `null`
 * if no row exists -- used by the overlay's "Coming next" preview to look up
 * the upcoming work slot rather than just checking whether it's covered
 * (`isSlotCovered`). Blank/"skip"-only text is treated the same as no row at
 * all by the caller (see overlay/+page.svelte's `isSkipOnlyText`), not here,
 * so this stays a plain lookup with no display-filtering opinion of its own.
 */
export async function getReflectionTextForSlot(slotStartIso: string): Promise<string | null> {
  const db = await getDb();
  const rows = await db.select<{ text: string }[]>(
    `SELECT text FROM reflection WHERE slot_start_at = $1 LIMIT 1`,
    [slotStartIso],
  );
  if (rows[0]?.text === undefined) return null;
  return decryptField(rows[0].text);
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

/**
 * When there's more than one covered (missed) slot and `text` splits into
 * exactly as many non-blank lines as there are slots, returns those lines in
 * slot order (index 0 = oldest slot, matching `findMissedSlots`'s own
 * oldest-first ordering) so each pomodoro gets its own line instead of every
 * slot sharing the same full text. Returns `null` on any mismatch -- caller
 * then falls back to writing the same complete text into every slot, same as
 * before this existed. Blank lines don't count as rows (they're filtered
 * before the length check), so a deliberately-skipped pomodoro in a split
 * needs the existing `"skip"` convention (see `isSkipOnlyText`) as its line's
 * content rather than an empty line.
 */
export function splitReflectionForSlots(text: string, coveredSlots: string[]): string[] | null {
  if (coveredSlots.length <= 1) return null;
  const lines = text
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => l.length > 0);
  if (lines.length !== coveredSlots.length) return null;
  return lines;
}

/** `storedCreatedAt` is ciphertext, not a plain timestamp: reflection's
 * created_at/updated_at are encrypted at rest (crypto.rs) -- a plaintext edit
 * time would leak when the user is awake and working even to someone who
 * can't read a single reflection. Nothing queries either column any more;
 * P2P sync finds changed rows via the `rev` counter instead (db.rs), which
 * the table's triggers maintain, so nothing here writes it. */
async function upsertReflectionRow(
  db: Database,
  slot: string,
  storedText: string,
  storedCreatedAt: string,
): Promise<void> {
  const existing = await db.select<{ id: number }[]>(
    `SELECT id FROM reflection WHERE slot_start_at = $1`,
    [slot],
  );
  if (existing.length > 0) {
    await db.execute(`UPDATE reflection SET text = $1, updated_at = $2 WHERE slot_start_at = $3`, [
      storedText,
      storedCreatedAt,
      slot,
    ]);
  } else {
    await db.execute(
      `INSERT INTO reflection (created_at, slot_start_at, text, updated_at) VALUES ($1, $2, $3, $4)`,
      [storedCreatedAt, slot, storedText, storedCreatedAt],
    );
  }
}

/** One DB row per covered slot -- so "missed" pomodoros are individually recorded, not bundled
 * into one array field. Upserts per slot (same pattern as bulkUpsertReflections) rather than
 * blindly inserting: retrying a failed submit -- the overlay's documented "Close break screen
 * anyway" escape-hatch path exists for exactly this -- would otherwise re-insert the slots that
 * already succeeded before the failure, since slot_start_at has no UNIQUE constraint. An update
 * leaves the original created_at alone.
 *
 * If `text` cleanly splits into one line per covered slot (see `splitReflectionForSlots`), each
 * slot gets its own line instead of the full text -- otherwise (the common case: one slot, or a
 * line count that doesn't match) every slot gets the identical complete text, same as always. */
export async function saveReflection(coveredSlots: string[], text: string): Promise<void> {
  const db = await getDb();
  const createdAt = new Date().toISOString();
  const perSlotLines = splitReflectionForSlots(text, coveredSlots);
  if (perSlotLines) {
    // Each slot gets genuinely different plaintext now, so each needs its own
    // fresh nonce -- encryptFields (batched) gives every value its own
    // encrypt call under the hood, unlike the single shared-ciphertext path
    // below where one nonce/plaintext pair is legitimately reused verbatim.
    // createdAt rides along in the same batch rather than costing a second
    // IPC round trip -- this is the break overlay's submit path.
    const encrypted = await encryptFields([...perSlotLines, createdAt]);
    const storedCreatedAt = encrypted[encrypted.length - 1];
    for (let i = 0; i < coveredSlots.length; i++) {
      await upsertReflectionRow(db, coveredSlots[i], encrypted[i], storedCreatedAt);
    }
    return;
  }
  // Encrypted once and the same ciphertext written to every covered slot,
  // rather than encrypting per slot. This is not nonce reuse: one nonce
  // encrypted one plaintext, and copying that result into several rows never
  // pairs that nonce with *different* plaintext (the property that actually
  // matters -- see crypto.rs). All it reveals is that these rows share text,
  // which the merge already makes explicit anyway.
  const [stored, storedCreatedAt] = await encryptFields([text, createdAt]);
  for (const slot of coveredSlots) {
    await upsertReflectionRow(db, slot, stored, storedCreatedAt);
  }
}

/** Text of the most recently saved reflection row for a slot strictly before
 * `beforeSlotStartIso`, used to pre-populate (never auto-save) the overlay's
 * reflection textarea so the user can see/tweak what they wrote last time
 * instead of starting blank. Ordered by `slot_start_at` (the slot the
 * reflection is *about*), not `id`/`created_at` (when it was *written*) --
 * the Entries page's bulk edit and "View all" mock-slot editor can both
 * insert/update a row for a slot far in the future relative to when it's
 * saved, and an id-ordered query would then surface that future entry as the
 * "last" one even while sitting inside an earlier break. Returns null when no
 * such reflection exists (fresh install, or the very first slot of all). */
export async function getLastReflectionText(
  beforeSlotStartIso: string,
): Promise<string | null> {
  const db = await getDb();
  const rows = await db.select<{ text: string }[]>(
    `SELECT text FROM reflection WHERE slot_start_at < $1 ORDER BY slot_start_at DESC LIMIT 1`,
    [beforeSlotStartIso],
  );
  if (rows[0]?.text === undefined) return null;
  return decryptField(rows[0].text);
}

export interface WellnessCheckValues {
  relaxedEyes: boolean;
  exercise: boolean;
  drankWater: boolean;
  washroom: boolean;
}

/** Saves a wellness check-in keyed directly on the work slot it's about
 * (`slotStartIso`, the same value as `reflection.slot_start_at`) rather than
 * a `reflection.id` FK -- see CLAUDE.md's "Data model" for why
 * `wellness_check.reflection_id` was replaced with `slot_start_at`:
 * it lets Settings -> Data merge-mode import dedupe/collapse
 * on the slot directly instead of remapping a foreign key through every
 * reflection-row collapse.
 *
 * Still throws rather than silently inserting an orphaned row if no
 * reflection exists for this slot (which shouldn't happen; the check-in
 * popup only ever opens after a reflection was saved) -- same fail-loudly
 * behavior the old `getReflectionIdForSlot` gave for free via its own
 * lookup, preserved here as an explicit existence check since there's no
 * FK to enforce it anymore. */
export async function saveWellnessCheck(
  slotStartIso: string,
  values: WellnessCheckValues,
): Promise<void> {
  const db = await getDb();
  const existing = await db.select<{ found: number }[]>(
    `SELECT 1 as found FROM reflection WHERE slot_start_at = $1 LIMIT 1`,
    [slotStartIso],
  );
  if (!existing[0]) throw new Error(`no reflection row found for slot ${slotStartIso}`);

  const createdAt = new Date().toISOString();
  const [relaxedEyes, exercise, drankWater, washroom] = await encryptFields([
    values.relaxedEyes ? "1" : "0",
    values.exercise ? "1" : "0",
    values.drankWater ? "1" : "0",
    values.washroom ? "1" : "0",
  ]);
  await db.execute(
    `INSERT INTO wellness_check (slot_start_at, relaxed_eyes, exercise, drank_water, washroom, created_at)
     VALUES ($1, $2, $3, $4, $5, $6)`,
    [slotStartIso, relaxedEyes, exercise, drankWater, washroom, createdAt],
  );
}

export interface WellnessSummary {
  total: number;
  relaxedEyes: number;
  exercise: number;
  drankWater: number;
  washroom: number;
}

/** Totals for each wellness check-in item on a given day, keyed off
 * wellness_check's own `slot_start_at` (not `created_at`) -- same grouping
 * basis getReflectionsForDate uses, so a late-night check-in submitted
 * after midnight stays filed under the pomodoro it was actually about
 * instead of leaking onto the next day and disagreeing with the reflections
 * shown beside it on the Entries page. No longer needs a JOIN to reflection
 * for this -- see CLAUDE.md's "Data model" on `wellness_check.slot_start_at`.
 *
 * The four boolean columns are ciphertext at rest (see "Encryption at
 * rest" above), so this can no longer let SQLite do the summing with
 * `SUM`/`COUNT` -- it fetches every matching row's four values, decrypts
 * them in one batched call, and sums client-side instead. A day is at most
 * 48 rows, so this stays cheap. */
export async function getWellnessSummaryForDate(dateStamp: string): Promise<WellnessSummary> {
  const db = await getDb();
  const rows = await db.select<
    { relaxed_eyes: string; exercise: string; drank_water: string; washroom: string }[]
  >(
    `SELECT relaxed_eyes, exercise, drank_water, washroom
     FROM wellness_check
     WHERE date(slot_start_at, 'localtime') = $1`,
    [dateStamp],
  );
  const flat = rows.flatMap((r) => [r.relaxed_eyes, r.exercise, r.drank_water, r.washroom]);
  const decrypted = await decryptFields(flat);

  const summary: WellnessSummary = { total: rows.length, relaxedEyes: 0, exercise: 0, drankWater: 0, washroom: 0 };
  for (let i = 0; i < rows.length; i++) {
    summary.relaxedEyes += Number(decrypted[i * 4]);
    summary.exercise += Number(decrypted[i * 4 + 1]);
    summary.drankWater += Number(decrypted[i * 4 + 2]);
    summary.washroom += Number(decrypted[i * 4 + 3]);
  }
  return summary;
}

/**
 * Filters on `slot_start_at`, not `created_at`: a reflection's day identity
 * is which pomodoro it's *about* (the work slot it covers), not the moment
 * it happened to be typed. A late-night pomodoro (e.g. the 23:30 slot) whose
 * reflection isn't submitted until after midnight has a `created_at` that
 * rolls onto the next calendar day while `slot_start_at` stays on the day
 * the work actually happened -- filtering on `created_at` used to leak that
 * row onto the following day's Entries view instead of the day it belongs
 * to. `date(slot_start_at, 'localtime')`, not bare `date(slot_start_at)`:
 * both timestamps are stored as UTC ISO strings (`new Date().toISOString()`,
 * see saveReflection/precedingWorkSlotStartIso), so a bare `date()` would
 * return the *UTC* calendar date while `dateStamp` here always comes from
 * `localDateStamp()`, the *local* calendar date -- SQLite's `'localtime'`
 * modifier delegates to the platform's own DST-aware timezone conversion
 * (`localtime_r`/equivalent), so this stays correct across DST transitions
 * too, not just a fixed current-offset shift.
 */
export async function getReflectionsForDate(dateStamp: string): Promise<ReflectionDisplayRow[]> {
  const db = await getDb();
  const rows = await db.select<ReflectionDisplayRow[]>(
    `SELECT id, slot_start_at, text FROM reflection
     WHERE date(slot_start_at, 'localtime') = $1
     ORDER BY slot_start_at ASC`,
    [dateStamp],
  );
  // One batched decrypt for the whole day (up to 48 rows) rather than one
  // IPC round trip per row. `clusterReflectionRows` compares these texts to
  // group consecutive slots, so it has to run on the decrypted values --
  // ciphertext of identical text differs every time by design.
  const texts = await decryptFields(rows.map((r) => r.text));
  return rows.map((row, i) => ({ ...row, text: texts[i] }));
}

const SLOT_INTERVAL_MS = 30 * 60 * 1000;

/** Groups a flat, slot_start_at-ordered row list into runs of consecutive
 * slots sharing identical (current) text. Pure and re-run on every render
 * (see the Entries page's $derived), so editing a row's text re-clusters
 * immediately -- an edited row that no longer matches its neighbor's text
 * splits into its own cluster without any explicit "ungroup" step. */
export function clusterReflectionRows(rows: ReflectionDisplayRow[]): ReflectionCluster[] {
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
 * cluster if the new text no longer matches). Also bumps updated_at, which is
 * now purely a record of when the row was last edited -- P2P sync tracks
 * changes via the `rev` counter (db.rs) rather than this column,
 * and the table's triggers advance rev on this UPDATE without it being named
 * here. Both the text and the timestamp are encrypted, in one batched call. */
export async function updateReflectionText(id: number, text: string): Promise<void> {
  const db = await getDb();
  const [storedText, storedUpdatedAt] = await encryptFields([text, new Date().toISOString()]);
  await db.execute(`UPDATE reflection SET text = $1, updated_at = $2 WHERE id = $3`, [
    storedText,
    storedUpdatedAt,
    id,
  ]);
}

// --- Bulk edit by time range (Entries page) ----------------------------
//
// Lets the user set the same text across every 30-minute work slot that
// falls *completely* inside a given hh:mm-hh:mm range on the day they're
// currently viewing, in one action -- an upsert per slot (update if a row
// already exists for it, insert if not) rather than one-row-at-a-time via
// the inline pencil edit.

const HHMM_RE = /^([01]\d|2[0-3]):[0-5]\d$/;

export function validateBulkEditRange(startTime: string, endTime: string): string | null {
  if (!HHMM_RE.test(startTime)) return "Start time must be in HH:MM format";
  if (!HHMM_RE.test(endTime)) return "End time must be in HH:MM format";
  if (startTime >= endTime) return "End time must be after start time";
  return null;
}

function hhmmToMinuteOfDay(hhmm: string): number {
  const [h, m] = hhmm.split(":").map(Number);
  return h * 60 + m;
}

/**
 * Only slots *completely* contained in [startTime, endTime) count -- a slot
 * that merely overlaps the range's edge is excluded. E.g. 11:45-13:15 keeps
 * 12:00 and 12:30 but not 11:30 (starts before 11:45) or 13:00 (would end at
 * 13:30, past 13:15).
 */
export function computeBulkEditSlots(dateStamp: string, startTime: string, endTime: string): string[] {
  const [y, mo, d] = dateStamp.split("-").map(Number);
  const startMin = hhmmToMinuteOfDay(startTime);
  const endMin = hhmmToMinuteOfDay(endTime);
  const firstSlotMinute = Math.ceil(startMin / 30) * 30;
  const lastSlotMinute = Math.floor((endMin - 30) / 30) * 30;
  const slots: string[] = [];
  for (let m = firstSlotMinute; m <= lastSlotMinute; m += 30) {
    slots.push(new Date(y, mo - 1, d, Math.floor(m / 60), m % 60).toISOString());
  }
  return slots;
}

/**
 * Every 30-minute work-slot start (:00 and :30, 00:00-23:30 -- 48 total) for
 * a local calendar date, unfiltered by whether a reflection exists yet.
 * Same Date-construction pattern as computeBulkEditSlots, just unbounded to
 * the whole day -- used by the Entries page's "View all" full-day view.
 */
export function getAllSlotStartsForDate(dateStamp: string): string[] {
  const [y, mo, d] = dateStamp.split("-").map(Number);
  const slots: string[] = [];
  for (let m = 0; m < 24 * 60; m += 30) {
    slots.push(new Date(y, mo - 1, d, Math.floor(m / 60), m % 60).toISOString());
  }
  return slots;
}

export interface BulkEditSlotPreview {
  slotStartIso: string;
  hasExisting: boolean;
}

/** Validates and resolves the slots a bulk edit would touch, plus whether
 * each already has a reflection -- so the UI can confirm ("N slots, M
 * already have an entry and will be overwritten") before anything is
 * written. Throws (rather than returning an error string) on invalid input,
 * since this is only ever called after the caller's own field-level
 * validation already passed. */
export async function previewBulkEditSlots(
  dateStamp: string,
  startTime: string,
  endTime: string,
): Promise<BulkEditSlotPreview[]> {
  const rangeError = validateBulkEditRange(startTime, endTime);
  if (rangeError) throw new Error(rangeError);
  const slots = computeBulkEditSlots(dateStamp, startTime, endTime);
  if (slots.length === 0) {
    throw new Error("No complete 30-minute slot falls within that time range");
  }
  return Promise.all(
    slots.map(async (slotStartIso) => ({
      slotStartIso,
      hasExisting: await isSlotCovered(slotStartIso),
    })),
  );
}

/** Re-validates and re-derives the slots rather than trusting a prior
 * previewBulkEditSlots call -- the fields may have changed since the user
 * last previewed. Returns the number of slots touched. */
export async function bulkUpsertReflections(
  dateStamp: string,
  startTime: string,
  endTime: string,
  text: string,
): Promise<number> {
  const trimmed = text.trim();
  if (!trimmed) throw new Error("Text is required");
  const rangeError = validateBulkEditRange(startTime, endTime);
  if (rangeError) throw new Error(rangeError);
  const slots = computeBulkEditSlots(dateStamp, startTime, endTime);
  if (slots.length === 0) {
    throw new Error("No complete 30-minute slot falls within that time range");
  }

  const db = await getDb();
  const createdAt = new Date().toISOString();
  // One encryption reused across every slot in the range, same reasoning as
  // saveReflection's; the timestamp rides in the same batch.
  const [stored, storedCreatedAt] = await encryptFields([trimmed, createdAt]);
  for (const slot of slots) {
    const existing = await db.select<{ id: number }[]>(
      `SELECT id FROM reflection WHERE slot_start_at = $1`,
      [slot],
    );
    if (existing.length > 0) {
      await db.execute(`UPDATE reflection SET text = $1, updated_at = $2 WHERE slot_start_at = $3`, [
        stored,
        storedCreatedAt,
        slot,
      ]);
    } else {
      await db.execute(
        `INSERT INTO reflection (created_at, slot_start_at, text, updated_at) VALUES ($1, $2, $3, $4)`,
        [storedCreatedAt, slot, stored, storedCreatedAt],
      );
    }
  }
  return slots.length;
}

// --- Bulk edit "Prefill" presets -----------------------------------------
//
// Named presets (a time range + text) a user can save once from the bulk-edit
// fields above and reapply with one click instead of retyping the same range
// for a recurring routine (sleep, lunch, ...). See CLAUDE.md's
// "Bulk-editing reflections by time range" section.

export interface BulkEditPreset {
  id: string;
  name: string;
  startTime: string;
  endTime: string;
  text: string;
}

const BULK_EDIT_PRESETS_SEEDED_KEY = "bulk_edit_presets_seeded";

/**
 * Seeds two example presets the first time this feature is used, so it's
 * immediately visible rather than starting as an empty list -- gated on an
 * app_setting flag, same one-shot pattern as ensureFirstRunMarker/
 * isOnboardingCompleted above, so this only ever runs once regardless of how
 * many times getBulkEditPresets() is called.
 */
async function seedDefaultBulkEditPresetsOnce(): Promise<void> {
  const db = await getDb();
  const rows = await db.select<{ value: string }[]>(
    `SELECT value FROM app_setting WHERE key = $1`,
    [BULK_EDIT_PRESETS_SEEDED_KEY],
  );
  if (rows.length > 0) return;

  const now = new Date().toISOString();
  const defaults: { name: string; startTime: string; endTime: string; text: string }[] = [
    { name: "Sleep", startTime: "00:00", endTime: "06:00", text: "Sleeping" },
    { name: "Lunch", startTime: "13:00", endTime: "14:00", text: "Lunch break" },
  ];
  // One batched encrypt call for all three encrypted fields across both rows
  // -- same "flatten across rows, one IPC round trip" shape
  // getWellnessSummaryForDate uses for its four booleans.
  const encrypted = await encryptFields(defaults.flatMap((d) => [d.startTime, d.endTime, d.text]));
  for (let i = 0; i < defaults.length; i++) {
    const [startTime, endTime, text] = encrypted.slice(i * 3, i * 3 + 3);
    await db.execute(
      `INSERT INTO bulk_edit_preset (id, name, start_time, end_time, text, created_at, updated_at)
       VALUES ($1, $2, $3, $4, $5, $6, $7)`,
      [crypto.randomUUID(), defaults[i].name, startTime, endTime, text, now, now],
    );
  }
  // ON CONFLICT DO NOTHING: a concurrent call (unlikely, but getBulkEditPresets
  // has no lock around this check-then-insert) should never seed twice.
  await db.execute(
    `INSERT INTO app_setting (key, value) VALUES ($1, '1') ON CONFLICT(key) DO NOTHING`,
    [BULK_EDIT_PRESETS_SEEDED_KEY],
  );
}

export async function getBulkEditPresets(): Promise<BulkEditPreset[]> {
  await seedDefaultBulkEditPresetsOnce();
  const db = await getDb();
  const rows = await db.select<{ id: string; name: string; start_time: string; end_time: string; text: string }[]>(
    `SELECT id, name, start_time, end_time, text FROM bulk_edit_preset ORDER BY name COLLATE NOCASE`,
  );
  const decrypted = await decryptFields(rows.flatMap((r) => [r.start_time, r.end_time, r.text]));
  return rows.map((row, i) => ({
    id: row.id,
    name: row.name,
    startTime: decrypted[i * 3],
    endTime: decrypted[i * 3 + 1],
    text: decrypted[i * 3 + 2],
  }));
}

/** Validates the plaintext inputs (same validateBulkEditRange guard the
 * bulk-edit fields themselves use) before anything is encrypted. */
export async function saveBulkEditPreset(
  name: string,
  startTime: string,
  endTime: string,
  text: string,
): Promise<void> {
  const trimmedName = name.trim();
  const trimmedText = text.trim();
  if (!trimmedName) throw new Error("Name is required");
  if (!trimmedText) throw new Error("Text is required");
  const rangeError = validateBulkEditRange(startTime, endTime);
  if (rangeError) throw new Error(rangeError);

  const db = await getDb();
  const [encStart, encEnd, encText] = await encryptFields([startTime, endTime, trimmedText]);
  const now = new Date().toISOString();
  await db.execute(
    `INSERT INTO bulk_edit_preset (id, name, start_time, end_time, text, created_at, updated_at)
     VALUES ($1, $2, $3, $4, $5, $6, $7)`,
    [crypto.randomUUID(), trimmedName, encStart, encEnd, encText, now, now],
  );
}

export async function deleteBulkEditPreset(id: string): Promise<void> {
  const db = await getDb();
  await db.execute(`DELETE FROM bulk_edit_preset WHERE id = $1`, [id]);
}

export async function getTaskList(dateStamp: string): Promise<string> {
  const db = await getDb();
  const rows = await db.select<{ content: string }[]>(
    `SELECT content FROM daily_task_list WHERE date = $1`,
    [dateStamp],
  );
  if (rows[0]?.content === undefined) return "";
  return decryptField(rows[0].content);
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
    [dateStamp, await encryptField(content), new Date().toISOString()],
  );
  // The broadcast below deliberately carries plaintext, unlike the row just
  // written: it's an in-memory hand-off between this app's own windows, and
  // every receiver would only have to decrypt it again to use it.
  //
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
  if (rows[0]?.content === undefined) return "";
  return decryptField(rows[0].content);
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
    [dateStamp, await encryptField(content), new Date().toISOString()],
  );
  // Plaintext broadcast, encrypted row -- see saveTaskList.
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
  /** Max successful breakit early-exits allowed per local calendar day. */
  maxPerDay: number;
}

const DEFAULT_BREAKIT: BreakitSettings = { length: 12, includeSpecial: false, maxPerDay: 5 };

export async function getBreakitSettings(): Promise<BreakitSettings> {
  const db = await getDb();
  const rows = await db.select<{ key: string; value: string }[]>(
    `SELECT key, value FROM app_setting WHERE key IN ('breakit_length', 'breakit_include_special', 'breakit_max_per_day')`,
  );
  const map = Object.fromEntries(rows.map((r) => [r.key, r.value]));
  return {
    length: numberOr(map.breakit_length, DEFAULT_BREAKIT.length),
    includeSpecial: (map.breakit_include_special ?? "false") === "true",
    maxPerDay: numberOr(map.breakit_max_per_day, DEFAULT_BREAKIT.maxPerDay),
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
  await db.execute(
    `INSERT INTO app_setting (key, value) VALUES ('breakit_max_per_day', $1)
     ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
    [String(settings.maxPerDay)],
  );
  await syncBreakitConfigToBackend(settings);
}

export async function syncBreakitConfigToBackend(settings: BreakitSettings): Promise<void> {
  await invoke("sync_breakit_config", {
    length: settings.length,
    includeSpecial: settings.includeSpecial,
    maxPerDay: settings.maxPerDay,
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
 * db.rs's SEED_SETTINGS creates it on a fresh install and every existing
 * database already has it -- but a missing setting should never silently
 * disable a kill switch).
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

// --- macOS menu bar/Dock hiding toggle (Settings) -----------------------
// macOS only in effect (see overlay.rs's spawn_or_update_overlay /
// macos_overlay.rs) -- whether a break additionally hides the menu bar and
// Dock. This is the *only* part of macOS break enforcement gated behind a
// setting: keeping the overlay visible across every Space and blocking
// Cmd+Tab are both always on. Off by default, unlike most other toggles here
// -- opt-in, since hiding system UI is a more disruptive change to the user's
// desktop than anything else this app does. Read/settable cross-platform like
// the other toggles so the Settings page doesn't need its own platform
// branching just to persist a value.

const MACOS_HIDE_MENU_BAR_DOCK_KEY = "macos_hide_menu_bar_dock_enabled";

export async function getMacosHideMenuBarDockEnabled(): Promise<boolean> {
  const db = await getDb();
  const rows = await db.select<{ value: string }[]>(
    `SELECT value FROM app_setting WHERE key = $1`,
    [MACOS_HIDE_MENU_BAR_DOCK_KEY],
  );
  return (rows[0]?.value ?? "false") === "true";
}

export async function saveMacosHideMenuBarDockEnabled(enabled: boolean): Promise<void> {
  const db = await getDb();
  await db.execute(
    `INSERT INTO app_setting (key, value) VALUES ($1, $2)
     ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
    [MACOS_HIDE_MENU_BAR_DOCK_KEY, String(enabled)],
  );
  await syncMacosHideMenuBarDockToBackend(enabled);
}

export async function syncMacosHideMenuBarDockToBackend(enabled: boolean): Promise<void> {
  await invoke("set_macos_hide_menu_bar_dock_enabled", { enabled });
}

/** Call once on app boot (main window) so Rust's in-memory flag matches SQLite. */
export async function loadAndSyncMacosHideMenuBarDockSetting(): Promise<boolean> {
  const enabled = await getMacosHideMenuBarDockEnabled();
  await syncMacosHideMenuBarDockToBackend(enabled);
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
// this; macOS's toggle is blind, so this keeps a relaunch or suspend-resume
// mid-break from toggling a second time for the same break and resuming the
// media the first toggle paused. Not a user-facing setting.

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

/** Call once on app boot (main window) so Rust's media-toggle guard matches SQLite. */
export async function loadAndSyncMediaToggleGuard(): Promise<void> {
  const lastToggleAt = await getLastMediaToggleTime();
  await invoke("sync_media_toggle_guard", { lastToggleAt });
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

// --- Screen time tracking (Settings) -----------------------------------
//
// Foreground-app focus tracking -- which app had focus, for how long (see
// screen_time.rs). Rust buffers sessions in memory, closes one only when a
// real app switch happens, and emits whatever accumulated as one batch every
// minute; everything below is the write side of that. SQLite stays
// frontend-written, the same split "media-toggle://recorded" already uses.

const SCREEN_TIME_TRACKING_KEY = "screen_time_tracking_enabled";

export async function getScreenTimeTrackingEnabled(): Promise<boolean> {
  const db = await getDb();
  const rows = await db.select<{ value: string }[]>(
    `SELECT value FROM app_setting WHERE key = $1`,
    [SCREEN_TIME_TRACKING_KEY],
  );
  return (rows[0]?.value ?? "true") === "true";
}

export async function saveScreenTimeTrackingEnabled(enabled: boolean): Promise<void> {
  const db = await getDb();
  await db.execute(
    `INSERT INTO app_setting (key, value) VALUES ($1, $2)
     ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
    [SCREEN_TIME_TRACKING_KEY, String(enabled)],
  );
  await syncScreenTimeTrackingToBackend(enabled);
}

export async function syncScreenTimeTrackingToBackend(enabled: boolean): Promise<void> {
  await invoke("set_screen_time_tracking_enabled", { enabled });
}

/** Call once on app boot (main window) so Rust's in-memory flag matches SQLite. */
export async function loadAndSyncScreenTimeTrackingSetting(): Promise<boolean> {
  const enabled = await getScreenTimeTrackingEnabled();
  await syncScreenTimeTrackingToBackend(enabled);
  return enabled;
}

// --- Screen time app threshold (Entries tab filter) ---------------------
//
// Apps with less than this many minutes of focus time on a given day are
// hidden from the Entries tab's per-app breakdown -- a day's tail is usually
// a long list of apps that briefly had focus for a few seconds (an alt-tab,
// a notification popup), which drowns out where the day actually went.
// Frontend-only filter, same as checkin_auto_close_minutes above: nothing in
// Rust reads this, so there's no backend value to sync.

const SCREEN_TIME_APP_THRESHOLD_KEY = "screen_time_app_threshold_minutes";
const DEFAULT_SCREEN_TIME_APP_THRESHOLD_MINUTES = 5;

export async function getScreenTimeAppThresholdMinutes(): Promise<number> {
  const db = await getDb();
  const rows = await db.select<{ value: string }[]>(
    `SELECT value FROM app_setting WHERE key = $1`,
    [SCREEN_TIME_APP_THRESHOLD_KEY],
  );
  return numberOr(rows[0]?.value, DEFAULT_SCREEN_TIME_APP_THRESHOLD_MINUTES);
}

export async function saveScreenTimeAppThresholdMinutes(minutes: number): Promise<void> {
  const db = await getDb();
  await db.execute(
    `INSERT INTO app_setting (key, value) VALUES ($1, $2)
     ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
    [SCREEN_TIME_APP_THRESHOLD_KEY, String(minutes)],
  );
}

// --- Device name (stamped onto every screen-time row) -------------------
//
// This app is single-device by design, but Settings -> Data export/import is
// the one path rows can cross devices -- and `platform` alone would merge
// "Chrome on laptop A" with "Chrome on laptop B" into one `windows` bucket.
// Seeded once from the OS hostname, renameable in Settings. Purely a display
// disambiguator: never an identity/sync key, so a non-unique or later-changed
// hostname is harmless.

const DEVICE_NAME_KEY = "device_name";

/** Read once per process and reused by saveScreenTimeSessions (which runs on
 * every flush) rather than re-queried per batch. Invalidated by saveDeviceName
 * so a rename takes effect on the very next batch. */
let cachedDeviceName: string | null = null;

export async function getDeviceName(): Promise<string> {
  const db = await getDb();
  const rows = await db.select<{ value: string }[]>(
    `SELECT value FROM app_setting WHERE key = $1`,
    [DEVICE_NAME_KEY],
  );
  return rows[0]?.value ?? "";
}

export async function saveDeviceName(name: string): Promise<void> {
  const db = await getDb();
  await db.execute(
    `INSERT INTO app_setting (key, value) VALUES ($1, $2)
     ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
    [DEVICE_NAME_KEY, name],
  );
  cachedDeviceName = name;
  // Re-registers the P2P LAN advertisement with the new name (see
  // p2p_sync.rs's resync_advertised_name/advertise_self doc comments) --
  // covers both call sites through this one function: ensureDeviceName's
  // cold-start auto-seed (which otherwise loses the race against the app's
  // very first advertisement, confirmed live) and Settings' manual rename
  // (which otherwise wouldn't take effect on peers until this app restarts).
  await invoke("resync_advertised_name").catch(() => {});
}

/** Seeds the device name from the OS hostname the first time, and only then --
 * an explicit rename (including back to empty) is never overwritten on a later
 * boot. Call once from the main window's boot sequence. */
export async function ensureDeviceName(): Promise<string> {
  const existing = await getDeviceName();
  if (existing) {
    cachedDeviceName = existing;
    return existing;
  }
  const hostname = (await invoke<string>("get_hostname")).trim();
  if (hostname) await saveDeviceName(hostname);
  cachedDeviceName = hostname;
  return hostname;
}

/** One closed focus session, exactly as Rust emits it. `device_name` is filled
 * in here rather than carried through Rust's buffer. `display_name` is a
 * best-effort friendly label (e.g. "Google Chrome" for app_id "chrome.exe")
 * -- may be "" wherever Rust couldn't resolve one (no version resource, or no
 * capture on this platform yet); every reader falls back to app_id then. */
export interface ScreenTimeSessionInput {
  app_id: string;
  display_name: string;
  platform: string;
  started_at: string;
  ended_at: string;
}

/** SQLite's default bound-variable ceiling is 999; at 7 columns per row this
 * keeps a chunk well under it while still collapsing a whole batch into one or
 * two statements instead of one per session. */
const SCREEN_TIME_INSERT_CHUNK = 100;

/** Empty `display_name` (no friendly name resolved) stays plain `""` rather
 * than being encrypted -- every reader already treats `""` as "no friendly
 * name, fall back to appId", and encrypting nothing would only cost a
 * decrypt on every later read for no benefit. Mirrors the same rule
 * import.rs's screen-time import uses. */
async function encryptDisplayNames(values: string[]): Promise<string[]> {
  const nonEmptyIndexes: number[] = [];
  const nonEmptyValues: string[] = [];
  values.forEach((v, i) => {
    if (v) {
      nonEmptyIndexes.push(i);
      nonEmptyValues.push(v);
    }
  });
  const encrypted = await encryptFields(nonEmptyValues);
  const result = values.map(() => "");
  nonEmptyIndexes.forEach((idx, j) => {
    result[idx] = encrypted[j];
  });
  return result;
}

export async function saveScreenTimeSessions(sessions: ScreenTimeSessionInput[]): Promise<void> {
  if (sessions.length === 0) return;
  const db = await getDb();
  const deviceName = cachedDeviceName ?? (await getDeviceName());
  cachedDeviceName = deviceName;

  for (let i = 0; i < sessions.length; i += SCREEN_TIME_INSERT_CHUNK) {
    const chunk = sessions.slice(i, i + SCREEN_TIME_INSERT_CHUNK);
    const appIds = chunk.map((s) => s.app_id);
    // app_id/display_name are encrypted the same way reflection.text is (see
    // crypto.rs's "Encryption at rest") -- a fresh random nonce every call,
    // so the same plaintext never produces the same ciphertext twice.
    // app_id_hash is a deterministic HMAC-SHA256 "blind index" of the
    // plaintext app_id computed alongside it, which is what lets
    // getScreenTimeForDate's GROUP BY and import.rs's duplicate check keep
    // working in SQL despite that. Never fall back to writing plaintext on
    // failure -- a rejected Promise here propagates to the caller's .catch
    // (listenForScreenTimeSessionBatches), which logs and drops the batch.
    const [encryptedAppIds, appIdHashes, encryptedDisplayNames] = await Promise.all([
      encryptFields(appIds),
      hashAppIds(appIds),
      encryptDisplayNames(chunk.map((s) => s.display_name)),
    ]);

    const values: string[] = [];
    const placeholders = chunk
      .map((session, index) => {
        const base = index * 7;
        values.push(
          encryptedAppIds[index],
          encryptedDisplayNames[index],
          appIdHashes[index],
          session.platform,
          deviceName,
          session.started_at,
          session.ended_at,
        );
        return `($${base + 1}, $${base + 2}, $${base + 3}, $${base + 4}, $${base + 5}, $${base + 6}, $${base + 7})`;
      })
      .join(", ");
    await db.execute(
      `INSERT INTO screen_time_session (app_id, display_name, app_id_hash, platform, device_name, started_at, ended_at)
       VALUES ${placeholders}`,
      values,
    );
  }
}

/** Persists each batch Rust flushes. Call once from the main window (the
 * always-alive route -- it's hidden, not destroyed, on close, so its listeners
 * keep receiving batches while the app runs tray-only); unlisten in onDestroy
 * like every other listener in this app. */
export async function listenForScreenTimeSessionBatches(): Promise<UnlistenFn> {
  return listen<ScreenTimeSessionInput[]>("screentime://session-batch", (event) => {
    // Failures are logged, never swallowed: Rust considers a batch delivered
    // once the event is emitted and drops it from its buffer, so a rejected
    // insert here is data gone with nothing else to notice it -- exactly the
    // silent-write-failure class this codebase has been bitten by before.
    void saveScreenTimeSessions(event.payload).catch((e) => {
      void logError(
        `screen time: failed to persist ${event.payload.length} session(s): ${e instanceof Error ? e.message : String(e)}`,
      );
    });
  });
}

export interface ScreenTimeEntry {
  appId: string;
  /** Falls back to appId wherever no friendly name could be resolved --
   * always safe to render directly, never "". */
  displayName: string;
  platform: string;
  deviceName: string;
  ms: number;
}

/**
 * Per-app totals for one local day, aggregated in SQL rather than by pulling
 * raw rows into JS -- same approach getWellnessSummaryForDate takes, and it
 * matters more here since this is the table that grows fastest.
 *
 * Filed by the local date the session *started* on (`date(started_at,
 * 'localtime')`, the same DST-aware conversion getReflectionsForDate uses), so
 * a session running across midnight counts entirely toward the day it began --
 * simple and stable, at the cost of a little drift for anyone working through
 * midnight.
 *
 * Grouped by `app_id_hash`, not `app_id` itself -- `app_id`/`display_name`
 * are crypto.rs's usual
 * random-nonce ciphertext (see "Encryption at rest"), which never produces
 * the same value twice for the same plaintext, so grouping on it directly
 * would put every session in its own group. `app_id_hash` is the
 * deterministic HMAC-SHA256 "blind index" computed alongside it precisely so
 * this grouping keeps working in SQL. `MIN(app_id)`/`MAX(display_name)` just
 * pick one representative ciphertext per group -- every row in a group
 * decrypts to the same plaintext app_id, and MAX naturally prefers a
 * non-empty ciphertext label over plain `""` when both exist in a group,
 * same property the pre-encryption code relied on.
 *
 * Grouped by device_name as well as the app identity, so the same app on two
 * devices (only possible after a cross-device import) reads as two rows
 * instead of being silently summed -- invisible in the ordinary
 * single-device case.
 */
export async function getScreenTimeForDate(dateStamp: string): Promise<ScreenTimeEntry[]> {
  const db = await getDb();
  const rows = await db.select<
    {
      app_id: string;
      display_name: string | null;
      platform: string;
      device_name: string;
      ms: number | null;
    }[]
  >(
    `SELECT MIN(app_id) as app_id,
            MAX(display_name) as display_name,
            platform,
            device_name,
            SUM((julianday(ended_at) - julianday(started_at)) * 86400000) as ms
     FROM screen_time_session
     WHERE date(started_at, 'localtime') = $1
     GROUP BY app_id_hash, platform, device_name
     ORDER BY ms DESC`,
    [dateStamp],
  );

  // One batched decrypt for the whole day's distinct apps (a handful of
  // values), not one per session row -- the entire point of grouping by
  // app_id_hash instead of decrypting every row up front.
  const appIds = await decryptFields(rows.map((r) => r.app_id));
  const displayNames = await decryptFields(rows.map((r) => r.display_name || ""));

  return rows.map((row, i) => ({
    appId: appIds[i],
    displayName: displayNames[i] || appIds[i],
    platform: row.platform,
    deviceName: row.device_name,
    ms: Math.max(0, Math.round(row.ms ?? 0)),
  }));
}

export interface CurrentScreenTimeSession {
  appId: string;
  /** Falls back to appId, same as ScreenTimeEntry.displayName. */
  displayName: string;
  elapsedMs: number;
}

/** The app in focus right now and how long it's been focused, read from Rust's
 * in-memory buffer -- nothing is written and no row is created. Blended into
 * today's breakdown so the in-progress app isn't missing from it (persisted
 * rows only cover sessions a real switch already closed). `null` when nothing
 * is being tracked (tracking off, Reflectodoro itself in focus, or no capture
 * on this platform yet). */
export async function getCurrentScreenTimeSession(): Promise<CurrentScreenTimeSession | null> {
  const snapshot = await invoke<{ app_id: string; display_name: string; elapsed_ms: number } | null>(
    "get_current_session_snapshot",
  );
  if (!snapshot) return null;
  return {
    appId: snapshot.app_id,
    displayName: snapshot.display_name || snapshot.app_id,
    elapsedMs: Math.max(0, snapshot.elapsed_ms),
  };
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
  slot_start_at: string;
  relaxed_eyes: number;
  exercise: number;
  drank_water: number;
  washroom: number;
  created_at: string;
}

export interface ScreenTimeSessionRow {
  id: number;
  app_id: string;
  display_name: string;
  platform: string;
  device_name: string;
  started_at: string;
  ended_at: string;
}

export interface BulkEditPresetRow {
  id: string;
  name: string;
  start_time: string;
  end_time: string;
  text: string;
  created_at: string;
  updated_at: string;
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
    screen_time_session: ScreenTimeSessionRow[];
    bulk_edit_preset: BulkEditPresetRow[];
  };
}

export async function exportAllData(includeSettings: boolean = true): Promise<ExportPayload> {
  const db = await getDb();
  const [
    reflection,
    daily_task_list,
    not_to_do_list,
    app_setting,
    wellness_check,
    screen_time_session,
    bulk_edit_preset,
  ] = await Promise.all([
    db.select<ReflectionRow[]>(`SELECT id, created_at, slot_start_at, text FROM reflection`),
    db.select<TaskListRow[]>(`SELECT date, content, updated_at FROM daily_task_list`),
    db.select<NotToDoRow[]>(`SELECT date, content, updated_at FROM not_to_do_list`),
    includeSettings
      ? db.select<SettingRow[]>(`SELECT key, value FROM app_setting`)
      : Promise.resolve([]),
    db.select<{ slot_start_at: string; relaxed_eyes: string; exercise: string; drank_water: string; washroom: string; created_at: string }[]>(
      `SELECT slot_start_at, relaxed_eyes, exercise, drank_water, washroom, created_at FROM wellness_check`,
    ),
    db.select<ScreenTimeSessionRow[]>(
      `SELECT id, app_id, display_name, platform, device_name, started_at, ended_at FROM screen_time_session`,
    ),
    db.select<BulkEditPresetRow[]>(
      `SELECT id, name, start_time, end_time, text, created_at, updated_at FROM bulk_edit_preset`,
    ),
  ]);
  // The export file is deliberately plaintext (a deliberate, documented
  // decision -- see CLAUDE.md's "Encryption at rest"): it's a portability
  // format meant to be re-importable on another device, which holds an
  // entirely unrelated key, so shipping ciphertext would make it unreadable
  // everywhere including here. Encryption protects the live pomodoro.db, not
  // a file the user explicitly chose to write somewhere of their choosing.
  const [
    reflectionTexts,
    reflectionCreatedAts,
    taskContents,
    notToDoContents,
    wellnessValues,
    screenTimeAppIds,
    screenTimeDisplayNames,
    bulkEditPresetValues,
  ] = await Promise.all([
    decryptFields(reflection.map((r) => r.text)),
    // created_at is ciphertext at rest now, and the export file is
    // deliberately plaintext, so it decrypts on the way out like everything
    // else here. `rev` is never selected at all: it's a device-local counter
    // (same exclusion as app_id_hash), and the importing device's triggers
    // assign their own.
    decryptFields(reflection.map((r) => r.created_at)),
    decryptFields(daily_task_list.map((r) => r.content)),
    decryptFields(not_to_do_list.map((r) => r.content)),
    decryptFields(wellness_check.flatMap((r) => [r.relaxed_eyes, r.exercise, r.drank_water, r.washroom])),
    decryptFields(screen_time_session.map((r) => r.app_id)),
    decryptFields(screen_time_session.map((r) => r.display_name)),
    decryptFields(bulk_edit_preset.flatMap((r) => [r.start_time, r.end_time, r.text])),
  ]);
  return {
    app: "reflectodoro",
    export_format_version: EXPORT_FORMAT_VERSION,
    exported_at: new Date().toISOString(),
    data: {
      reflection: reflection.map((row, i) => ({
        ...row,
        created_at: reflectionCreatedAts[i],
        text: reflectionTexts[i],
      })),
      daily_task_list: daily_task_list.map((row, i) => ({ ...row, content: taskContents[i] })),
      not_to_do_list: not_to_do_list.map((row, i) => ({ ...row, content: notToDoContents[i] })),
      app_setting,
      wellness_check: wellness_check.map((row, i) => ({
        slot_start_at: row.slot_start_at,
        relaxed_eyes: Number(wellnessValues[i * 4]),
        exercise: Number(wellnessValues[i * 4 + 1]),
        drank_water: Number(wellnessValues[i * 4 + 2]),
        washroom: Number(wellnessValues[i * 4 + 3]),
        created_at: row.created_at,
      })),
      // app_id_hash is deliberately left out of every exported row -- it's a
      // blind index keyed to this device's own key, meaningless (and
      // potentially misleading) once re-imported on a different device with
      // an unrelated key. import.rs recomputes it on the way in.
      screen_time_session: screen_time_session.map((row, i) => ({
        ...row,
        app_id: screenTimeAppIds[i],
        display_name: screenTimeDisplayNames[i],
      })),
      bulk_edit_preset: bulk_edit_preset.map((row, i) => ({
        ...row,
        start_time: bulkEditPresetValues[i * 3],
        end_time: bulkEditPresetValues[i * 3 + 1],
        text: bulkEditPresetValues[i * 3 + 2],
      })),
    },
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
    "breakit_max_per_day",
    "overlay_auto_close_minutes",
    "checkin_auto_close_minutes",
    "screen_time_app_threshold_minutes",
  ]);
  for (const row of app_setting) {
    if (NUMERIC_SETTING_KEYS.has(row.key) && !Number.isFinite(Number(row.value))) {
      throw new Error(`data.app_setting has a non-numeric value for "${row.key}": ${JSON.stringify(row.value)}`);
    }
  }

  const wellnessRaw = data.wellness_check;
  if (!Array.isArray(wellnessRaw)) throw new Error("data.wellness_check is missing or not an array");
  const wellness_check: WellnessCheckRow[] = wellnessRaw.map((row, i) => {
    if (typeof row !== "object" || row === null) throw new Error(`wellness_check[${i}] is not an object`);
    const r = row as Record<string, unknown>;
    return {
      slot_start_at: assertString(r.slot_start_at, `wellness_check[${i}].slot_start_at`),
      relaxed_eyes: assertNumber(r.relaxed_eyes, `wellness_check[${i}].relaxed_eyes`),
      exercise: assertNumber(r.exercise, `wellness_check[${i}].exercise`),
      drank_water: assertNumber(r.drank_water, `wellness_check[${i}].drank_water`),
      washroom: assertNumber(r.washroom, `wellness_check[${i}].washroom`),
      created_at: assertString(r.created_at, `wellness_check[${i}].created_at`),
    };
  });

  // Deliberately tolerant of absence rather than gated behind a bumped
  // EXPORT_FORMAT_VERSION: screen_time_session arrived after the format did,
  // and rejecting every file exported before it -- the user's existing
  // backups -- to gain a field that's purely additive would be a bad trade.
  // A missing array simply imports as no screen-time rows.
  const screenTimeRaw = data.screen_time_session ?? [];
  if (!Array.isArray(screenTimeRaw)) throw new Error("data.screen_time_session is not an array");
  const screen_time_session: ScreenTimeSessionRow[] = screenTimeRaw.map((row, i) => {
    if (typeof row !== "object" || row === null) throw new Error(`screen_time_session[${i}] is not an object`);
    const r = row as Record<string, unknown>;
    return {
      id: assertNumber(r.id, `screen_time_session[${i}].id`),
      app_id: assertString(r.app_id, `screen_time_session[${i}].app_id`),
      // Tolerant default, not assertString: display_name arrived after
      // screen_time_session itself did, so a file exported in that window
      // has rows without it -- same "purely additive" reasoning as the
      // missing-array case above, just at the per-row level instead.
      display_name: typeof r.display_name === "string" ? r.display_name : "",
      platform: assertString(r.platform, `screen_time_session[${i}].platform`),
      device_name: assertString(r.device_name, `screen_time_session[${i}].device_name`),
      started_at: assertString(r.started_at, `screen_time_session[${i}].started_at`),
      ended_at: assertString(r.ended_at, `screen_time_session[${i}].ended_at`),
    };
  });

  // Same "tolerant of absence" reasoning as screen_time_session above --
  // bulk_edit_preset arrived after the export format did.
  const bulkEditPresetRaw = data.bulk_edit_preset ?? [];
  if (!Array.isArray(bulkEditPresetRaw)) throw new Error("data.bulk_edit_preset is not an array");
  const bulk_edit_preset: BulkEditPresetRow[] = bulkEditPresetRaw.map((row, i) => {
    if (typeof row !== "object" || row === null) throw new Error(`bulk_edit_preset[${i}] is not an object`);
    const r = row as Record<string, unknown>;
    const start_time = assertString(r.start_time, `bulk_edit_preset[${i}].start_time`);
    const end_time = assertString(r.end_time, `bulk_edit_preset[${i}].end_time`);
    const rangeError = validateBulkEditRange(start_time, end_time);
    if (rangeError) throw new Error(`bulk_edit_preset[${i}]: ${rangeError}`);
    return {
      id: assertString(r.id, `bulk_edit_preset[${i}].id`),
      name: assertString(r.name, `bulk_edit_preset[${i}].name`),
      start_time,
      end_time,
      text: assertString(r.text, `bulk_edit_preset[${i}].text`),
      created_at: assertString(r.created_at, `bulk_edit_preset[${i}].created_at`),
      updated_at: assertString(r.updated_at, `bulk_edit_preset[${i}].updated_at`),
    };
  });
  assertNoDuplicates(bulk_edit_preset.map((r) => r.id), "bulk_edit_preset", "id");

  return {
    app: "reflectodoro",
    export_format_version: obj.export_format_version,
    exported_at: obj.exported_at,
    data: {
      reflection,
      daily_task_list,
      not_to_do_list,
      app_setting,
      wellness_check,
      screen_time_session,
      bulk_edit_preset,
    },
  };
}

export type ImportMode = "replace" | "merge";

export interface ImportResult {
  reflectionCount: number;
  taskListCount: number;
  notToDoListCount: number;
  settingCount: number;
  wellnessCheckCount: number;
  screenTimeSessionCount: number;
  /** How many slots had more than one reflection row (pre-existing
   * duplicates and/or multiple imported rows for that slot) collapse into
   * one surviving row -- see import.rs's per-slot merge algorithm. */
  mergedSlotCount: number;
  /** How many imported screen_time_session rows were skipped because a row
   * with the same (app_id, platform, device_name, started_at, ended_at)
   * already existed -- always 0 in "replace" mode. */
  screenTimeDuplicateCount: number;
  /** How many imported wellness_check rows lost the "keep the earliest
   * created_at per slot_start_at" collapse to an existing row or another
   * imported row -- see import.rs's import_wellness_checks. Always 0 in
   * "replace" mode. */
  wellnessCheckDuplicateCount: number;
  bulkEditPresetCount: number;
  /** How many imported bulk_edit_preset rows were skipped because an
   * existing row with the same id had an updated_at that was already newer
   * (last-write-wins) -- see import.rs's import_bulk_edit_presets. Always 0
   * in "replace" mode. */
  bulkEditPresetStaleCount: number;
}

/**
 * Applies a validated export payload to the DB by invoking the Rust
 * `import_data` command (`src-tauri/src/import.rs`), which runs the whole
 * operation -- replace-mode wipes, "merge" mode's per-slot reflection line
 * merge (existing lines and imported lines combined git-merge-style, "Skip"
 * placeholder lines dropped once real content arrives), daily_task_list/
 * not_to_do_list/app_setting upserts (imported wins on key conflict), and
 * the wellness_check FK remap -- inside one real `sqlx` transaction on a
 * dedicated connection. That fixes what used to be a real gap here: sending
 * `BEGIN`/`COMMIT` through tauri-plugin-sql's own `execute()` isn't reliably
 * atomic, since its pooled connections aren't guaranteed to stay pinned
 * across calls.
 *
 * `parseAndValidateExport()` still fully validates the payload client-side
 * before this is ever called -- that hasn't moved. Everything below this
 * point is just the invoke call plus the settings-cache resyncs, which stay
 * here since they only read SQLite (via the plugin) and refresh in-memory
 * Rust state -- no writes of their own to keep transactional.
 */
export async function importData(
  payload: ExportPayload,
  mode: ImportMode,
  includeSettings: boolean = true,
): Promise<ImportResult> {
  const result = await invoke<ImportResult>("import_data", {
    data: payload.data,
    mode,
    includeSettings,
  });

  if (includeSettings) {
    // Rust's in-memory breakit config, force-close-shortcut flag, and overlay
    // auto-close minutes are all caches of app_setting -- resync so an
    // imported value takes effect immediately, not just after the next app
    // restart.
    await loadAndSyncBreakitSettings();
    await loadAndSyncForceCloseShortcutSetting();
    await loadAndSyncOverlayAutoClose();
    await loadAndSyncMediaPauseOnBreakSetting();
    await loadAndSyncScreenTimeTrackingSetting();
    await loadAndSyncMediaToggleGuard();
    // device_name is read through a process-lifetime cache (see
    // cachedDeviceName) -- drop it so an imported value doesn't keep getting
    // stamped onto new rows from the pre-import name until the next restart.
    cachedDeviceName = null;
  }

  return result;
}

// --- P2P LAN device pairing/sync (Settings > Data > Paired devices) -----
//
// Thin invoke() wrappers around p2p_sync.rs -- all the actual pairing/sync
// logic (SPAKE2 handshake, Noise-encrypted transport, delta queries, merge)
// lives in Rust; this module just gives the Settings UI typed calls. See
// CLAUDE.md's "P2P LAN sync" section for the full design.

export interface DiscoveredDevice {
  deviceId: string;
  name: string;
  platform: string;
}

export interface PairedDeviceInfo {
  deviceId: string;
  name: string;
  platform: string;
  pairedAt: string;
  lastSyncAt: string | null;
  /** A fresh LAN-presence snapshot from the moment this was fetched, not a
   * stored flag -- re-fetch (getPairedDevices/browseOnlinePairedDevices)
   * rather than trusting a stale value. */
  online: boolean;
  /** Per-device opt-in for automatic sync (see maybeAutoSync in Rust),
   * local to this side's own copy of the pairing record -- never part of
   * the sync payload, so the peer's own choice is independent of this. */
  autoSyncEnabled: boolean;
}

export interface SyncResult {
  /** Rows this device sent to the peer -- a single total across all six
   * synced tables, not broken down. The fields below are the other
   * direction (received from the peer and applied here); without this, a
   * sync that only moved data outward reported "0 rows" even though it
   * worked, since the count only ever reflected what came back. */
  sentCount: number;
  reflectionCount: number;
  taskListCount: number;
  notToDoListCount: number;
  wellnessCheckCount: number;
  screenTimeSessionCount: number;
  mergedSlotCount: number;
  screenTimeDuplicateCount: number;
  wellnessCheckDuplicateCount: number;
  bulkEditPresetCount: number;
}

/** Opens a ~60s pairing window on this device and returns the PIN to show
 * the user -- they read it aloud/type it into the *other* device's "Enter
 * PIN" step (confirmPairing). Starting a new session replaces any still-open
 * previous one. */
export async function startPairing(): Promise<string> {
  return invoke<string>("start_pairing");
}

/** Closes an open pairing window early (e.g. the user backed out of the
 * dialog before a PIN was entered anywhere). */
export async function cancelPairing(): Promise<void> {
  await invoke("cancel_pairing");
}

/** Devices currently advertising on the LAN that aren't already paired with
 * this one -- what the "Pair a new device" flow's device picker lists. */
export async function browsePairingCandidates(): Promise<DiscoveredDevice[]> {
  return invoke<DiscoveredDevice[]>("browse_pairing_candidates");
}

/** Dials `deviceId` (from browsePairingCandidates) and runs the joiner side
 * of the pairing handshake using the PIN shown on that device's own "Pair a
 * new device" screen. Throws with a user-facing message on a wrong PIN or an
 * unreachable/no-longer-pairing peer. */
export async function confirmPairing(deviceId: string, pin: string): Promise<PairedDeviceInfo> {
  return invoke<PairedDeviceInfo>("confirm_pairing", { deviceId, pin });
}

/** Every paired device, each with a fresh online/offline snapshot -- backs
 * the Settings "Paired devices" list. */
export async function getPairedDevices(): Promise<PairedDeviceInfo[]> {
  return invoke<PairedDeviceInfo[]>("get_paired_devices");
}

/** Just the currently-online subset -- what the "Import from device"
 * dropdown populates itself from. */
export async function browseOnlinePairedDevices(): Promise<PairedDeviceInfo[]> {
  return invoke<PairedDeviceInfo[]>("browse_online_paired_devices");
}

/** Removes a paired device (and its shared key) from this device. Does not
 * affect the other device's own paired_device row -- forgetting is
 * one-sided; re-pairing requires running the PIN flow again on both sides. */
export async function forgetPairedDevice(deviceId: string): Promise<void> {
  await invoke("forget_paired_device", { deviceId });
}

/** Runs a bidirectional delta sync with an already-paired, currently-online
 * device: sends this device's changes since the pair's last successful sync
 * and applies whatever the other device sends back, via the same merge
 * logic as a manual file import ("merge" mode -- never "replace"). Settings
 * (app_setting) are never part of the payload in either direction. */
export async function syncWithDevice(deviceId: string): Promise<SyncResult> {
  return invoke<SyncResult>("sync_with_device", { deviceId });
}

/** Flips one paired device's auto-sync opt-in. Local-only -- never affects
 * the peer's own copy of this pairing. */
export async function setDeviceAutoSyncEnabled(deviceId: string, enabled: boolean): Promise<void> {
  await invoke("set_device_auto_sync_enabled", { deviceId, enabled });
}

/** Fire-and-forget: asks Rust to attempt an auto-sync check right now
 * (same maybe_auto_sync logic the break-boundary trigger uses), for the
 * window-focus trigger -- see CLAUDE.md's "P2P LAN sync". */
export async function attemptAutoSync(): Promise<void> {
  await invoke("attempt_auto_sync");
}

// --- Quote API (end-of-break overlay panel) ------------------------------
//
// Blank URL = feature off. Defaults to zenquotes.io's random-quote endpoint,
// seeded as a real app_setting row by db.rs's SEED_SETTINGS -- not a
// read-side fallback here, so clearing the field in Settings genuinely
// disables it (an explicit saved "" is never overridden). The URL itself is
// stored here (plain app_setting, read by the overlay page), but the actual
// fetch happens in Rust via the fetch_quote command -- see commands.rs's doc
// comment for why (CORS, and this being the app's first arbitrary-
// third-party outbound call).

const QUOTE_API_URL_KEY = "quote_api_url";

export async function getQuoteApiUrl(): Promise<string> {
  const db = await getDb();
  const rows = await db.select<{ value: string }[]>(
    `SELECT value FROM app_setting WHERE key = $1`,
    [QUOTE_API_URL_KEY],
  );
  return rows[0]?.value ?? "";
}

export async function saveQuoteApiUrl(url: string): Promise<void> {
  const db = await getDb();
  await db.execute(
    `INSERT INTO app_setting (key, value) VALUES ($1, $2)
     ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
    [QUOTE_API_URL_KEY, url],
  );
}
