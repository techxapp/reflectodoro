<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
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
    validateBulkEditRange,
    previewBulkEditSlots,
    bulkUpsertReflections,
    type BulkEditSlotPreview,
    getScreenTimeForDate,
    getScreenTimeTrackingEnabled,
    getScreenTimeAppThresholdMinutes,
    getCurrentScreenTimeSession,
    getDeviceName,
    type ReflectionRow,
    type ScreenTimeEntry,
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
  let screenTime = $state<ScreenTimeEntry[]>([]);
  let screenTimeLoaded = $state(false);
  let screenTimeTrackingOn = $state(true);
  let screenTimeThresholdMinutes = $state(5);
  // Windows and Android are the only platforms capturing focus so far --
  // without this the empty state on the others reads as "you did nothing
  // today" rather than "nothing is recording yet".
  let captureSupported = $state(true);
  let isAndroid = $state(false);
  // Android's capture depends on the special-access "Usage access" grant
  // (see screen_time.rs's Android platform_impl); without it tracking can be
  // on and still record nothing, which would otherwise look identical to a
  // genuinely quiet day.
  let usageAccessGranted = $state(true);
  let taskSaveTimer: ReturnType<typeof setTimeout> | undefined;
  let notToDoSaveTimer: ReturnType<typeof setTimeout> | undefined;

  // --- Bulk edit reflections by time range ---
  let bulkEditOpen = $state(false);
  let bulkStart = $state("");
  let bulkEnd = $state("");
  let bulkText = $state("");
  let bulkError = $state("");
  let bulkPreview = $state<BulkEditSlotPreview[] | null>(null);
  let bulkPreviewLoading = $state(false);
  let bulkApplying = $state(false);
  // Tracks which field values the current bulkPreview was computed for, so
  // editing any field after previewing invalidates it instead of letting
  // "Apply" act on stale (possibly no-longer-matching) slots.
  let bulkPreviewFor = $state("");

  const selectedStamp = $derived(localDateStamp(selected));
  const isToday = $derived(selectedStamp === localDateStamp(new Date()));
  const clusters = $derived(clusterReflectionRows(reflectionRows));

  // Bumped on every load() call and captured per-call so a load for a day the
  // user has already navigated away from can't win a race against a load for
  // the day they're now on -- without this, the last-*resolving* call wins
  // regardless of which was started last, and since taskList/notToDo are
  // bind:value, a late-resolving load for day A can clobber text already
  // typed for day B (which the debounced saver then persists under day B).
  let loadGeneration = 0;

  async function load() {
    loading = true;
    const stamp = selectedStamp;
    const generation = ++loadGeneration;
    const [r, t, n, w] = await Promise.all([
      getReflectionsForDate(stamp),
      getTaskList(stamp),
      getNotToDoList(stamp),
      getWellnessSummaryForDate(stamp),
    ]);
    if (generation !== loadGeneration) return;
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

  function resetBulkEdit() {
    bulkEditOpen = false;
    bulkStart = "";
    bulkEnd = "";
    bulkText = "";
    bulkError = "";
    bulkPreview = null;
    bulkPreviewLoading = false;
    bulkApplying = false;
    bulkPreviewFor = "";
  }

  function bulkFieldsKey(): string {
    return `${bulkStart}|${bulkEnd}|${bulkText}`;
  }

  async function previewBulkEdit() {
    const text = bulkText.trim();
    if (!text) {
      bulkError = "Text is required";
      bulkPreview = null;
      return;
    }
    const rangeError = validateBulkEditRange(bulkStart, bulkEnd);
    if (rangeError) {
      bulkError = rangeError;
      bulkPreview = null;
      return;
    }
    bulkError = "";
    bulkPreviewLoading = true;
    try {
      bulkPreview = await previewBulkEditSlots(selectedStamp, bulkStart, bulkEnd);
      bulkPreviewFor = bulkFieldsKey();
    } catch (e) {
      bulkError = e instanceof Error ? e.message : String(e);
      bulkPreview = null;
    } finally {
      bulkPreviewLoading = false;
    }
  }

  async function applyBulkEdit() {
    // Fields changed since the preview was computed -- re-preview instead of
    // upserting against possibly-stale slots.
    if (bulkFieldsKey() !== bulkPreviewFor) {
      await previewBulkEdit();
      return;
    }
    bulkApplying = true;
    try {
      await bulkUpsertReflections(selectedStamp, bulkStart, bulkEnd, bulkText);
      resetBulkEdit();
      await load();
    } catch (e) {
      bulkError = e instanceof Error ? e.message : String(e);
      bulkApplying = false;
    }
  }

  const bulkOverwriteCount = $derived(bulkPreview?.filter((s) => s.hasExisting).length ?? 0);

  /** Same generation guard as load() above, for the same reason: a
   * slow screen-time query for a day the user has navigated away from must
   * not overwrite the day they're now looking at. */
  let screenTimeGeneration = 0;

  async function loadScreenTime() {
    const stamp = selectedStamp;
    const forToday = stamp === localDateStamp(new Date());
    const generation = ++screenTimeGeneration;
    const [enabled, entries, deviceName, current, thresholdMinutes] = await Promise.all([
      getScreenTimeTrackingEnabled(),
      getScreenTimeForDate(stamp),
      getDeviceName(),
      // Only today can have an in-progress session to blend in; asking on any
      // other day would attribute the currently-focused app to that day.
      forToday ? getCurrentScreenTimeSession() : Promise.resolve(null),
      getScreenTimeAppThresholdMinutes(),
    ]);
    if (generation !== screenTimeGeneration) return;

    let blended = entries;
    if (current) {
      const existing = blended.find(
        (e) => e.appId === current.appId && e.deviceName === deviceName,
      );
      blended = existing
        ? blended.map((e) => (e === existing ? { ...e, ms: e.ms + current.elapsedMs } : e))
        : [
            ...blended,
            {
              appId: current.appId,
              displayName: current.displayName,
              platform: "",
              deviceName,
              ms: current.elapsedMs,
            },
          ];
      blended = [...blended].sort((a, b) => b.ms - a.ms);
    }

    screenTimeTrackingOn = enabled;
    screenTime = blended;
    screenTimeThresholdMinutes = thresholdMinutes;
    screenTimeLoaded = true;
  }

  // Apps with only a few seconds/minutes of focus (an alt-tab, a notification
  // popup) otherwise drown out where the day actually went -- see
  // getScreenTimeAppThresholdMinutes in db.ts. The total below still sums
  // every entry, filtered or not, so it keeps reading as "the whole day",
  // not just the apps visible under the threshold.
  const visibleScreenTime = $derived(
    screenTime.filter((e) => e.ms >= screenTimeThresholdMinutes * 60000),
  );
  const screenTimeTotalMs = $derived(screenTime.reduce((sum, e) => sum + e.ms, 0));
  const screenTimeMaxMs = $derived(visibleScreenTime.reduce((max, e) => Math.max(max, e.ms), 0));
  /** Only worth showing a device label once rows from more than one device
   * actually exist -- i.e. after a cross-device import. */
  const showDeviceNames = $derived(new Set(visibleScreenTime.map((e) => e.deviceName)).size > 1);

  function formatDuration(ms: number): string {
    const totalMinutes = Math.floor(ms / 60000);
    if (totalMinutes < 1) return `${Math.max(0, Math.round(ms / 1000))}s`;
    const hours = Math.floor(totalMinutes / 60);
    const minutes = totalMinutes % 60;
    return hours > 0 ? `${hours}h ${minutes}m` : `${minutes}m`;
  }

  /** The in-progress session's elapsed time is read live from memory, not from
   * the DB, so today's totals go stale while the window sits in the
   * background. Refresh on focus rather than on a timer -- there's nothing to
   * see while the window isn't being looked at. */
  function onWindowFocus() {
    void loadScreenTime();
  }

  async function refreshUsageAccess() {
    if (!isAndroid) return;
    usageAccessGranted = await invoke<boolean>("can_query_usage_stats");
  }

  function onEntriesVisibilityChange() {
    // Catches the user granting Usage Access in system Settings and coming
    // back -- same pattern as settings/+page.svelte's own permission
    // re-checks.
    if (document.visibilityState === "visible") void refreshUsageAccess();
  }

  onMount(async () => {
    window.addEventListener("focus", onWindowFocus);
    document.addEventListener("visibilitychange", onEntriesVisibilityChange);
    const os = await invoke<string>("current_os");
    captureSupported = os === "windows" || os === "android";
    isAndroid = os === "android";
    await refreshUsageAccess();
  });

  onDestroy(() => {
    window.removeEventListener("focus", onWindowFocus);
    document.removeEventListener("visibilitychange", onEntriesVisibilityChange);
  });

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
    void loadScreenTime();
  });
</script>

<div class="page">
  <div class="left-col">
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

  <section class="card screen-time">
    <h2>Screen time</h2>

    <!-- Rows first, whatever the current settings say: a day can hold data
         recorded before tracking was switched off, or imported from a
         platform that does capture. The messages below are empty states, not
         status banners. -->
    {#if !screenTimeLoaded}
      <p class="hint">Loading&hellip;</p>
    {:else if screenTime.length === 0 && !screenTimeTrackingOn}
      <p class="hint">
        Tracking is off. Turn it on in <a href="/settings">Settings</a> to see where your day went.
      </p>
    {:else if screenTime.length === 0 && !captureSupported}
      <p class="hint">Screen time isn't captured on this platform yet.</p>
    {:else if screenTime.length === 0 && isAndroid && !usageAccessGranted}
      <p class="hint">
        Usage access isn't granted, so nothing can be recorded yet. Turn it on in
        <a href="/settings">Settings</a>.
      </p>
    {:else if screenTime.length === 0}
      <p class="hint">Nothing recorded for this day.</p>
    {:else if visibleScreenTime.length === 0}
      <p class="hint">
        {formatDuration(screenTimeTotalMs)} total, but nothing over {screenTimeThresholdMinutes}
        min per app.
      </p>
    {:else}
      <p class="st-total">{formatDuration(screenTimeTotalMs)} total</p>
      <ul class="st-list">
        {#each visibleScreenTime as entry (entry.appId + "|" + entry.deviceName)}
          <li>
            <div class="st-row">
              <span class="st-app" title={entry.appId}>
                {entry.displayName}{#if showDeviceNames && entry.deviceName}<span class="st-device"
                    >{entry.deviceName}</span
                  >{/if}
              </span>
              <span class="st-duration">{formatDuration(entry.ms)}</span>
            </div>
            <div class="st-bar">
              <div
                class="st-bar-fill"
                style={`width: ${screenTimeMaxMs > 0 ? (entry.ms / screenTimeMaxMs) * 100 : 0}%`}
              ></div>
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  </section>
  </div>

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

      <div class="bulk-edit">
        {#if !bulkEditOpen}
          <button class="bulk-toggle" onclick={() => (bulkEditOpen = true)}>+ Bulk edit reflections</button>
        {:else}
          <div class="bulk-form">
            <h3>Bulk edit reflections</h3>
            <p class="hint">
              Set the same text on every complete 30-minute slot between a start and end time on
              {selected.toLocaleDateString(undefined, { month: "long", day: "numeric" })}.
            </p>
            <div class="bulk-fields">
              <label>
                Start time
                <input type="time" bind:value={bulkStart} onchange={() => (bulkPreview = null)} />
              </label>
              <label>
                End time
                <input type="time" bind:value={bulkEnd} onchange={() => (bulkPreview = null)} />
              </label>
            </div>
            <label class="bulk-text-label">
              Text
              <input
                type="text"
                bind:value={bulkText}
                oninput={() => (bulkPreview = null)}
                placeholder="What were you doing?"
              />
            </label>

            {#if bulkError}
              <p class="bulk-error">{bulkError}</p>
            {/if}

            {#if bulkPreview}
              <p class="bulk-summary">
                This will set {bulkPreview.length} slot{bulkPreview.length === 1 ? "" : "s"} between
                {bulkStart} and {bulkEnd} to this text.
                {#if bulkOverwriteCount > 0}
                  {bulkOverwriteCount} already {bulkOverwriteCount === 1 ? "has" : "have"} an entry and
                  will be overwritten.
                {/if}
              </p>
            {/if}

            <div class="edit-actions">
              {#if bulkPreview}
                <button class="save" disabled={bulkApplying} onclick={applyBulkEdit}>
                  {bulkApplying ? "Applying…" : "Apply"}
                </button>
              {:else}
                <button class="save" disabled={bulkPreviewLoading} onclick={previewBulkEdit}>
                  {bulkPreviewLoading ? "Checking…" : "Preview"}
                </button>
              {/if}
              <button class="cancel" onclick={resetBulkEdit}>Cancel</button>
            </div>
          </div>
        {/if}
      </div>

      {#if clusters.length === 0}
        <p class="hint">No reflections logged for this day.</p>
      {:else}
        <ul class="reflection-list">
          {#each clusters as cluster (cluster.rows[0].id)}
            {@const clusterKey = cluster.rows[0].id}
            <li>
              <div class="meta">
                <span class="time">{formatTime(cluster.rows[0].slot_start_at)}</span>
                {#if cluster.rows.length === 1}
                  {@render editButton(cluster.rows[0])}
                {:else}
                  <span
                    class="badge"
                    role="button"
                    tabindex="0"
                    onclick={() => toggleExpanded(clusterKey)}
                    onkeydown={(e) => {
                      if (e.key === "Enter" || e.key === " ") {
                        e.preventDefault();
                        toggleExpanded(clusterKey);
                      }
                    }}
                  >covers {cluster.rows.length} pomodoros</span>
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

  /* Left column stacks calendar-then-screen-time as plain flex children, so
     each card is only ever as tall as its own content -- no row-spanning
     grid track to inflate them (that used to leave a large gap between the
     calendar and screen-time cards whenever the reflections list was long). */
  .left-col {
    display: flex;
    flex-direction: column;
    gap: 20px;
  }

  @media (max-width: 600px) {
    .page {
      grid-template-columns: 1fr;
      padding: 16px;
      gap: 16px;
    }

    .left-col {
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

  .bulk-edit {
    margin-bottom: 16px;
  }

  .bulk-toggle {
    background: var(--surface-2);
    border: none;
    color: inherit;
    padding: 8px 12px;
    border-radius: 8px;
    font-size: 13px;
  }

  .bulk-toggle:hover {
    background: var(--surface);
  }

  .bulk-form {
    background: var(--surface-2);
    border-radius: 10px;
    padding: 14px;
  }

  .bulk-form h3 {
    margin: 0 0 4px;
    font-size: 14px;
  }

  .bulk-form .hint {
    margin: 0 0 12px;
  }

  .bulk-fields {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 10px;
    margin-bottom: 10px;
  }

  .bulk-fields label,
  .bulk-text-label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12px;
    color: var(--text-dim);
  }

  .bulk-text-label {
    margin-bottom: 10px;
  }

  .bulk-fields input,
  .bulk-text-label input {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 8px;
    color: inherit;
    padding: 8px 10px;
    font-size: 14px;
    font-family: inherit;
  }

  .bulk-error {
    color: #e05555;
    font-size: 12px;
    margin: 0 0 10px;
  }

  .bulk-summary {
    font-size: 12px;
    color: var(--text-dim);
    margin: 0 0 10px;
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
    cursor: pointer;
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

  .screen-time h2 {
    font-size: 14px;
    margin: 0 0 4px;
  }

  .st-total {
    color: var(--text-dim);
    font-size: 12px;
    font-variant-numeric: tabular-nums;
    margin: 0 0 12px;
  }

  .st-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .st-row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 4px;
  }

  .st-app {
    font-size: 13px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .st-device {
    color: var(--text-dim);
    font-size: 11px;
    margin-left: 6px;
  }

  .st-duration {
    font-size: 12px;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
    flex-shrink: 0;
  }

  .st-bar {
    background: var(--surface-2);
    border-radius: 999px;
    height: 6px;
    overflow: hidden;
  }

  .st-bar-fill {
    background: var(--accent);
    height: 100%;
    border-radius: 999px;
  }
</style>
