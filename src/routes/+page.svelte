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
    loadAndSyncHideOverlayOnCallSetting,
    loadAndSyncMediaToggleGuard,
    loadAndSyncMacosHideMenuBarDockSetting,
    loadAndSyncMacosMediaKeyFallbackSetting,
    getMediaPauseStatus,
    listenForTaskListUpdates,
    listenForNotToDoListUpdates,
    listenForMediaToggleRecorded,
    loadAndSyncScreenTimeTrackingSetting,
    ensureDeviceName,
    getPairedDevices,
    syncWithDevice,
    setDeviceAutoSyncEnabled,
    attemptAutoSync,
    loadAndSyncPomodoroMode,
    savePomodoroMode,
    type PomodoroMode,
    type PairedDeviceInfo,
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
  // Which mechanism a break would actually use to pause media. null until the
  // first status read resolves -- same no-flash reasoning as the default
  // above, and load-bearing here: the Accessibility grant button must never
  // appear on the (default) MediaRemote path, which needs no permission.
  let mediaPauseBackend = $state<string | null>(null);
  // Distinguishes "the user chose the media-key path" from "this Mac can't
  // use MediaRemote at all", which need different explanations.
  let mediaRemoteAvailable = $state<boolean | null>(null);
  // Set once the user has actually pressed the grant button this session. A
  // request that comes back still-not-granted means macOS never showed a
  // prompt, which on this (unsigned, ad-hoc-signed) build almost always means
  // a stale TCC row from a previous build's code signature -- TCC keys grants
  // to the exact binary, so an update leaves an entry that the new build can
  // never satisfy and that suppresses the prompt. Pressing the button again
  // cannot fix that; removing the old entry can, so say so instead.
  let mediaKeyPermissionRequested = $state(false);
  let taskListContent = $state("");
  let notToDoContent = $state("");
  let pairedDevices = $state<PairedDeviceInfo[]>([]);
  let pairedDevicesLoaded = $state(false);
  let syncingDeviceId = $state<string | null>(null);
  let syncStatus = $state<"idle" | "success" | "error">("idle");
  let syncMessage = $state("");
  let autoSyncBusyId = $state<string | null>(null);
  let unlisten: UnlistenFn | null = null;
  let unlistenSnooze: UnlistenFn | null = null;
  let unlistenTasks: UnlistenFn | null = null;
  let unlistenNotToDo: UnlistenFn | null = null;
  let unlistenMediaToggle: UnlistenFn | null = null;
  let unlistenAutoSyncCompleted: UnlistenFn | null = null;
  let tickInterval: ReturnType<typeof setInterval> | null = null;
  let taskSaveTimer: ReturnType<typeof setTimeout> | null = null;
  let notToDoSaveTimer: ReturnType<typeof setTimeout> | null = null;

  let pomodoroMode = $state<PomodoroMode>("normal");
  let modeHelpOpen = $state(false);
  const slot = $derived(slotFor(now, pomodoroMode));
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

  async function handleModeSelect(event: Event) {
    const select = event.currentTarget as HTMLSelectElement;
    const previous = pomodoroMode;
    const next = select.value === "concentration" ? "concentration" : "normal";
    pomodoroMode = next;
    try {
      pomodoroMode = await savePomodoroMode(next);
    } catch (e) {
      pomodoroMode = previous;
      select.value = previous;
      logError(`failed to save pomodoro mode: ${e}`);
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

  // One round trip for both the effective backend and the permission state:
  // the two are only ever read together, and the backend can change at any
  // time from Settings, so it must be re-read rather than cached from boot.
  async function refreshMediaPauseStatus() {
    if (!isMacos) return;
    const status = await getMediaPauseStatus();
    mediaPauseBackend = status.backend;
    mediaRemoteAvailable = status.mediaRemoteAvailable;
    mediaKeyPermissionGranted = status.postEventGranted;
  }

  async function requestMediaKeyPermission() {
    await invoke("request_media_key_permission");
    mediaKeyPermissionRequested = true;
    await refreshMediaPauseStatus();
  }

  // Same fallback as settings/+page.svelte's deviceLabel -- device_name is
  // blank by default on platforms without hostname resolution (Android), so
  // fall back to a short device_id prefix rather than the full 32 characters.
  function deviceLabel(name: string, deviceId: string): string {
    return name || deviceId.slice(0, 8);
  }

  function formatLastSync(iso: string | null): string {
    if (!iso) return "Never";
    return new Date(iso).toLocaleString();
  }

  async function loadPairedDevices() {
    try {
      pairedDevices = await getPairedDevices();
    } catch {
      // Best-effort, same reasoning as settings/+page.svelte's own
      // loadPairedDevices -- leave the previous snapshot in place.
    } finally {
      pairedDevicesLoaded = true;
    }
  }

  async function runDeviceSync(device: PairedDeviceInfo) {
    syncingDeviceId = device.deviceId;
    syncStatus = "idle";
    const label = deviceLabel(device.name, device.deviceId);
    try {
      const result = await syncWithDevice(device.deviceId);
      const receivedTotal =
        result.reflectionCount +
        result.taskListCount +
        result.notToDoListCount +
        result.wellnessCheckCount +
        result.screenTimeSessionCount +
        result.bulkEditPresetCount;
      const mergedClause =
        result.mergedSlotCount > 0
          ? ` ${result.mergedSlotCount} reflection slot${result.mergedSlotCount === 1 ? "" : "s"} merged with existing entries.`
          : "";
      syncMessage = `Sent ${result.sentCount} row${result.sentCount === 1 ? "" : "s"}, received ${receivedTotal} row${receivedTotal === 1 ? "" : "s"} with ${label}.${mergedClause}`;
      syncStatus = "success";
      await loadPairedDevices();
    } catch (e) {
      syncMessage = e instanceof Error ? e.message : String(e);
      syncStatus = "error";
    } finally {
      syncingDeviceId = null;
    }
  }

  async function toggleDeviceAutoSync(device: PairedDeviceInfo) {
    autoSyncBusyId = device.deviceId;
    try {
      await setDeviceAutoSyncEnabled(device.deviceId, !device.autoSyncEnabled);
      await loadPairedDevices();
    } finally {
      autoSyncBusyId = null;
    }
  }

  // Granting happens in System Settings, so re-check when the user comes back.
  // Also a good moment to try an auto-sync (see p2p_sync.rs's maybe_auto_sync)
  // without waiting for the next break boundary -- rides an event the OS
  // already delivers, and the Rust side no-ops cheaply when nothing is
  // opted in or nothing is due yet.
  function onWindowFocus() {
    void refreshMediaPauseStatus();
    void attemptAutoSync();
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
    // Must push the saved fallback flag into Rust BEFORE reading the status
    // below: get_media_pause_status computes the effective backend from that
    // in-memory flag, which starts at its default until this runs.
    await loadAndSyncMacosMediaKeyFallbackSetting();
    await refreshMediaPauseStatus();
    await loadAndSyncBreakNotificationPersistentSetting();
    await loadAndSyncHideOverlayOnCallSetting();
    await loadAndSyncMediaToggleGuard();
    await loadAndSyncMacosHideMenuBarDockSetting();
    await loadAndSyncScreenTimeTrackingSetting();
    await ensureDeviceName();
    pomodoroMode = await loadAndSyncPomodoroMode();
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
    unlistenAutoSyncCompleted = await listen<string>("p2p-sync://auto-completed", () => {
      void loadPairedDevices();
    });
    // The screen-time batch listener lives in +layout.svelte, not here --
    // this route ("/") unmounts on every tab navigation, which would tear
    // the listener down and silently drop any batch Rust flushes while the
    // user is sitting on another tab. See +layout.svelte's onMount for why.
    void logInfo("main window: boot sequence complete, all listeners registered");
    // Boot is also a natural moment for the window-focus trigger's logic --
    // covers a fresh launch that lands well after the last break boundary.
    void attemptAutoSync();
  }

  onMount(async () => {
    window.addEventListener("focus", onWindowFocus);
    // Independent of bootMainWindow's sequential chain below -- nothing else
    // needs to block on the paired-device list resolving.
    void loadPairedDevices();
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
    unlistenAutoSyncCompleted?.();
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
      <option value="on">Pomodoro: On</option>
      {#each SNOOZE_MINUTES_OPTIONS as minutes (minutes)}
        <option value={String(minutes)}>{snoozeOptionLabel(minutes)}</option>
      {/each}
      <option value="off">Pomodoro: Off</option>
    </select>
    {#if snoozeInfo}
      <p class="hint">{snoozeResumeLabel}</p>
    {/if}

    <div class="mode-row">
      <!-- <label class="mode-label" for="pomodoro-mode-select">Pomodoro Mode</label> -->
      <select id="pomodoro-mode-select" class="pomodoro-select" value={pomodoroMode} onchange={handleModeSelect}>
        <option value="normal">Pomodoro Mode: Normal</option>
        <option value="concentration">Pomodoro Mode: Concentration</option>
      </select>
      <button
        type="button"
        class="info-btn"
        aria-label="About Pomodoro modes"
        aria-expanded={modeHelpOpen}
        onclick={() => (modeHelpOpen = !modeHelpOpen)}
      >i</button>
    </div>
    {#if modeHelpOpen}
      <p class="hint mode-help">
        <strong>Normal:</strong> work :00&ndash;:25 and :30&ndash;:55 each hour; breaks :25&ndash;:30 and :55&ndash;:00.<br/>
        <strong>Concentration:</strong> work :00&ndash;:25 &rarr; 30 sec break &rarr; work :25:30&ndash;:50 &rarr; 10 min break (:50&ndash;:00).
      </p>
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
      <!-- Only ever shown on the media-key path. The default MediaRemote path
           needs no permission at all, so asking for one there would be a
           prompt for something the app never uses. `mediaPauseBackend` stays
           null until the status read resolves, so nothing flashes first. -->
      {#if isMacos && mediaPauseOnBreakEnabled && mediaPauseBackend === "media_key" && !mediaKeyPermissionGranted}
        <p class="hint">
          {mediaRemoteAvailable === false
            ? "This Mac can't use the permission-free method, so Reflectodoro needs Accessibility access to pause media."
            : "You've switched on the media-key method, which needs Accessibility access to pause media."}
        </p>
        <button class="toggle permission-button" onclick={requestMediaKeyPermission}>
          Grant Accessibility access…
        </button>
        {#if mediaKeyPermissionRequested}
          <p class="hint">
            Still not granted — macOS didn't show a prompt. That usually means an
            old Reflectodoro entry is stuck in System Settings → Privacy &amp;
            Security → Accessibility: select Reflectodoro there, remove it with
            the “−” button, then press Grant again. If it still won't take, run
            <code>tccutil reset PostEvent com.reflectodoro.app</code> in Terminal
            and reopen Reflectodoro.
          </p>
        {/if}
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

  {#if pairedDevicesLoaded && pairedDevices.length > 0}
    <section class="card paired-devices-card">
      <h2>Paired devices</h2>
      <ul class="paired-device-list">
        {#each pairedDevices as device (device.deviceId)}
          <li>
            <span
              class="paired-device-status"
              class:online={device.online}
              title={device.online ? "Online" : "Offline"}
            ></span>
            <span class="paired-device-name"
              >{deviceLabel(device.name, device.deviceId)} <span class="hint">({device.platform})</span></span
            >
            <span class="hint">Last synced: {formatLastSync(device.lastSyncAt)}</span>
            <label class="checkbox auto-sync-checkbox">
              <input
                type="checkbox"
                checked={device.autoSyncEnabled}
                disabled={autoSyncBusyId === device.deviceId}
                onchange={() => toggleDeviceAutoSync(device)}
              />
              Auto-sync
            </label>
            <button
              type="button"
              class="toggle sync-button"
              onclick={() => runDeviceSync(device)}
              disabled={syncingDeviceId !== null || !device.online}
              title={device.online ? "" : "Device is offline"}
            >
              {syncingDeviceId === device.deviceId ? "Syncing…" : "Sync"}
            </button>
          </li>
        {/each}
      </ul>
      {#if syncStatus === "success"}
        <p class="hint saved">{syncMessage}</p>
      {:else if syncStatus === "error"}
        <p class="hint error">{syncMessage}</p>
      {/if}
    </section>
  {/if}
</div>

<style>
  .page {
    padding: 24px;
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 20px;
    max-width: 1200px;
    margin: 0 auto;
  }

  /* Large windows: scale the type/padding up alongside the wider cap so the
     cards fill the space instead of floating in it. min-width only, so small
     windows and Android keep the base sizes (the max-width: 600px block below
     still wins there). */
  @media (min-width: 1200px) {
    .page {
      padding: 32px;
      gap: 28px;
    }

    .card {
      padding: 32px;
    }

    .label,
    .sub,
    .hint,
    .permission-button,
    .sync-button {
      font-size: 14px;
    }

    .big {
      font-size: 64px;
    }

    h2 {
      font-size: 18px;
    }

    .toggle,
    .pomodoro-select {
      font-size: 16px;
      padding: 12px 20px;
    }

    textarea {
      font-size: 16px;
      padding: 12px 14px;
    }

    .paired-device-name {
      font-size: 16px;
    }

    label.checkbox {
      font-size: 15px;
    }
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
    width: fit-content;
    max-width: 100%;
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
    /* Size to the selected option's text, not the widest option in the list. */
    field-sizing: content;
    width: fit-content;
    max-width: 100%;
  }

  .pomodoro-select.off {
    background: var(--surface-2);
    color: var(--text-dim);
  }

  .mode-row {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    margin-top: 14px;
    flex-wrap: wrap;
  }

  .mode-label {
    font-size: 14px;
    font-weight: 500;
    color: var(--text-dim);
  }

  .info-btn {
    width: 22px;
    height: 22px;
    padding: 0;
    border-radius: 50%;
    border: 1px solid var(--text-dim);
    background: transparent;
    color: var(--text-dim);
    font-size: 12px;
    font-style: italic;
    font-weight: 600;
    line-height: 1;
    cursor: pointer;
  }

  .mode-help {
    text-align: left;
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

  .saved {
    color: #3a9d5d;
  }

  .error {
    color: #d9534f;
  }

  .paired-device-list {
    list-style: none;
    margin: 12px 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .paired-device-list li {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }

  .paired-device-status {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--border);
    flex-shrink: 0;
  }

  .paired-device-status.online {
    background: #3a9d5d;
  }

  .paired-device-name {
    font-size: 14px;
    flex: 1;
    min-width: 120px;
  }

  label.checkbox {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 8px;
    font-size: 13px;
    color: var(--text);
  }

  label.checkbox input[type="checkbox"] {
    width: 16px;
    height: 16px;
  }

  .sync-button {
    padding: 6px 12px;
    font-size: 12px;
  }
</style>
