<script lang="ts">
  // Habits tab: track recurring personal events (periods, hair wash, ...)
  // with per-habit history views. See CLAUDE.md's "Habit tracker". All rows
  // are loaded once and decrypted in one batch (db.ts's getHabits/
  // getHabitLogs); every view below is derived client-side from them.
  import { onDestroy, onMount } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { ask } from "@tauri-apps/plugin-dialog";
  import { error as writeLog } from "@tauri-apps/plugin-log";
  import {
    KEY_STATE_EVENT,
    addHabitLog,
    createHabit,
    deleteHabit,
    deleteHabitLog,
    getHabitLogs,
    getHabits,
    localDateStamp,
    saveHabitOrder,
    setHabitArchived,
    updateHabit,
    updateHabitLog,
  } from "$lib/db";
  import {
    HABIT_COLORS,
    HABIT_EMOJI_MAX,
    HABIT_NAME_MAX,
    HABIT_NOTE_MAX,
    daysBetween,
    formatDaysSince,
    habitStats,
    logsByDate,
    timeHHMM,
    type Habit,
    type HabitInput,
    type HabitLog,
    type HabitStats,
  } from "$lib/habits";
  import MonthCalendar from "$lib/MonthCalendar.svelte";

  type View = "calendar" | "timeline";
  const ALL = "all";
  const VIEW_KEY = "habits.view";
  const SELECTED_KEY = "habits.selected";
  const TIMELINE_PAGE = 100;

  let habits = $state<Habit[]>([]);
  let logs = $state<HabitLog[]>([]);
  let loading = $state(true);
  let loadError = $state<string | null>(null);
  let loadGeneration = 0;

  let todayStamp = $state(localDateStamp());
  let selectedId = $state<string>(readStored(SELECTED_KEY) ?? ALL);
  let showHidden = $state(false);
  let view = $state<View>(readStored(VIEW_KEY) === "timeline" ? "timeline" : "calendar");
  let calendarMonth = $state(new Date());
  let selectedDay = $state<string>(localDateStamp());
  let timelineLimit = $state(TIMELINE_PAGE);

  let busy = $state(false);
  let habitActionError = $state<string | null>(null);
  let logActionError = $state<string | null>(null);

  // Add / edit habit forms.
  let adding = $state(false);
  let newHabit = $state<HabitInput>(blankHabitInput());
  let editingHabitId = $state<string | null>(null);
  let editHabit = $state<HabitInput>(blankHabitInput());

  // Log form.
  let logHabitId = $state<string>("");
  let logDate = $state(localDateStamp());
  let logTime = $state(timeHHMM());
  let logNote = $state("");
  let loggedFlash = $state(false);
  let flashTimer: ReturnType<typeof setTimeout> | undefined;

  // Inline log edit.
  let editingLogId = $state<string | null>(null);
  let editLogDate = $state("");
  let editLogTime = $state("");
  let editLogNote = $state("");

  function blankHabitInput(): HabitInput {
    return { name: "", emoji: "", color: HABIT_COLORS[0], targetDays: "" };
  }

  // Per-viewer conveniences only (which habit/view was last open) -- never
  // data. Wrapped because storage can be unavailable or throw.
  function readStored(key: string): string | null {
    try {
      return localStorage.getItem(key);
    } catch {
      return null;
    }
  }
  function writeStored(key: string, value: string) {
    try {
      localStorage.setItem(key, value);
    } catch {
      // Not worth surfacing: it only means the choice isn't remembered.
    }
  }

  function message(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  // --- Derived views -----------------------------------------------------

  const habitById = $derived(new Map(habits.map((h) => [h.id, h])));
  const visibleHabits = $derived(showHidden ? habits : habits.filter((h) => !h.archived));
  const selectedHabit = $derived(selectedId === ALL ? null : (habitById.get(selectedId) ?? null));

  const logsByHabit = $derived.by(() => {
    const map = new Map<string, HabitLog[]>();
    for (const log of logs) {
      const list = map.get(log.habitId);
      if (list) list.push(log);
      else map.set(log.habitId, [log]);
    }
    return map;
  });

  const statsById = $derived.by(() => {
    const map = new Map<string, HabitStats>();
    for (const h of habits) map.set(h.id, habitStats(logsByHabit.get(h.id) ?? [], h.targetDays, todayStamp));
    return map;
  });

  /** Logs in the current scope: one habit, or every visible habit. */
  const scopedLogs = $derived.by(() => {
    if (selectedHabit) return logsByHabit.get(selectedHabit.id) ?? [];
    const visible = new Set(visibleHabits.map((h) => h.id));
    return logs.filter((l) => visible.has(l.habitId));
  });
  const scopedByDate = $derived(logsByDate(scopedLogs));
  const selectedDayLogs = $derived(scopedByDate.get(selectedDay) ?? []);
  const selectedStats = $derived(selectedHabit ? statsById.get(selectedHabit.id) : undefined);

  /** Timeline items newest first, grouped by month. The oldest entry of each
   * date carries the gap (in days) back to the previous logged date, so a
   * day with several entries shows it once. Single-habit view only, since a
   * gap between two different habits means nothing. */
  const timelineGroups = $derived.by(() => {
    const shown = scopedLogs.slice(0, timelineLimit);
    const groups: { key: string; label: string; items: { log: HabitLog; gap: number | null }[] }[] = [];
    for (let i = 0; i < shown.length; i++) {
      const log = shown[i];
      let gap: number | null = null;
      const older = scopedLogs[i + 1];
      if (selectedHabit && older && older.date !== log.date) gap = daysBetween(older.date, log.date);
      const key = log.date.slice(0, 7);
      let group = groups[groups.length - 1];
      if (!group || group.key !== key) {
        const [y, m] = key.split("-").map(Number);
        group = {
          key,
          label: new Date(y, m - 1, 1).toLocaleDateString(undefined, { month: "long", year: "numeric" }),
          items: [],
        };
        groups.push(group);
      }
      group.items.push({ log, gap });
    }
    return groups;
  });

  // Keep the log form's habit valid as the list changes.
  $effect(() => {
    if (selectedHabit) {
      logHabitId = selectedHabit.id;
    } else if (!visibleHabits.some((h) => h.id === logHabitId)) {
      logHabitId = visibleHabits[0]?.id ?? "";
    }
  });

  // --- Loading -------------------------------------------------------------

  async function load() {
    const generation = ++loadGeneration;
    try {
      const [h, l] = await Promise.all([getHabits(), getHabitLogs()]);
      if (generation !== loadGeneration) return;
      habits = h;
      logs = l;
      loadError = null;
      if (selectedId !== ALL && !h.some((x) => x.id === selectedId)) selectHabit(ALL);
    } catch (e) {
      if (generation !== loadGeneration) return;
      loadError = message(e);
      void writeLog(`habits: load failed: ${loadError}`);
    } finally {
      if (generation === loadGeneration) loading = false;
    }
    // A window left open across midnight should roll "today" over too.
    todayStamp = localDateStamp();
  }

  function onWindowFocus() {
    void load();
  }

  let unlistenKeyState: UnlistenFn | undefined;
  let unlistenAutoSync: UnlistenFn | undefined;
  let destroyed = false;

  onMount(async () => {
    window.addEventListener("focus", onWindowFocus);
    void load();
    // Reload after an unlock (a locked key fails every decrypt) and after an
    // automatic sync lands new rows while this tab is open.
    const keyState = await listen(KEY_STATE_EVENT, () => void load());
    const autoSync = await listen("p2p-sync://auto-completed", () => void load());
    if (destroyed) {
      keyState();
      autoSync();
    } else {
      unlistenKeyState = keyState;
      unlistenAutoSync = autoSync;
    }
  });

  onDestroy(() => {
    destroyed = true;
    window.removeEventListener("focus", onWindowFocus);
    unlistenKeyState?.();
    unlistenAutoSync?.();
    clearTimeout(flashTimer);
  });

  // --- Actions ---------------------------------------------------------------

  /** Runs one write, then reloads. Errors are shown next to where the action
   * happened and logged (never the user's content, only the message). */
  async function run(where: "habit" | "log", fn: () => Promise<void>): Promise<boolean> {
    if (busy) return false;
    busy = true;
    if (where === "habit") habitActionError = null;
    else logActionError = null;
    try {
      await fn();
      await load();
      return true;
    } catch (e) {
      const msg = message(e);
      if (where === "habit") habitActionError = msg;
      else logActionError = msg;
      void writeLog(`habits: ${where} action failed: ${msg}`);
      return false;
    } finally {
      busy = false;
    }
  }

  function selectHabit(id: string) {
    selectedId = id;
    writeStored(SELECTED_KEY, id);
    timelineLimit = TIMELINE_PAGE;
    editingLogId = null;
  }

  function setView(next: View) {
    view = next;
    writeStored(VIEW_KEY, next);
  }

  function pickDay(stamp: string) {
    selectedDay = stamp;
    // Clicking a past day also targets the log form at it -- the quickest
    // way to backdate an entry.
    if (stamp <= todayStamp) logDate = stamp;
  }

  async function submitNewHabit() {
    const ok = await run("habit", () => createHabit(newHabit));
    if (ok) {
      newHabit = blankHabitInput();
      adding = false;
    }
  }

  function startEditHabit(h: Habit) {
    editingHabitId = h.id;
    editHabit = { name: h.name, emoji: h.emoji, color: h.color, targetDays: h.targetDays === null ? "" : String(h.targetDays) };
    habitActionError = null;
  }

  async function submitEditHabit() {
    const id = editingHabitId;
    if (!id) return;
    if (await run("habit", () => updateHabit(id, editHabit))) editingHabitId = null;
  }

  async function reorder(id: string, direction: -1 | 1) {
    const i = visibleHabits.findIndex((h) => h.id === id);
    const j = i + direction;
    if (i < 0 || j < 0 || j >= visibleHabits.length) return;
    // Swap with the neighbour *as displayed*, so a hidden habit in between
    // never makes a click look like it did nothing.
    const full = [...habits];
    const a = full.findIndex((h) => h.id === visibleHabits[i].id);
    const b = full.findIndex((h) => h.id === visibleHabits[j].id);
    [full[a], full[b]] = [full[b], full[a]];
    await run("habit", () => saveHabitOrder(full));
  }

  async function toggleHidden(h: Habit) {
    await run("habit", () => setHabitArchived(h.id, !h.archived));
  }

  async function removeHabit(h: Habit) {
    const count = logsByHabit.get(h.id)?.length ?? 0;
    const ok = await ask(
      `Delete "${h.name}" and its ${count} log${count === 1 ? "" : "s"}?\n\n` +
        `This can't be undone, and paired devices delete it too the next time they sync. ` +
        `To keep the history out of sight instead, use Hide.`,
      { title: "Delete habit", kind: "warning", okLabel: "Delete", cancelLabel: "Cancel" },
    );
    if (!ok) return;
    if (await run("habit", () => deleteHabit(h.id))) editingHabitId = null;
  }

  async function quickLog(h: Habit) {
    await run("habit", () => addHabitLog(h.id, localDateStamp(), timeHHMM(), ""));
  }

  async function submitLog() {
    const habitId = logHabitId;
    if (!habitId) return;
    const ok = await run("log", () => addHabitLog(habitId, logDate, logTime, logNote));
    if (ok) {
      logNote = "";
      logDate = todayStamp;
      logTime = timeHHMM();
      loggedFlash = true;
      clearTimeout(flashTimer);
      flashTimer = setTimeout(() => (loggedFlash = false), 2000);
    }
  }

  function startEditLog(log: HabitLog) {
    editingLogId = log.id;
    editLogDate = log.date;
    editLogTime = log.time;
    editLogNote = log.note;
    logActionError = null;
  }

  async function submitEditLog() {
    const id = editingLogId;
    if (!id) return;
    if (await run("log", () => updateHabitLog(id, editLogDate, editLogTime, editLogNote))) editingLogId = null;
  }

  async function removeLog(log: HabitLog) {
    const ok = await ask(`Delete the entry from ${formatDate(log.date)}?`, {
      title: "Delete entry",
      kind: "warning",
      okLabel: "Delete",
      cancelLabel: "Cancel",
    });
    if (!ok) return;
    if (await run("log", () => deleteHabitLog(log.id)) && editingLogId === log.id) editingLogId = null;
  }

  // --- Formatting ----------------------------------------------------------

  function formatDate(stamp: string): string {
    const [y, m, d] = stamp.split("-").map(Number);
    return new Date(y, m - 1, d).toLocaleDateString(undefined, {
      weekday: "short",
      month: "short",
      day: "numeric",
      year: stamp.slice(0, 4) === todayStamp.slice(0, 4) ? undefined : "numeric",
    });
  }

  function formatTime(hhmm: string): string {
    if (!hhmm) return "";
    const [h, m] = hhmm.split(":").map(Number);
    return new Date(2000, 0, 1, h, m).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
  }

  function colorVar(h: Habit | undefined): string {
    return `var(--habit-${h?.color ?? "slate"})`;
  }

  function habitLabel(h: Habit): string {
    return h.emoji ? `${h.emoji} ${h.name}` : h.name;
  }

  function dayStyle(stamp: string): string {
    if (!selectedHabit || !scopedByDate.has(stamp)) return "";
    const c = colorVar(selectedHabit);
    return `background: color-mix(in srgb, ${c} 30%, transparent); color: inherit;`;
  }

  /** Distinct habits logged on a day, for the all-habits calendar's dots. */
  function habitsOnDay(stamp: string): Habit[] {
    const seen = new Set<string>();
    const out: Habit[] = [];
    for (const log of scopedByDate.get(stamp) ?? []) {
      if (seen.has(log.habitId)) continue;
      seen.add(log.habitId);
      const h = habitById.get(log.habitId);
      if (h) out.push(h);
    }
    return out;
  }
</script>

{#snippet habitFields(form: HabitInput, idPrefix: string)}
  <div class="field-row">
    <label class="grow">
      <span>Name</span>
      <input type="text" maxlength={HABIT_NAME_MAX} placeholder="e.g. Hair wash" bind:value={form.name} />
    </label>
    <label class="emoji">
      <span>Emoji</span>
      <input type="text" maxlength={HABIT_EMOJI_MAX} placeholder="🌸" bind:value={form.emoji} />
    </label>
  </div>
  <fieldset class="swatches">
    <legend>Colour</legend>
    {#each HABIT_COLORS as color}
      <label class="swatch" style="--swatch: var(--habit-{color})" title={color}>
        <input
          type="radio"
          name="{idPrefix}-color"
          value={color}
          checked={form.color === color}
          onchange={() => (form.color = color)} />
        <span class="sr-only">{color}</span>
      </label>
    {/each}
  </fieldset>
  <div>
    <label class="target">
      <span>Target: every</span>
      <input type="number" min="1" max="3650" step="1" placeholder="—" bind:value={form.targetDays} />
      <span>days</span>
    </label>
    <p class="field-hint">Optional. Marks the habit "Overdue" once it's been longer.</p>
  </div>
{/snippet}

{#snippet logEntry(log: HabitLog)}
  {@const h = habitById.get(log.habitId)}
  {#if editingLogId === log.id}
    <div class="log-edit">
      <div class="field-row">
        <input type="date" max={todayStamp} bind:value={editLogDate} aria-label="Date" />
        <input type="time" bind:value={editLogTime} aria-label="Time" />
      </div>
      <textarea rows="2" maxlength={HABIT_NOTE_MAX} placeholder="Description (optional)" bind:value={editLogNote}></textarea>
      <div class="actions">
        <button type="button" class="primary" disabled={busy} onclick={submitEditLog}>Save</button>
        <button type="button" class="secondary" onclick={() => (editingLogId = null)}>Cancel</button>
      </div>
    </div>
  {:else}
    <div class="log-meta">
      <span class="log-date">{formatDate(log.date)}</span>
      {#if log.time}<span class="log-time">{formatTime(log.time)}</span>{/if}
      {#if !selectedHabit && h}
        <span class="chip" style="--chip: {colorVar(h)}">{habitLabel(h)}</span>
      {/if}
      <span class="spacer"></span>
      <button type="button" class="icon-btn" aria-label="Edit entry" title="Edit" onclick={() => startEditLog(log)}>✎</button>
      <button type="button" class="icon-btn danger" aria-label="Delete entry" title="Delete" onclick={() => removeLog(log)}>✕</button>
    </div>
    {#if log.note}<p class="log-note">{log.note}</p>{/if}
  {/if}
{/snippet}

<div class="page">
  <div class="left-col">
    <section class="card">
      <div class="card-head">
        <h2>Habits</h2>
        <label class="inline-check">
          <input type="checkbox" bind:checked={showHidden} />
          Show hidden
        </label>
      </div>

      {#if loading}
        <p class="hint">Loading…</p>
      {:else if loadError}
        <p class="load-error" role="alert">
          Couldn't load habits: {loadError}
          <button type="button" onclick={() => void load()}>Retry</button>
        </p>
      {:else}
        <ul class="habit-list">
          <li class:selected={selectedId === ALL}>
            <button type="button" class="habit-select" onclick={() => selectHabit(ALL)}>
              <span class="dot all" aria-hidden="true"></span>
              <span class="habit-text">
                <span class="habit-name">All habits</span>
                <span class="habit-sub">{logs.length} log{logs.length === 1 ? "" : "s"}</span>
              </span>
            </button>
          </li>
          {#each visibleHabits as h, i (h.id)}
            {@const stats = statsById.get(h.id)}
            <li class:selected={selectedId === h.id} class:archived={h.archived}>
              {#if editingHabitId === h.id}
                <div class="habit-edit">
                  {@render habitFields(editHabit, `edit-${h.id}`)}
                  <div class="actions">
                    <button type="button" class="primary" disabled={busy} onclick={submitEditHabit}>Save</button>
                    <button type="button" class="secondary" onclick={() => (editingHabitId = null)}>Cancel</button>
                  </div>
                  <div class="actions manage">
                    <button type="button" class="secondary" disabled={busy || i === 0} onclick={() => reorder(h.id, -1)} aria-label="Move up">↑</button>
                    <button
                      type="button"
                      class="secondary"
                      disabled={busy || i === visibleHabits.length - 1}
                      onclick={() => reorder(h.id, 1)}
                      aria-label="Move down">↓</button>
                    <button type="button" class="secondary" disabled={busy} onclick={() => toggleHidden(h)}>
                      {h.archived ? "Show" : "Hide"}
                    </button>
                    <button type="button" class="danger-btn" disabled={busy} onclick={() => removeHabit(h)}>Delete…</button>
                  </div>
                </div>
              {:else}
                <button type="button" class="habit-select" onclick={() => selectHabit(h.id)}>
                  <span class="dot" style="background: {colorVar(h)}" aria-hidden="true">{h.emoji}</span>
                  <span class="habit-text">
                    <span class="habit-name">{h.name}</span>
                    <span class="habit-sub">
                      {formatDaysSince(stats?.daysSince ?? null)}
                      {#if h.targetDays}· every {h.targetDays}d{/if}
                      {#if stats?.overdue}<span class="badge overdue">Overdue</span>{/if}
                      {#if h.archived}<span class="badge">Hidden</span>{/if}
                    </span>
                  </span>
                </button>
                <button
                  type="button"
                  class="icon-btn quick"
                  disabled={busy}
                  title="Log now"
                  aria-label="Log {h.name} now"
                  onclick={() => quickLog(h)}>✓</button>
                <button type="button" class="icon-btn" title="Edit" aria-label="Edit {h.name}" onclick={() => startEditHabit(h)}>✎</button>
              {/if}
            </li>
          {/each}
        </ul>

        {#if adding}
          <div class="habit-edit add">
            {@render habitFields(newHabit, "new")}
            <div class="actions">
              <button type="button" class="primary" disabled={busy} onclick={submitNewHabit}>Add habit</button>
              <button type="button" class="secondary" onclick={() => ((adding = false), (newHabit = blankHabitInput()))}>Cancel</button>
            </div>
          </div>
        {:else}
          <button type="button" class="add-btn" onclick={() => ((adding = true), (habitActionError = null))}>+ Add habit</button>
        {/if}
        {#if habitActionError}<p class="hint error" role="alert">{habitActionError}</p>{/if}
      {/if}
    </section>
  </div>

  <div class="right-col">
    {#if !loading && !loadError}
      {#if habits.length === 0}
        <section class="card empty">
          <h2>Track the things that aren't on a schedule</h2>
          <p class="hint">
            Add a habit on the left — a period, a hair wash, a medication — then log it whenever it
            happens. Each habit gets its own calendar, timeline and "last done" stats. Everything here is
            encrypted on this device.
          </p>
        </section>
      {:else}
        <section class="card">
          <h2>
            {#if selectedHabit}Log {habitLabel(selectedHabit)}{:else}Log a habit{/if}
          </h2>
          {#if !selectedHabit}
            <label class="stack">
              <span>Habit</span>
              <select bind:value={logHabitId}>
                {#each visibleHabits as h (h.id)}
                  <option value={h.id}>{habitLabel(h)}</option>
                {/each}
              </select>
            </label>
          {/if}
          <div class="field-row">
            <label class="stack">
              <span>Date</span>
              <input type="date" max={todayStamp} bind:value={logDate} />
            </label>
            <label class="stack">
              <span>Time</span>
              <input type="time" bind:value={logTime} />
            </label>
          </div>
          <label class="stack">
            <span>Description (optional)</span>
            <textarea rows="2" maxlength={HABIT_NOTE_MAX} bind:value={logNote}></textarea>
          </label>
          <div class="actions">
            <button type="button" class="primary" disabled={busy || !logHabitId} onclick={submitLog}>Log</button>
            {#if loggedFlash}<span class="hint saved">Logged.</span>{/if}
          </div>
          {#if logActionError}<p class="hint error" role="alert">{logActionError}</p>{/if}
        </section>

        {#if selectedHabit && selectedStats}
          <section class="card">
            <div class="stats">
              <div class="stat">
                <span class="stat-value">{formatDaysSince(selectedStats.daysSince)}</span>
                <span class="stat-label">
                  Last done{#if selectedStats.lastDate}: {formatDate(selectedStats.lastDate)}{/if}
                </span>
              </div>
              <div class="stat">
                <span class="stat-value">{selectedStats.avgGapDays === null ? "—" : `${selectedStats.avgGapDays}d`}</span>
                <span class="stat-label">Average gap</span>
              </div>
              <div class="stat">
                <span class="stat-value">{selectedStats.countLast30}</span>
                <span class="stat-label">Last 30 days</span>
              </div>
              <div class="stat">
                <span class="stat-value">{selectedStats.total}</span>
                <span class="stat-label">Total</span>
              </div>
            </div>
            {#if selectedHabit.targetDays}
              <p class="hint" class:error={selectedStats.overdue}>
                Target: every {selectedHabit.targetDays} day{selectedHabit.targetDays === 1 ? "" : "s"}.
                {#if selectedStats.overdue}
                  Overdue by {(selectedStats.daysSince ?? 0) - selectedHabit.targetDays} day{(selectedStats.daysSince ?? 0) - selectedHabit.targetDays === 1 ? "" : "s"}.
                {:else if selectedStats.daysSince !== null}
                  {selectedHabit.targetDays - selectedStats.daysSince} day{selectedHabit.targetDays - selectedStats.daysSince === 1 ? "" : "s"} left.
                {/if}
              </p>
            {/if}
          </section>
        {/if}

        <section class="card">
          <div class="card-head">
            <h2>History</h2>
            <div class="segmented" role="tablist" aria-label="History view">
              <button type="button" role="tab" aria-selected={view === "calendar"} class:active={view === "calendar"} onclick={() => setView("calendar")}>
                Calendar
              </button>
              <button type="button" role="tab" aria-selected={view === "timeline"} class:active={view === "timeline"} onclick={() => setView("timeline")}>
                Timeline
              </button>
            </div>
          </div>

          {#if view === "calendar"}
            <MonthCalendar bind:month={calendarMonth} selectedStamp={selectedDay} {todayStamp} onPick={pickDay} {dayStyle}>
              {#snippet decoration(stamp)}
                {#if selectedHabit}
                  {@const n = scopedByDate.get(stamp)?.length ?? 0}
                  {#if n > 1}<span class="count">×{n}</span>{/if}
                {:else}
                  {@const on = habitsOnDay(stamp)}
                  {#if on.length > 0}
                    <span class="dots">
                      {#each on.slice(0, 4) as h (h.id)}
                        <span class="mini-dot" style="background: {colorVar(h)}"></span>
                      {/each}
                      {#if on.length > 4}<span class="more">+</span>{/if}
                    </span>
                  {/if}
                {/if}
              {/snippet}
            </MonthCalendar>

            <h3 class="day-heading">{formatDate(selectedDay)}</h3>
            {#if selectedDayLogs.length === 0}
              <p class="hint">Nothing logged this day.</p>
            {:else}
              <ul class="log-list">
                {#each selectedDayLogs as log (log.id)}
                  <li style="--chip: {colorVar(habitById.get(log.habitId))}">{@render logEntry(log)}</li>
                {/each}
              </ul>
            {/if}
          {:else if scopedLogs.length === 0}
            <p class="hint">Nothing logged yet.</p>
          {:else}
            {#each timelineGroups as group (group.key)}
              <h3 class="month-heading">{group.label}</h3>
              <ul class="timeline">
                {#each group.items as item (item.log.id)}
                  <li style="--chip: {colorVar(habitById.get(item.log.habitId))}">
                    {@render logEntry(item.log)}
                    {#if item.gap !== null && item.gap > 0}
                      <span class="gap">{item.gap} day{item.gap === 1 ? "" : "s"} after the previous one</span>
                    {/if}
                  </li>
                {/each}
              </ul>
            {/each}
            {#if scopedLogs.length > timelineLimit}
              <button type="button" class="add-btn" onclick={() => (timelineLimit += TIMELINE_PAGE)}>
                Show older ({scopedLogs.length - timelineLimit} more)
              </button>
            {/if}
          {/if}
        </section>
      {/if}
    {/if}
  </div>
</div>

<style>
  .page {
    padding: 24px;
    display: grid;
    grid-template-columns: 320px 1fr;
    gap: 20px;
    max-width: 1200px;
    margin: 0 auto;
  }

  /* Same large-window scaling as the Entries page. */
  @media (min-width: 1200px) {
    .page {
      zoom: 1.15;
      max-width: calc(1200px / 1.15);
    }
  }

  .left-col,
  .right-col {
    display: flex;
    flex-direction: column;
    gap: 20px;
    min-width: 0;
  }

  @media (max-width: 600px) {
    .page {
      grid-template-columns: 1fr;
      padding: 16px;
      gap: 16px;
    }

    .left-col,
    .right-col {
      gap: 16px;
    }
  }

  .card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 14px;
    padding: 20px;
  }

  h2 {
    font-size: 15px;
    margin: 0 0 12px;
  }

  .card-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: 8px;
    margin-bottom: 12px;
  }

  .card-head h2 {
    margin: 0;
  }

  .inline-check {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
    color: var(--text-dim);
  }

  .hint {
    color: var(--text-dim);
    font-size: 13px;
  }

  .hint.error {
    color: var(--danger);
  }

  .hint.saved {
    color: var(--accent);
  }

  .load-error {
    color: var(--danger);
    font-size: 0.9rem;
    margin: 0.5rem 0;
  }

  .load-error button {
    margin-left: 0.5rem;
  }

  /* --- Habit list --- */

  .habit-list {
    list-style: none;
    margin: 0 0 12px;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .habit-list li {
    display: flex;
    align-items: center;
    gap: 4px;
    border-radius: 10px;
    padding: 2px 4px 2px 0;
  }

  .habit-list li.selected {
    background: var(--accent-soft);
  }

  .habit-list li.archived .habit-name {
    color: var(--text-dim);
  }

  .habit-select {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 10px;
    background: none;
    border: none;
    color: inherit;
    text-align: left;
    padding: 8px;
    border-radius: 10px;
  }

  .habit-select:hover {
    background: var(--surface-2);
  }

  .habit-list li.selected .habit-select:hover {
    background: transparent;
  }

  .dot {
    flex: none;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font-size: 15px;
  }

  .dot.all {
    background: conic-gradient(var(--habit-rose), var(--habit-amber), var(--habit-teal), var(--habit-indigo), var(--habit-rose));
  }

  .habit-text {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .habit-name {
    font-size: 14px;
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .habit-sub {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 4px 6px;
    font-size: 12px;
    color: var(--text-dim);
  }

  .badge {
    flex: none;
    font-size: 10px;
    background: var(--surface-2);
    color: var(--text-dim);
    padding: 1px 7px;
    border-radius: 999px;
  }

  .badge.overdue {
    background: color-mix(in srgb, var(--danger) 15%, transparent);
    color: var(--danger);
  }

  .icon-btn {
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 32px;
    min-height: 32px;
    background: none;
    border: none;
    color: var(--text-dim);
    border-radius: 8px;
    font-size: 15px;
  }

  .icon-btn:hover:not(:disabled) {
    background: var(--surface-2);
    color: var(--accent);
  }

  .icon-btn.danger:hover:not(:disabled) {
    color: var(--danger);
  }

  .icon-btn.quick {
    font-weight: 700;
  }

  .habit-edit {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: 10px;
    background: var(--surface);
  }

  .habit-edit.add {
    margin-bottom: 8px;
  }

  .add-btn {
    background: none;
    border: 1px dashed var(--border);
    color: var(--text-dim);
    border-radius: 10px;
    padding: 8px 12px;
    width: 100%;
    font-size: 13px;
  }

  .add-btn:hover {
    color: var(--accent);
    border-color: var(--accent);
  }

  /* --- Forms --- */

  .field-row {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
  }

  .field-row > * {
    flex: 1 1 120px;
  }

  label.grow {
    flex: 3 1 140px;
  }

  label.emoji {
    flex: 0 0 72px;
  }

  label.grow,
  label.emoji,
  label.stack {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12px;
    color: var(--text-dim);
    margin-bottom: 10px;
  }

  .habit-edit label.grow,
  .habit-edit label.emoji {
    margin-bottom: 0;
  }

  input[type="text"],
  input[type="number"],
  input[type="date"],
  input[type="time"],
  select,
  textarea {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 8px;
    color: var(--text);
    padding: 8px 10px;
    font-size: 14px;
    font-family: inherit;
    min-width: 0;
    width: 100%;
  }

  textarea {
    resize: vertical;
  }

  .swatches {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
    border: none;
    margin: 0;
    padding: 0;
  }

  .swatches legend {
    font-size: 12px;
    color: var(--text-dim);
    margin-bottom: 4px;
    padding: 0;
  }

  .swatch {
    position: relative;
    width: 24px;
    height: 24px;
    border-radius: 50%;
    background: var(--swatch);
    cursor: pointer;
  }

  .swatch input {
    position: absolute;
    opacity: 0;
    inset: 0;
    margin: 0;
    cursor: pointer;
  }

  .swatch:has(input:checked) {
    box-shadow:
      0 0 0 2px var(--surface),
      0 0 0 4px var(--swatch);
  }

  .swatch:has(input:focus-visible) {
    outline: 2px solid var(--accent);
    outline-offset: 4px;
  }

  .target {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 6px;
    font-size: 12px;
    color: var(--text-dim);
  }

  .target input {
    width: 5em;
  }

  .field-hint {
    margin: 4px 0 0;
    font-size: 11px;
    color: var(--text-dim);
  }

  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }

  .actions.manage {
    padding-top: 8px;
    border-top: 1px solid var(--border);
  }

  .primary {
    background: var(--accent);
    color: white;
    border: none;
    padding: 7px 14px;
    border-radius: 8px;
    font-size: 13px;
  }

  .secondary {
    background: none;
    border: 1px solid var(--border);
    color: inherit;
    padding: 6px 12px;
    border-radius: 8px;
    font-size: 13px;
  }

  .danger-btn {
    background: none;
    border: 1px solid var(--danger);
    color: var(--danger);
    padding: 6px 12px;
    border-radius: 8px;
    font-size: 13px;
    margin-left: auto;
  }

  button:disabled {
    opacity: 0.5;
    cursor: default;
  }

  /* --- Stats --- */

  .stats {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 10px;
  }

  @media (max-width: 600px) {
    .stats {
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

  .stats + .hint {
    margin: 12px 0 0;
  }

  /* --- History --- */

  .segmented {
    display: inline-flex;
    background: var(--surface-2);
    border-radius: 8px;
    padding: 2px;
  }

  .segmented button {
    background: none;
    border: none;
    color: var(--text-dim);
    padding: 5px 12px;
    border-radius: 6px;
    font-size: 13px;
  }

  .segmented button.active {
    background: var(--surface);
    color: var(--text);
  }

  .count {
    font-size: 10px;
    color: var(--text-dim);
    line-height: 1;
  }

  .dots {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    line-height: 1;
  }

  .mini-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
  }

  .more {
    font-size: 9px;
    color: var(--text-dim);
  }

  .day-heading,
  .month-heading {
    font-size: 13px;
    font-weight: 600;
    margin: 18px 0 8px;
  }

  .month-heading:first-of-type {
    margin-top: 0;
  }

  .log-list,
  .timeline {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .log-list li,
  .timeline li {
    padding-left: 12px;
    border-left: 3px solid var(--chip, var(--border));
  }

  .log-meta {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
  }

  .log-date {
    font-size: 13px;
    font-weight: 500;
  }

  .log-time {
    font-size: 12px;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
  }

  .chip {
    font-size: 11px;
    padding: 2px 8px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--chip) 18%, transparent);
  }

  .spacer {
    flex: 1;
  }

  .log-meta .icon-btn {
    min-width: 28px;
    min-height: 28px;
    font-size: 13px;
  }

  .log-note {
    margin: 4px 0 0;
    font-size: 14px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .log-edit {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .gap {
    display: block;
    margin-top: 6px;
    font-size: 11px;
    color: var(--text-dim);
  }

  .empty p {
    margin: 0;
    line-height: 1.5;
  }
</style>
