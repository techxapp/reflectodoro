<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { error as logError, info as logInfo } from "@tauri-apps/plugin-log";
  import { slotFor } from "$lib/grid";
  import {
    getTaskList,
    saveTaskList,
    getNotToDoList,
    saveNotToDoList,
    localDateStamp,
    loadAndSyncBreakitSettings,
    loadAndSyncForceCloseShortcutSetting,
    loadAndSyncOverlayAutoClose,
    loadAndSyncMediaPauseOnBreakSetting,
    saveMediaPauseOnBreakEnabled,
    loadAndSyncBreakNotificationPersistentSetting,
    loadAndSyncMediaToggleGuard,
    loadAndSyncMacosHideMenuBarDockSetting,
    listenForTaskListUpdates,
    listenForNotToDoListUpdates,
    listenForMediaToggleRecorded,
    loadAndSyncScreenTimeTrackingSetting,
    ensureDeviceName,
  } from "$lib/db";

  type SnoozeInfo = { resume_at: string; minutes: number };
  const SNOOZE_MINUTES_OPTIONS = [30, 60, 120, 360, 720];
  function snoozeOptionLabel(minutes: number): string {
    const hours = minutes / 60;
    return `Pause for ${hours < 1 ? `${minutes} min` : `${hours} hr`}`;
  }

  let now = $state(new Date());
  let enabled = $state(true);
  let snoozeInfo = $state<SnoozeInfo | null>(null);
  let mediaPauseOnBreakEnabled = $state(true);
  let mediaPauseOnBreakLoaded = $state(false);
  let mediaPauseOnBreakBusy = $state(false);
  let isMacos = $state(false);
  // Defaults to true so nothing flashes before the real check resolves.
  let mediaKeyPermissionGranted = $state(true);
  let taskListContent = $state("");
  let notToDoContent = $state("");
  let unlisten: UnlistenFn | null = null;
  let unlistenSnooze: UnlistenFn | null = null;
  let unlistenTasks: UnlistenFn | null = null;
  let unlistenNotToDo: UnlistenFn | null = null;
  let unlistenMediaToggle: UnlistenFn | null = null;
  let tickInterval: ReturnType<typeof setInterval> | null = null;
  let taskSaveTimer: ReturnType<typeof setTimeout> | null = null;
  let notToDoSaveTimer: ReturnType<typeof setTimeout> | null = null;

  const slot = $derived(slotFor(now));
  const remainingLabel = $derived.by(() => {
    const ms = slot.end.getTime() - now.getTime();
    const totalSec = Math.max(0, Math.round(ms / 1000));
    const m = Math.floor(totalSec / 60);
    const s = totalSec % 60;
    return `${m}:${String(s).padStart(2, "0")}`;
  });

  // Drives the dropdown's selected option: a snooze always wins over the
  // plain enabled flag (POMODORO_ENABLED is false for both a snooze and a
  // permanent Off -- snoozeInfo is what tells them apart).
  const pomodoroSelection = $derived(snoozeInfo ? String(snoozeInfo.minutes) : enabled ? "on" : "off");
  const snoozeResumeLabel = $derived.by(() => {
    if (!snoozeInfo) return "";
    const resumeAt = new Date(snoozeInfo.resume_at);
    return `Resumes at ${resumeAt.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}`;
  });

  function scheduleTaskSave() {
    if (taskSaveTimer) clearTimeout(taskSaveTimer);
    taskSaveTimer = setTimeout(() => {
      void saveTaskList(localDateStamp(), taskListContent);
    }, 800);
  }

  function scheduleNotToDoSave() {
    if (notToDoSaveTimer) clearTimeout(notToDoSaveTimer);
    notToDoSaveTimer = setTimeout(() => {
      void saveNotToDoList(localDateStamp(), notToDoContent);
    }, 800);
  }

  async function handlePomodoroSelect(event: Event) {
    const value = (event.currentTarget as HTMLSelectElement).value;
    if (value === "on") {
      enabled = true;
      snoozeInfo = null;
      await invoke("set_enabled", { enabled: true });
    } else if (value === "off") {
      enabled = false;
      snoozeInfo = null;
      await invoke("set_enabled", { enabled: false });
    } else {
      const minutes = Number(value);
      enabled = false;
      // Uses the command's own return value rather than waiting on the
      // pomodoro://snooze-changed event, so this window's dropdown/hint
      // update immediately instead of racing it (see best_practices.md).
      snoozeInfo = await invoke<SnoozeInfo>("snooze_pomodoro", { minutes });
    }
  }

  async function toggleMediaPauseOnBreak() {
    const next = !mediaPauseOnBreakEnabled;
    mediaPauseOnBreakBusy = true;
    try {
      await saveMediaPauseOnBreakEnabled(next);
      mediaPauseOnBreakEnabled = next;
    } finally {
      mediaPauseOnBreakBusy = false;
    }
  }

  async function refreshMediaKeyPermission() {
    if (!isMacos) return;
    mediaKeyPermissionGranted = await invoke<boolean>("get_media_key_permission_granted");
  }

  async function requestMediaKeyPermission() {
    await invoke("request_media_key_permission");
    await refreshMediaKeyPermission();
  }

  // Granting happens in System Settings, so re-check when the user comes back.
  function onWindowFocus() {
    void refreshMediaKeyPermission();
  }

  /** The boot sequence below is a chain of awaits: before this wrapper, the
   * first one to throw silently killed every step after it -- including the
   * event-listener registrations at the end, so the window kept running while
   * quietly not receiving task-list updates or screen-time batches, with no
   * error anywhere. Log it instead. */
  async function bootMainWindow() {
    await loadAndSyncBreakitSettings();
    await loadAndSyncForceCloseShortcutSetting();
    await loadAndSyncOverlayAutoClose();
    mediaPauseOnBreakEnabled = await loadAndSyncMediaPauseOnBreakSetting();
    mediaPauseOnBreakLoaded = true;
    isMacos = (await invoke<string>("current_os")) === "macos";
    await refreshMediaKeyPermission();
    await loadAndSyncBreakNotificationPersistentSetting();
    await loadAndSyncMediaToggleGuard();
    await loadAndSyncMacosHideMenuBarDockSetting();
    await loadAndSyncScreenTimeTrackingSetting();
    await ensureDeviceName();
    enabled = await invoke<boolean>("get_enabled");
    snoozeInfo = await invoke<SnoozeInfo | null>("get_snooze_until");
    taskListContent = await getTaskList(localDateStamp());
    notToDoContent = await getNotToDoList(localDateStamp());

    unlisten = await listen<boolean>("pomodoro://enabled-changed", (event) => {
      enabled = event.payload;
    });
    unlistenSnooze = await listen<SnoozeInfo | null>("pomodoro://snooze-changed", (event) => {
      snoozeInfo = event.payload;
    });
    unlistenTasks = await listenForTaskListUpdates((content) => {
      taskListContent = content;
    });
    unlistenNotToDo = await listenForNotToDoListUpdates((content) => {
      notToDoContent = content;
    });
    unlistenMediaToggle = await listenForMediaToggleRecorded();
    // The screen-time batch listener lives in +layout.svelte, not here --
    // this route ("/") unmounts on every tab navigation, which would tear
    // the listener down and silently drop any batch Rust flushes while the
    // user is sitting on another tab. See +layout.svelte's onMount for why.
    void logInfo("main window: boot sequence complete, all listeners registered");
  }

  onMount(async () => {
    window.addEventListener("focus", onWindowFocus);
    // Started immediately, before bootMainWindow's long chain of sequential
    // awaited IPC round-trips (settings syncs, get_enabled, get_snooze_until,
    // task-list reads, listener registrations) -- this route unmounts on
    // every client-side tab navigation and remounts when the user comes back
    // to it (see +layout.svelte's screen-time-listener comment for why), so
    // `onMount` -- and this boot chain -- re-runs on every such visit. With
    // the interval previously only started at the end of that chain, `now`
    // (initialized once at component creation) sat frozen for however long
    // the chain took on each remount, showing stale clock/countdown time
    // right after switching back to this tab. Resyncing `now` here too
    // covers the gap between component creation and this line, same
    // reasoning as the overlay's identical fix.
    now = new Date();
    tickInterval = setInterval(() => (now = new Date()), 1000);
    try {
      await bootMainWindow();
    } catch (e) {
      void logError(`main window boot failed: ${e instanceof Error ? e.message : String(e)}`);
    }
  });

  onDestroy(() => {
    window.removeEventListener("focus", onWindowFocus);
    unlisten?.();
    unlistenSnooze?.();
    unlistenTasks?.();
    unlistenNotToDo?.();
    unlistenMediaToggle?.();
    if (tickInterval) clearInterval(tickInterval);
    if (taskSaveTimer) clearTimeout(taskSaveTimer);
    if (notToDoSaveTimer) clearTimeout(notToDoSaveTimer);
  });
</script>

<div class="page">
  <section class="card timer-card">
    <p class="label">{slot.phase === "work" ? "Working" : "On break"}</p>
    <p class="big">{remainingLabel}</p><br/>
    <select class="pomodoro-select" class:off={!enabled} value={pomodoroSelection} onchange={handlePomodoroSelect}>
      <option value="on">Pomodoro mode: On</option>
      {#each SNOOZE_MINUTES_OPTIONS as minutes (minutes)}
        <option value={String(minutes)}>{snoozeOptionLabel(minutes)}</option>
      {/each}
      <option value="off">Pomodoro mode: Off</option>
    </select>
    {#if snoozeInfo}
      <p class="hint">{snoozeResumeLabel}</p>
    {/if}

    {#if mediaPauseOnBreakLoaded}
      <button
        class="toggle media-toggle"
        class:off={!mediaPauseOnBreakEnabled}
        disabled={mediaPauseOnBreakBusy}
        onclick={toggleMediaPauseOnBreak}
      >
        {mediaPauseOnBreakEnabled ? "Pause media on break: On" : "Pause media on break: Off"}
      </button>
      {#if isMacos && mediaPauseOnBreakEnabled && !mediaKeyPermissionGranted}
        <p class="hint">macOS needs Accessibility access for Reflectodoro to pause media.</p>
        <button class="toggle permission-button" onclick={requestMediaKeyPermission}>
          Grant Accessibility access…
        </button>
      {/if}
    {/if}
  </section>

  <section class="card">
    <h2>Most Important Tasks Today</h2>
    <textarea
      bind:value={taskListContent}
      oninput={scheduleTaskSave}
      placeholder="1.
2.
3."
      rows="5"
    ></textarea>
    <p class="hint">Auto-saves as you type.</p>
  </section>

  <section class="card">
    <h2>Not To Do Tasks Today</h2>
    <textarea
      bind:value={notToDoContent}
      oninput={scheduleNotToDoSave}
      placeholder="1.
2.
3."
      rows="3"
    ></textarea>
    <p class="hint">Auto-saves as you type.</p>
  </section>
</div>

<style>
  .page {
    padding: 24px;
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 20px;
    max-width: 900px;
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
    padding: 24px;
  }

  .timer-card {
    text-align: center;
  }

  .label {
    text-transform: uppercase;
    letter-spacing: 0.08em;
    font-size: 12px;
    color: var(--text-dim);
    margin: 0 0 8px;
  }

  .big {
    font-size: 48px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    margin: 0;
  }

  .sub {
    color: var(--text-dim);
    font-size: 12px;
    margin: 8px 0 20px;
  }

  .toggle {
    background: var(--accent-soft);
    color: var(--accent);
    border: none;
    border-radius: 10px;
    padding: 10px 16px;
    font-size: 14px;
    font-weight: 500;
  }

  .toggle.off {
    background: var(--surface-2);
    color: var(--text-dim);
  }

  .pomodoro-select {
    background: var(--accent-soft);
    color: var(--accent);
    border: none;
    border-radius: 10px;
    padding: 10px 16px;
    font-size: 14px;
    font-weight: 500;
    font-family: inherit;
    cursor: pointer;
  }

  .pomodoro-select.off {
    background: var(--surface-2);
    color: var(--text-dim);
  }

  .media-toggle {
    display: block;
    margin: 14px auto 0;
  }

  .permission-button {
    display: block;
    margin: 8px auto 0;
    padding: 6px 12px;
    font-size: 12px;
  }

  h2 {
    margin: 0 0 12px;
    font-size: 15px;
  }

  textarea {
    width: 100%;
    box-sizing: border-box;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 10px;
    color: inherit;
    padding: 10px 12px;
    font-size: 14px;
    resize: vertical;
  }

  .hint {
    font-size: 12px;
    color: var(--text-dim);
    margin: 8px 0 0;
  }
</style>
