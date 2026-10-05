// Pure helpers for the Habits tab (see CLAUDE.md's "Habit tracker"). No
// Tauri or DB imports here on purpose: everything in this file is plain
// computation over already-decrypted rows, so it can be exercised on its own.
// Storage lives in db.ts.

/** Palette keys. The stored value is the key, never a hex code, so the actual
 * colours can follow the theme (`--habit-<key>` tokens in app.css). */
export const HABIT_COLORS = ["rose", "amber", "lime", "teal", "sky", "indigo", "violet", "slate"] as const;
export type HabitColor = (typeof HABIT_COLORS)[number];

export const HABIT_NAME_MAX = 60;
export const HABIT_EMOJI_MAX = 8;
export const HABIT_NOTE_MAX = 2000;
export const HABIT_TARGET_MAX_DAYS = 3650;

export interface Habit {
  id: string;
  name: string;
  emoji: string;
  color: HabitColor;
  /** "Every N days" target, or null for none. */
  targetDays: number | null;
  sortOrder: number;
  archived: boolean;
}

export interface HabitLog {
  id: string;
  habitId: string;
  /** Local wall-clock date, 'YYYY-MM-DD'. */
  date: string;
  /** 'HH:MM', or '' when no time was given. */
  time: string;
  note: string;
}

export interface HabitInput {
  name: string;
  emoji: string;
  color: string;
  /** Raw form value: '' / null = no target. A number when it comes from a
   * bound `<input type="number">`, which Svelte binds as a number. */
  targetDays: string | number | null;
}

export interface NormalizedHabitInput {
  name: string;
  emoji: string;
  color: HabitColor;
  targetDays: number | null;
}

export function toHabitColor(value: string): HabitColor {
  return (HABIT_COLORS as readonly string[]).includes(value) ? (value as HabitColor) : HABIT_COLORS[0];
}

/** Parses the stored/entered target. Anything that isn't a whole number in
 * range reads as "no target" rather than throwing, so a hand-edited or odd
 * imported value never breaks the page. */
export function parseTargetDays(value: string): number | null {
  const trimmed = value.trim();
  if (!/^\d+$/.test(trimmed)) return null;
  const n = Number(trimmed);
  return n >= 1 && n <= HABIT_TARGET_MAX_DAYS ? n : null;
}

/** Validates the add/edit habit form. Throws with a user-facing message. */
export function normalizeHabitInput(input: HabitInput): NormalizedHabitInput {
  const name = input.name.trim();
  if (!name) throw new Error("Name is required");
  if (name.length > HABIT_NAME_MAX) throw new Error(`Name must be at most ${HABIT_NAME_MAX} characters`);
  const emoji = input.emoji.trim();
  if (emoji.length > HABIT_EMOJI_MAX) throw new Error("Emoji is too long");
  const rawTarget = String(input.targetDays ?? "").trim();
  const targetDays = rawTarget === "" ? null : parseTargetDays(rawTarget);
  if (rawTarget !== "" && targetDays === null) {
    throw new Error(`Target must be a whole number of days between 1 and ${HABIT_TARGET_MAX_DAYS}`);
  }
  return { name, emoji, color: toHabitColor(input.color), targetDays };
}

const DATE_RE = /^\d{4}-\d{2}-\d{2}$/;
const TIME_RE = /^([01]\d|2[0-3]):[0-5]\d$/;

/** Validates a log entry. `todayStamp` is passed in (not read from the clock)
 * so the check stays pure. Future dates are refused: this logs what happened,
 * and a future date would also make "days since" negative. */
export function normalizeLogInput(
  date: string,
  time: string,
  note: string,
  todayStamp: string,
): { date: string; time: string; note: string } {
  if (!DATE_RE.test(date) || Number.isNaN(dayNumber(date))) throw new Error("Pick a valid date");
  if (date > todayStamp) throw new Error("The date can't be in the future");
  const t = time.trim();
  if (t !== "" && !TIME_RE.test(t)) throw new Error("Time must be HH:MM");
  const n = note.trim();
  if (n.length > HABIT_NOTE_MAX) throw new Error(`Description must be at most ${HABIT_NOTE_MAX} characters`);
  return { date, time: t, note: n };
}

/** Whole days since 1970-01-01 for a 'YYYY-MM-DD' stamp. Uses UTC so the
 * difference between two dates is always a whole number, regardless of the
 * device's timezone or a DST change between them. */
export function dayNumber(stamp: string): number {
  const [y, m, d] = stamp.split("-").map(Number);
  return Math.round(Date.UTC(y, m - 1, d) / 86_400_000);
}

export function daysBetween(fromStamp: string, toStamp: string): number {
  return dayNumber(toStamp) - dayNumber(fromStamp);
}

export function timeHHMM(d: Date = new Date()): string {
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

/** Newest first: by date, then time (a log with no time sorts after timed
 * logs on the same day). */
export function sortLogsNewestFirst(logs: HabitLog[]): HabitLog[] {
  return [...logs].sort((a, b) => {
    if (a.date !== b.date) return a.date < b.date ? 1 : -1;
    if (a.time === b.time) return 0;
    if (a.time === "") return 1;
    if (b.time === "") return -1;
    return a.time < b.time ? 1 : -1;
  });
}

export function logsByDate(logs: HabitLog[]): Map<string, HabitLog[]> {
  const map = new Map<string, HabitLog[]>();
  for (const log of logs) {
    const list = map.get(log.date);
    if (list) list.push(log);
    else map.set(log.date, [log]);
  }
  return map;
}

export interface HabitStats {
  total: number;
  lastDate: string | null;
  daysSince: number | null;
  /** Mean days between distinct logged dates; null with fewer than two. */
  avgGapDays: number | null;
  /** Logs dated within the last 30 days, today included. */
  countLast30: number;
  overdue: boolean;
}

/** `logs` must all belong to one habit. */
export function habitStats(logs: HabitLog[], targetDays: number | null, todayStamp: string): HabitStats {
  const dates = [...new Set(logs.map((l) => l.date))].sort();
  const lastDate = dates.length > 0 ? dates[dates.length - 1] : null;
  const daysSince = lastDate === null ? null : daysBetween(lastDate, todayStamp);
  const avgGapDays =
    dates.length < 2
      ? null
      : Math.round((daysBetween(dates[0], dates[dates.length - 1]) / (dates.length - 1)) * 10) / 10;
  const today = dayNumber(todayStamp);
  const countLast30 = logs.filter((l) => {
    const age = today - dayNumber(l.date);
    return age >= 0 && age < 30;
  }).length;
  return {
    total: logs.length,
    lastDate,
    daysSince,
    avgGapDays,
    countLast30,
    overdue: targetDays !== null && daysSince !== null && daysSince > targetDays,
  };
}

export interface DueHabit {
  habit: Habit;
  /** Whole days since the last log, or null when never logged. */
  daysSince: number | null;
  /** Days past the target (0 = due today), or null when never logged. */
  overdueDays: number | null;
}

/**
 * Habits that are due on `todayStamp`: visible, with an "every N days" target,
 * and either never logged or last logged at least N days ago. A habit without a
 * target is never due (there's nothing to be due against). Due on the target
 * day itself, a day earlier than the Habits tab's "Overdue" badge (which needs
 * days-since > target). Most overdue first, never-logged last, ties by the
 * habit's own sort order.
 */
export function dueHabits(habits: Habit[], logs: HabitLog[], todayStamp: string): DueHabit[] {
  const logsByHabit = new Map<string, HabitLog[]>();
  for (const log of logs) {
    const list = logsByHabit.get(log.habitId);
    if (list) list.push(log);
    else logsByHabit.set(log.habitId, [log]);
  }
  const due: DueHabit[] = [];
  for (const habit of habits) {
    if (habit.archived || habit.targetDays === null) continue;
    const { daysSince } = habitStats(logsByHabit.get(habit.id) ?? [], habit.targetDays, todayStamp);
    if (daysSince === null) {
      due.push({ habit, daysSince, overdueDays: null });
    } else if (daysSince >= habit.targetDays) {
      due.push({ habit, daysSince, overdueDays: daysSince - habit.targetDays });
    }
  }
  return due.sort((a, b) => {
    if ((a.overdueDays === null) !== (b.overdueDays === null)) return a.overdueDays === null ? 1 : -1;
    if (a.overdueDays !== b.overdueDays) return (b.overdueDays ?? 0) - (a.overdueDays ?? 0);
    return a.habit.sortOrder - b.habit.sortOrder;
  });
}

/** "Due today", "Overdue by 2 days" or "Not logged yet". */
export function formatDueStatus(item: DueHabit): string {
  if (item.overdueDays === null) return "Not logged yet";
  if (item.overdueDays <= 0) return "Due today";
  return `Overdue by ${item.overdueDays} day${item.overdueDays === 1 ? "" : "s"}`;
}

/** "today", "yesterday", "5d ago", or "never". */
export function formatDaysSince(daysSince: number | null): string {
  if (daysSince === null) return "never";
  if (daysSince <= 0) return "today";
  if (daysSince === 1) return "yesterday";
  return `${daysSince}d ago`;
}
