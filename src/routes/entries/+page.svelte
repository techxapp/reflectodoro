<script lang="ts">
  import { onMount } from "svelte";
  import {
    clusterReflectionRows,
    getReflectionsForDate,
    getTaskList,
    getNotToDoList,
    getWellnessSummaryForDate,
    localDateStamp,
    saveTaskList,
    saveNotToDoList,
    updateReflectionText,
    type ReflectionRow,
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
  let reflectionRows = $state<ReflectionRow[]>([]);
  let taskList = $state("");
  let notToDo = $state("");
  let wellnessSummary = $state<WellnessSummary>(EMPTY_WELLNESS_SUMMARY);
  let calendarMonth = $state(new Date());
  let loading = $state(false);
  let expandedClusters = $state<Set<number>>(new Set());
  let editingId = $state<number | null>(null);
  let editText = $state("");
  let taskSaveTimer: ReturnType<typeof setTimeout> | undefined;
  let notToDoSaveTimer: ReturnType<typeof setTimeout> | undefined;

  const selectedStamp = $derived(localDateStamp(selected));
  const isToday = $derived(selectedStamp === localDateStamp(new Date()));
  const clusters = $derived(clusterReflectionRows(reflectionRows));

  async function load() {
    loading = true;
    const stamp = selectedStamp;
    const [r, t, n, w] = await Promise.all([
      getReflectionsForDate(stamp),
      getTaskList(stamp),
      getNotToDoList(stamp),
      getWellnessSummaryForDate(stamp),
    ]);
    reflectionRows = r;
    taskList = t;
    notToDo = n;
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

  /** Captures the date and content at call time (not read fresh when the
   * timer fires) so a debounced save always lands on the day it was typed
   * on, even if the user has since navigated to a different day. */
  function scheduleTaskSave(content: string) {
    const stamp = selectedStamp;
    if (taskSaveTimer) clearTimeout(taskSaveTimer);
    taskSaveTimer = setTimeout(() => {
      void saveTaskList(stamp, content);
    }, 800);
  }

  /** Same "capture the date at call time" pattern as scheduleTaskSave above. */
  function scheduleNotToDoSave(content: string) {
    const stamp = selectedStamp;
    if (notToDoSaveTimer) clearTimeout(notToDoSaveTimer);
    notToDoSaveTimer = setTimeout(() => {
      void saveNotToDoList(stamp, content);
    }, 800);
  }

  function toggleExpanded(clusterKey: number) {
    const next = new Set(expandedClusters);
    if (next.has(clusterKey)) next.delete(clusterKey);
    else next.add(clusterKey);
    expandedClusters = next;
  }

  function startEdit(row: { id: number; text: string }) {
    editingId = row.id;
    editText = row.text;
  }

  function cancelEdit() {
    editingId = null;
    editText = "";
  }

  async function saveEdit(row: { id: number }) {
    const text = editText.trim();
    if (!text) return;
    await updateReflectionText(row.id, text);
    reflectionRows = reflectionRows.map((r) => (r.id === row.id ? { ...r, text } : r));
    editingId = null;
    editText = "";
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

      <div class="task-list">
        <h3>Most Important Tasks</h3>
        <textarea
          bind:value={taskList}
          oninput={() => scheduleTaskSave(taskList)}
          placeholder="1.
2.
3."
          rows="5"
        ></textarea>
      </div>

      <div class="task-list">
        <h3>Not To Do Tasks</h3>
        <textarea
          bind:value={notToDo}
          oninput={() => scheduleNotToDoSave(notToDo)}
          placeholder="1.
2.
3."
          rows="3"
        ></textarea>
      </div>

      {#snippet editButton(row: { id: number; text: string })}
        {#if editingId !== row.id}
          <button class="icon-btn" onclick={() => startEdit(row)} aria-label="Edit reflection" title="Edit">
            <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
              <path d="M17 3a2.828 2.828 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5L17 3z" />
            </svg>
          </button>
        {/if}
      {/snippet}

      {#snippet reflectionRow(row: { id: number; text: string })}
        {#if editingId === row.id}
          <div class="edit-row">
            <textarea bind:value={editText} rows="3"></textarea>
            <div class="edit-actions">
              <button class="save" disabled={!editText.trim()} onclick={() => saveEdit(row)}>Save</button>
              <button class="cancel" onclick={cancelEdit}>Cancel</button>
            </div>
          </div>
        {:else}
          <p>{row.text}</p>
        {/if}
      {/snippet}

      {#if clusters.length === 0}
        <p class="hint">No reflections logged for this day.</p>
      {:else}
        <ul class="reflection-list">
          {#each clusters as cluster (cluster.rows[0].id)}
            {@const clusterKey = cluster.rows[0].id}
            <li>
              <div class="meta">
                <span class="time">{formatTime(cluster.rows[0].created_at)}</span>
                {#if cluster.rows.length === 1}
                  {@render editButton(cluster.rows[0])}
                {:else}
                  <span class="badge">covers {cluster.rows.length} pomodoros</span>
                  <button class="chevron" onclick={() => toggleExpanded(clusterKey)}>
                    {expandedClusters.has(clusterKey) ? "Collapse" : "Expand"}
                  </button>
                {/if}
              </div>

              {#if cluster.rows.length === 1}
                {@render reflectionRow(cluster.rows[0])}
              {:else}
                <p>{cluster.rows[0].text}</p>
                {#if expandedClusters.has(clusterKey)}
                  <ul class="slot-list">
                    {#each cluster.rows as row (row.id)}
                      <li>
                        <div class="slot-meta">
                          <span class="slot-time">{formatTime(row.slot_start_at)}</span>
                          {@render editButton(row)}
                        </div>
                        {@render reflectionRow(row)}
                      </li>
                    {/each}
                  </ul>
                {/if}
              {/if}
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

  @media (max-width: 600px) {
    .page {
      grid-template-columns: 1fr;
      padding: 16px;
      gap: 16px;
    }
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

  @media (max-width: 600px) {
    .day {
      min-height: 40px;
    }
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
    flex-wrap: wrap;
    row-gap: 8px;
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

  @media (max-width: 600px) {
    /* Date heading gets its own full-width row above the prev/next
       buttons instead of squeezing between them -- a long localized date
       string ("Saturday, August 29") doesn't leave much room otherwise. */
    .day-nav h2 {
      order: -1;
      width: 100%;
      text-align: center;
    }

    .day-nav button {
      padding: 8px 14px;
    }
  }

  .wellness-summary {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 10px;
    margin-bottom: 20px;
  }

  @media (max-width: 600px) {
    .wellness-summary {
      grid-template-columns: repeat(2, 1fr);
    }
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

  .task-list textarea {
    width: 100%;
    box-sizing: border-box;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 10px;
    color: inherit;
    padding: 10px 12px;
    font-size: 14px;
    font-family: inherit;
    resize: vertical;
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

  .chevron {
    background: none;
    border: none;
    color: var(--text-dim);
    font-size: 11px;
    padding: 2px 6px;
    border-radius: 6px;
    margin-left: auto;
  }

  .chevron:hover {
    background: var(--surface);
    color: inherit;
  }

  .reflection-list p {
    margin: 0;
    font-size: 14px;
    white-space: pre-wrap;
  }

  .icon-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: none;
    border: none;
    color: var(--text-dim);
    padding: 4px;
    border-radius: 6px;
    margin-left: auto;
  }

  .icon-btn:hover {
    background: var(--surface);
    color: var(--accent);
  }

  .edit-row textarea {
    width: 100%;
    box-sizing: border-box;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 10px;
    color: inherit;
    padding: 10px 12px;
    font-size: 14px;
    resize: vertical;
  }

  .edit-actions {
    display: flex;
    gap: 8px;
    margin-top: 8px;
  }

  .edit-actions .save {
    background: var(--accent);
    color: white;
    border: none;
    padding: 6px 12px;
    border-radius: 8px;
    font-size: 13px;
  }

  .edit-actions .save:disabled {
    opacity: 0.5;
  }

  .edit-actions .cancel {
    background: none;
    border: 1px solid var(--border);
    color: inherit;
    padding: 6px 12px;
    border-radius: 8px;
    font-size: 13px;
  }

  .slot-list {
    list-style: none;
    margin: 10px 0 0;
    padding: 10px 0 0;
    border-top: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .slot-list li {
    padding-left: 12px;
    border-left: 2px solid var(--border);
  }

  .slot-meta {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 4px;
  }

  .slot-time {
    font-size: 11px;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
  }

  .hint {
    color: var(--text-dim);
    font-size: 13px;
  }
</style>
