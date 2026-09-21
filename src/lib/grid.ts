// Mirrors src-tauri/src/grid.rs — display-only on the frontend (informational
// "time until next break" countdown). The Rust backend is the sole source of
// truth for when the overlay actually opens/closes.

export type Phase = "work" | "break";
export type Mode = "normal" | "concentration";

export interface Slot {
  phase: Phase;
  start: Date;
  end: Date;
}

function atMinute(d: Date, minute: number, second = 0): Date {
  const copy = new Date(d);
  copy.setMinutes(minute, second, 0);
  return copy;
}

export function slotFor(now: Date, mode: Mode = "normal"): Slot {
  let phase: Phase;
  let start: [number, number];
  let end: [number, number]; // minute 60 = top of next hour

  if (mode === "concentration") {
    // Work :00-:25, break :25:00-:25:30, work :25:30-:50, break :50-:00.
    const s = now.getMinutes() * 60 + now.getSeconds();
    if (s < 25 * 60) {
      [phase, start, end] = ["work", [0, 0], [25, 0]];
    } else if (s < 25 * 60 + 30) {
      [phase, start, end] = ["break", [25, 0], [25, 30]];
    } else if (s < 50 * 60) {
      [phase, start, end] = ["work", [25, 30], [50, 0]];
    } else {
      [phase, start, end] = ["break", [50, 0], [60, 0]];
    }
  } else {
    const m = now.getMinutes();
    if (m < 25) {
      [phase, start, end] = ["work", [0, 0], [25, 0]];
    } else if (m < 30) {
      [phase, start, end] = ["break", [25, 0], [30, 0]];
    } else if (m < 55) {
      [phase, start, end] = ["work", [30, 0], [55, 0]];
    } else {
      [phase, start, end] = ["break", [55, 0], [60, 0]];
    }
  }

  const startDate = atMinute(now, start[0], start[1]);
  const endDate =
    end[0] === 60 ? new Date(atMinute(now, 0).getTime() + 60 * 60 * 1000) : atMinute(now, end[0], end[1]);

  return { phase, start: startDate, end: endDate };
}
