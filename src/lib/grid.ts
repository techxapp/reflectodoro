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
    // Work :00-:25, break :25:00-:26:00, work :26:00-:50, break :50-:00.
    const s = now.getMinutes() * 60 + now.getSeconds();
    if (s < 25 * 60) {
      [phase, start, end] = ["work", [0, 0], [25, 0]];
    } else if (s < 26 * 60) {
      [phase, start, end] = ["break", [25, 0], [26, 0]];
    } else if (s < 50 * 60) {
      [phase, start, end] = ["work", [26, 0], [50, 0]];
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

/** A break shorter than this gets no end-of-break quote at all -- the panel
 * stays hidden *and* no request is made to the user-configured quote API.
 * Concentration mode's 1-minute `:25` break is the case this exists for: too
 * short to read a quote in, and fetching one anyway would spend a request
 * (often against a rate-limited free API) on something nobody sees.
 * Mirrors `grid::MIN_QUOTE_BREAK_MINUTES` (src-tauri/src/grid.rs). */
export const MIN_QUOTE_BREAK_MINUTES = 2;

/** Whether a break running `breakStartIso`..`breakEndIso` is long enough to
 * show a quote (see MIN_QUOTE_BREAK_MINUTES). Mirrors Rust's
 * `grid::break_qualifies_for_quote`, down to returning `true` when either
 * timestamp is missing/unparseable -- the quote panel is a nicety, and the
 * pre-existing behavior (always fetch) is the safer fallback. */
export function breakQualifiesForQuote(breakStartIso: string, breakEndIso: string): boolean {
  const start = Date.parse(breakStartIso);
  const end = Date.parse(breakEndIso);
  if (Number.isNaN(start) || Number.isNaN(end)) return true;
  return end - start >= MIN_QUOTE_BREAK_MINUTES * 60_000;
}
