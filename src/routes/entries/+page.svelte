<script lang="ts">
  import { onMount } from "svelte";
  import {
    getReflectionsForDate,
    getTaskList,
    getWellnessSummaryForDate,
    localDateStamp,
    type ReflectionEntry,
    type WellnessSummary,
  } from "$lib/db";

  const EMPTY_WELLNESS_SUMMARY: WellnessSummary = {
    total: 0,
    relaxedEyes: 0,
    exercise: 0,
    drankWater: 0,
    washroom: 0,
  };

  let selected = $state(new Date());
  let reflections = $state<ReflectionEntry[]>([]);
  let taskList = $state("");
  let wellnessSummary = $state<WellnessSummary>(EMPTY_WELLNESS_SUMMARY);
  let calendarMonth = $state(new Date());
  let loading = $state(false);

  const selectedStamp = $derived(localDateStamp(selected));
  const isToday = $derived(selectedStamp === localDateStamp(new Date()));

  async function load() {
    loading = true;
    const stamp = selectedStamp;
    const [r, t, w] = await Promise.all([
      getReflectionsForDate(stamp),
      getTaskList(stamp),
      getWellnessSummaryForDate(stamp),
    ]);
    reflections = r;
    taskList = t;
    wellnessSummary = w;
    loading = false;
  }

  function goToDay(delta: number) {
    const d = new Date(selected);
    d.setDate(d.getDate() + delta);
    selected = d;
  }

  function pickDate(d: Date) {
    selected = d;
  }

  function shiftMonth(delta: number) {
    const d = new Date(calendarMonth);
    d.setMonth(d.getMonth() + delta);
    calendarMonth = d;
  }

  function formatTime(iso: string): string {
    return new Date(iso).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  }

  const calendarDays = $derived.by(() => {
    const year = calendarMonth.getFullYear();
    const month = calendarMonth.getMonth();
    const firstOfMonth = new Date(year, month, 1);
    const startOffset = firstOfMonth.getDay();
    const daysInMonth = new Date(year, month + 1, 0).getDate();
    const days: (Date | null)[] = [];
    for (let i = 0; i < startOffset; i++) days.push(null);
    for (let d = 1; d <= daysInMonth; d++) days.push(new Date(year, month, d));
    return days;
  });

  $effect(() => {
    void selectedStamp;
    void load();
  });
</script>

<div class="page">
  <section class="card calendar">
    <div class="cal-header">
      <button onclick={() => shiftMonth(-1)} aria-label="Previous month">&larr;</button>
      <span>{calendarMonth.toLocaleDateString(undefined, { month: "long", year: "numeric" })}</span>
      <button onclick={() => shiftMonth(1)} aria-label="Next month">&rarr;</button>
    </div>
    <div class="cal-grid dow">
      {#each ["S", "M", "T", "W", "T", "F", "S"] as d}
        <span>{d}</span>
      {/each}
    </div>
    <div class="cal-grid">
      {#each calendarDays as day}
        {#if day}
          <button
            class="day"
            class:selected={localDateStamp(day) === selectedStamp}
            class:today={localDateStamp(day) === localDateStamp(new Date())}
            onclick={() => pickDate(day)}
          >
            {day.getDate()}
          </button>
        {:else}
          <span></span>
        {/if}
      {/each}
    </div>
  </section>

  <section class="card entries">
    <div class="day-nav">
      <button onclick={() => goToDay(-1)}>&larr; Prev day</button>
      <h2>{selected.toLocaleDateString(undefined, { weekday: "long", month: "long", day: "numeric" })}{isToday ? " (today)" : ""}</h2>
      <button onclick={() => goToDay(1)}>Next day &rarr;</button>
    </div>

    {#if loading}
      <p class="hint">Loading...</p>
    {:else}
      {#if wellnessSummary.total > 0}
        <div class="wellness-summary">
          <div class="stat">
            <span class="stat-value">{wellnessSummary.relaxedEyes}/{wellnessSummary.total}</span>
            <span class="stat-label">Relaxed eyes</span>
          </div>
          <div class="stat">
            <span class="stat-value">{wellnessSummary.exercise}/{wellnessSummary.total}</span>
            <span class="stat-label">Exercise</span>
          </div>
          <div class="stat">
            <span class="stat-value">{wellnessSummary.drankWater}/{wellnessSummary.total}</span>
            <span class="stat-label">Drank water</span>
          </div>
          <div class="stat">
            <span class="stat-value">{wellnessSummary.washroom}/{wellnessSummary.total}</span>
            <span class="stat-label">Washroom</span>
          </div>
        </div>
      {/if}

      {#if taskList.trim()}
        <div class="task-list">
          <h3>Most Important Tasks</h3>
          <pre>{taskList}</pre>
        </div>
      {/if}

      {#if reflections.length === 0}
        <p class="hint">No reflections logged for this day.</p>
      {:else}
        <ul class="reflection-list">
          {#each reflections as row (row.created_at)}
            <li>
              <div class="meta">
                <span class="time">{formatTime(row.created_at)}</span>
                {#if row.slot_count > 1}
                  <span class="badge">covers {row.slot_count} pomodoros</span>
                {/if}
              </div>
              <p>{row.text}</p>
            </li>
          {/each}
        </ul>
      {/if}
    {/if}
  </section>
</div>

<style>
  .page {
    padding: 24px;
    display: grid;
    grid-template-columns: 300px 1fr;
    gap: 20px;
    max-width: 1000px;
    margin: 0 auto;
  }

  .card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 14px;
    padding: 20px;
  }

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

  .day.today {
    font-weight: 700;
  }

  .day.selected {
    background: var(--accent);
    color: white;
  }

  .day-nav {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 16px;
  }

  .day-nav h2 {
    font-size: 15px;
    margin: 0;
  }

  .day-nav button {
    background: var(--surface-2);
    border: none;
    color: inherit;
    padding: 6px 12px;
    border-radius: 8px;
    font-size: 13px;
  }

  .wellness-summary {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 10px;
    margin-bottom: 20px;
  }

  .stat {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 4px;
    background: var(--surface-2);
    border-radius: 10px;
    padding: 10px 8px;
    text-align: center;
  }

  .stat-value {
    font-size: 18px;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }

  .stat-label {
    font-size: 11px;
    color: var(--text-dim);
  }

  .task-list {
    margin-bottom: 20px;
    padding-bottom: 16px;
    border-bottom: 1px solid var(--border);
  }

  .task-list h3 {
    margin: 0 0 8px;
    font-size: 13px;
    color: var(--text-dim);
  }

  .task-list pre {
    margin: 0;
    font-family: inherit;
    white-space: pre-wrap;
    font-size: 14px;
  }

  .reflection-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }

  .reflection-list li {
    background: var(--surface-2);
    border-radius: 10px;
    padding: 12px 14px;
  }

  .meta {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 6px;
  }

  .time {
    font-size: 12px;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
  }

  .badge {
    font-size: 11px;
    background: var(--accent-soft);
    color: var(--accent);
    padding: 2px 8px;
    border-radius: 999px;
  }

  .reflection-list p {
    margin: 0;
    font-size: 14px;
    white-space: pre-wrap;
  }

  .hint {
    color: var(--text-dim);
    font-size: 13px;
  }
</style>
