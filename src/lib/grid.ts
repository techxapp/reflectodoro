// Mirrors src-tauri/src/grid.rs — display-only on the frontend (informational
// "time until next break" countdown). The Rust backend is the sole source of
// truth for when the overlay actually opens/closes.

export type Phase = "work" | "break";

export interface Slot {
  phase: Phase;
  start: Date;
  end: Date;
}

function atMinute(d: Date, minute: number): Date {
  const copy = new Date(d);
  copy.setMinutes(minute, 0, 0);
  return copy;
}

export function slotFor(now: Date): Slot {
  const m = now.getMinutes();
  let phase: Phase;
  let startMin: number;
  let endMin: number;

  if (m < 25) {
    phase = "work";
    startMin = 0;
    endMin = 25;
  } else if (m < 30) {
    phase = "break";
    startMin = 25;
    endMin = 30;
  } else if (m < 55) {
    phase = "work";
    startMin = 30;
    endMin = 55;
  } else {
    phase = "break";
    startMin = 55;
    endMin = 60;
  }

  const start = atMinute(now, startMin);
  const end = endMin === 60 ? new Date(atMinute(now, 0).getTime() + 60 * 60 * 1000) : atMinute(now, endMin);

  return { phase, start, end };
}
