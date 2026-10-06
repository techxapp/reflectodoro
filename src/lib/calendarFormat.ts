// Pure presentation helpers for the home screen's "Upcoming events" card.
// No Tauri imports on purpose (same as habits.ts), so they can be exercised
// directly. Everything is computed in the viewer's local timezone: all-day
// events are plain `YYYY-MM-DD` strings and are never run through `Date`'s
// UTC parsing, so they can't slide to a neighbouring day.

export interface FormattableEvent {
  start: string;
  end: string;
  allDay: boolean;
}

export interface EventDayGroup<T extends FormattableEvent> {
  /** `YYYY-MM-DD` in local time. */
  dayKey: string;
  /** "Today", "Tomorrow", or e.g. "Fri, Oct 10". */
  label: string;
  events: T[];
}

function pad(n: number): string {
  return String(n).padStart(2, "0");
}

/** Local `YYYY-MM-DD` for a Date. */
export function localDayKey(d: Date): string {
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/** A `YYYY-MM-DD` key as a local-midnight Date (never UTC-parsed). */
function dateFromKey(key: string): Date {
  const [y, m, d] = key.split("-").map(Number);
  return new Date(y, m - 1, d);
}

/** The local day an event belongs under. A multi-day all-day event that began
 * before today is listed under today, since that's when it's relevant. */
export function eventDayKey(ev: FormattableEvent, todayKey: string): string {
  const key = ev.allDay ? ev.start : localDayKey(new Date(ev.start));
  return key < todayKey ? todayKey : key;
}

export function dayLabel(dayKey: string, now: Date): string {
  const todayKey = localDayKey(now);
  if (dayKey === todayKey) return "Today";
  const tomorrow = new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1);
  if (dayKey === localDayKey(tomorrow)) return "Tomorrow";
  return dateFromKey(dayKey).toLocaleDateString(undefined, {
    weekday: "short",
    month: "short",
    day: "numeric",
  });
}

/** Groups already-sorted events by local day, preserving their order. */
export function groupEventsByDay<T extends FormattableEvent>(events: T[], now: Date): EventDayGroup<T>[] {
  const todayKey = localDayKey(now);
  const groups: EventDayGroup<T>[] = [];
  for (const ev of events) {
    const key = eventDayKey(ev, todayKey);
    const last = groups[groups.length - 1];
    if (last && last.dayKey === key) {
      last.events.push(ev);
    } else {
      groups.push({ dayKey: key, label: dayLabel(key, now), events: [ev] });
    }
  }
  return groups;
}

function timeOfDay(d: Date): string {
  return d.toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
}

/** "All day", "All day · until Fri, Oct 10", "9:00 AM", or "9:00 AM – 10:00 AM". */
export function timeLabel(ev: FormattableEvent, now: Date): string {
  if (ev.allDay) {
    return ev.end > ev.start ? `All day · until ${dayLabel(ev.end, now)}` : "All day";
  }
  const start = new Date(ev.start);
  const end = new Date(ev.end);
  if (end.getTime() > start.getTime() && localDayKey(end) === localDayKey(start)) {
    return `${timeOfDay(start)} – ${timeOfDay(end)}`;
  }
  return timeOfDay(start);
}
