<script lang="ts">
  // Month grid, lifted from the Entries page's inline calendar (same layout
  // and sizes) so the Habits tab can reuse it with per-day decorations.
  // Entries still carries its own copy; switching it over is a separate
  // change.
  import type { Snippet } from "svelte";

  let {
    month = $bindable(),
    selectedStamp = null,
    todayStamp,
    onPick,
    dayStyle,
    decoration,
  }: {
    /** Any date inside the month to show. */
    month: Date;
    selectedStamp?: string | null;
    todayStamp: string;
    onPick: (stamp: string) => void;
    /** Inline style for a day's button, e.g. a highlight colour. */
    dayStyle?: (stamp: string) => string;
    /** Rendered under the day number, e.g. coloured dots. */
    decoration?: Snippet<[string]>;
  } = $props();

  function stampOf(d: Date): string {
    return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
  }

  function shiftMonth(delta: number) {
    month = new Date(month.getFullYear(), month.getMonth() + delta, 1);
  }

  const days = $derived.by(() => {
    const year = month.getFullYear();
    const m = month.getMonth();
    const startOffset = new Date(year, m, 1).getDay();
    const daysInMonth = new Date(year, m + 1, 0).getDate();
    const out: ({ date: Date; stamp: string } | null)[] = [];
    for (let i = 0; i < startOffset; i++) out.push(null);
    for (let d = 1; d <= daysInMonth; d++) {
      const date = new Date(year, m, d);
      out.push({ date, stamp: stampOf(date) });
    }
    return out;
  });
</script>

<div class="cal-header">
  <button type="button" onclick={() => shiftMonth(-1)} aria-label="Previous month">&larr;</button>
  <span>{month.toLocaleDateString(undefined, { month: "long", year: "numeric" })}</span>
  <button type="button" onclick={() => shiftMonth(1)} aria-label="Next month">&rarr;</button>
</div>
<div class="cal-grid dow">
  {#each ["S", "M", "T", "W", "T", "F", "S"] as d}
    <span>{d}</span>
  {/each}
</div>
<div class="cal-grid">
  {#each days as day}
    {#if day}
      <button
        type="button"
        class="day"
        class:selected={day.stamp === selectedStamp}
        class:today={day.stamp === todayStamp}
        style={dayStyle?.(day.stamp) ?? ""}
        aria-pressed={day.stamp === selectedStamp}
        aria-label={day.date.toLocaleDateString(undefined, { weekday: "long", month: "long", day: "numeric" })}
        onclick={() => onPick(day.stamp)}
      >
        <span class="num">{day.date.getDate()}</span>
        {#if decoration}{@render decoration(day.stamp)}{/if}
      </button>
    {:else}
      <span></span>
    {/if}
  {/each}
</div>

<style>
  .cal-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 12px;
    font-size: 14px;
    font-weight: 500;
  }

  .cal-header button {
    background: none;
    border: none;
    color: inherit;
    font-size: 14px;
    padding: 4px 8px;
    border-radius: 6px;
  }

  .cal-header button:hover {
    background: var(--surface-2);
  }

  .cal-grid {
    display: grid;
    grid-template-columns: repeat(7, 1fr);
    gap: 4px;
    text-align: center;
  }

  .dow {
    color: var(--text-dim);
    font-size: 11px;
    margin-bottom: 4px;
  }

  .day {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
    min-height: 40px;
    background: none;
    border: none;
    color: inherit;
    padding: 6px 0;
    border-radius: 8px;
    font-size: 13px;
  }

  .day:hover {
    background: var(--surface-2);
  }

  .day.today .num {
    font-weight: 700;
  }

  /* An outline rather than Entries' filled accent background, so a day's
     own highlight colour stays visible while it's selected. */
  .day.selected {
    box-shadow: inset 0 0 0 2px var(--accent);
  }
</style>
