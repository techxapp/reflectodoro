<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { save as saveDialog, open as openDialog } from "@tauri-apps/plugin-dialog";
  import {
    getBreakitSettings,
    saveBreakitSettings,
    exportAllData,
    parseAndValidateExport,
    importData,
    readTextFile,
    writeTextFile,
    localDateStamp,
    getAutostartEnabled,
    setAutostartEnabled,
    getWellnessTextExclusions,
    saveWellnessTextExclusions,
    getForceCloseShortcutEnabled,
    saveForceCloseShortcutEnabled,
    getMediaPauseOnBreakEnabled,
    saveMediaPauseOnBreakEnabled,
    getBreakNotificationPersistentEnabled,
    saveBreakNotificationPersistentEnabled,
    getOverlayAutoCloseMinutes,
    saveOverlayAutoCloseMinutes,
    getCheckinAutoCloseMinutes,
    saveCheckinAutoCloseMinutes,
    type BreakitSettings,
    type ImportMode,
  } from "$lib/db";

  let length = $state(15);
  let includeSpecial = $state(false);
  let saved = $state(false);
  let loaded = $state(false);

  let overlayAutoCloseMinutes = $state(5);
  let overlayAutoCloseLoaded = $state(false);
  let overlayAutoCloseSaved = $state(false);

  let checkinAutoCloseMinutes = $state(5);
  let checkinAutoCloseLoaded = $state(false);
  let checkinAutoCloseSaved = $state(false);

  let autostartEnabled = $state(false);
  let autostartLoaded = $state(false);
  let autostartBusy = $state(false);
  let autostartError = $state("");

  let wellnessExclusions = $state("");
  let wellnessExclusionsLoaded = $state(false);
  let wellnessExclusionsSaved = $state(false);

  let forceCloseShortcutEnabled = $state(true);
  let forceCloseShortcutLoaded = $state(false);
  let forceCloseShortcutBusy = $state(false);
  // Windows/Linux default; overwritten on macOS once current_os resolves.
  let forceCloseShortcutLabel = $state("Ctrl+Alt+Shift+F12");

  let mediaPauseOnBreakEnabled = $state(true);
  let mediaPauseOnBreakLoaded = $state(false);
  let mediaPauseOnBreakBusy = $state(false);

  let isAndroid = $state(false);
  let breakNotificationPersistentEnabled = $state(true);
  let breakNotificationPersistentLoaded = $state(false);
  let breakNotificationPersistentBusy = $state(false);

  let overlayGranted = $state(false);
  let overlayChecked = $state(false);

  let includeSettingsInTransfer = $state(true);

  let exportStatus = $state<"idle" | "success" | "error">("idle");
  let exportError = $state("");
  let importPath = $state<string | null>(null);
  let importFileName = $state("");
  let importBusy = $state(false);
  let importStatus = $state<"idle" | "success" | "error">("idle");
  let importMessage = $state("");

  async function loadBreakitSettings() {
    const settings: BreakitSettings = await getBreakitSettings();
    length = settings.length;
    includeSpecial = settings.includeSpecial;
    loaded = true;
  }

  onMount(loadBreakitSettings);

  onMount(async () => {
    autostartEnabled = await getAutostartEnabled();
    autostartLoaded = true;
  });

  onMount(async () => {
    wellnessExclusions = await getWellnessTextExclusions();
    wellnessExclusionsLoaded = true;
  });

  onMount(async () => {
    forceCloseShortcutEnabled = await getForceCloseShortcutEnabled();
    forceCloseShortcutLoaded = true;
  });

  onMount(async () => {
    const os = await invoke<string>("current_os");
    if (os === "macos") forceCloseShortcutLabel = "Cmd+Option+Shift+F12";
    isAndroid = os === "android";
  });

  onMount(async () => {
    mediaPauseOnBreakEnabled = await getMediaPauseOnBreakEnabled();
    mediaPauseOnBreakLoaded = true;
  });

  onMount(async () => {
    breakNotificationPersistentEnabled = await getBreakNotificationPersistentEnabled();
    breakNotificationPersistentLoaded = true;
  });

  async function refreshOverlayPermission() {
    overlayGranted = await invoke<boolean>("can_draw_overlays");
    overlayChecked = true;
  }

  async function openOverlaySettings() {
    await invoke("request_draw_overlays_permission");
  }

  // Re-checks when the user comes back from the system settings screen --
  // same pattern as onboarding's exact-alarm re-check, needed since that
  // screen's return doesn't reliably resolve any promise here.
  function onOverlayVisibilityChange() {
    if (document.visibilityState === "visible") void refreshOverlayPermission();
  }

  onMount(() => {
    void refreshOverlayPermission();
    document.addEventListener("visibilitychange", onOverlayVisibilityChange);
  });

  onDestroy(() => {
    document.removeEventListener("visibilitychange", onOverlayVisibilityChange);
  });

  onMount(async () => {
    overlayAutoCloseMinutes = await getOverlayAutoCloseMinutes();
    overlayAutoCloseLoaded = true;
  });

  onMount(async () => {
    checkinAutoCloseMinutes = await getCheckinAutoCloseMinutes();
    checkinAutoCloseLoaded = true;
  });

  // 150px-wide slide track. Deliberately not a click-to-toggle control: the
  // thumb only flips state when dragged past the midpoint (see onSliderUp) --
  // a plain click/tap that doesn't move the pointer leaves dragOffset equal
  // to dragStartOffset, so nothing changes. This is a physical safety-net
  // toggle, so requiring an intentional slide guards against flipping it by
  // an accidental click.
  const SLIDER_TRACK_WIDTH = 150;
  const SLIDER_THUMB_WIDTH = 70;
  const SLIDER_PADDING = 4;
  const SLIDER_MAX_OFFSET = SLIDER_TRACK_WIDTH - SLIDER_THUMB_WIDTH - SLIDER_PADDING * 2;

  let sliderDragging = $state(false);
  let sliderDragOffset = $state(0);
  let sliderDragStartX = 0;
  let sliderDragStartOffset = 0;

  const sliderOffset = $derived(
    sliderDragging ? sliderDragOffset : forceCloseShortcutEnabled ? SLIDER_MAX_OFFSET : 0,
  );

  async function commitForceCloseShortcut(next: boolean) {
    if (next === forceCloseShortcutEnabled) return;
    forceCloseShortcutBusy = true;
    try {
      await saveForceCloseShortcutEnabled(next);
      forceCloseShortcutEnabled = next;
    } finally {
      forceCloseShortcutBusy = false;
    }
  }

  function onSliderPointerDown(e: PointerEvent) {
    if (forceCloseShortcutBusy) return;
    sliderDragging = true;
    sliderDragStartX = e.clientX;
    sliderDragStartOffset = forceCloseShortcutEnabled ? SLIDER_MAX_OFFSET : 0;
    sliderDragOffset = sliderDragStartOffset;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }

  function onSliderPointerMove(e: PointerEvent) {
    if (!sliderDragging) return;
    const delta = e.clientX - sliderDragStartX;
    sliderDragOffset = Math.min(SLIDER_MAX_OFFSET, Math.max(0, sliderDragStartOffset + delta));
  }

  async function onSliderPointerUp() {
    if (!sliderDragging) return;
    sliderDragging = false;
    await commitForceCloseShortcut(sliderDragOffset > SLIDER_MAX_OFFSET / 2);
  }

  function onSliderKeydown(e: KeyboardEvent) {
    if (e.key !== "Enter" && e.key !== " ") return;
    e.preventDefault();
    void commitForceCloseShortcut(!forceCloseShortcutEnabled);
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

  async function toggleBreakNotificationPersistent() {
    const next = !breakNotificationPersistentEnabled;
    breakNotificationPersistentBusy = true;
    try {
      await saveBreakNotificationPersistentEnabled(next);
      breakNotificationPersistentEnabled = next;
    } finally {
      breakNotificationPersistentBusy = false;
    }
  }

  async function saveWellnessExclusions(e: Event) {
    e.preventDefault();
    await saveWellnessTextExclusions(wellnessExclusions);
    wellnessExclusionsSaved = true;
    setTimeout(() => (wellnessExclusionsSaved = false), 2000);
  }

  async function toggleAutostart() {
    const next = !autostartEnabled;
    autostartBusy = true;
    autostartError = "";
    try {
      await setAutostartEnabled(next);
      autostartEnabled = next;
    } catch (e) {
      autostartError = e instanceof Error ? e.message : String(e);
    } finally {
      autostartBusy = false;
    }
  }

  async function save(e: Event) {
    e.preventDefault();
    await saveBreakitSettings({ length: Math.min(64, Math.max(4, length)), includeSpecial });
    saved = true;
    setTimeout(() => (saved = false), 2000);
  }

  async function saveOverlayAutoClose(e: Event) {
    e.preventDefault();
    overlayAutoCloseMinutes = Math.max(1, overlayAutoCloseMinutes);
    await saveOverlayAutoCloseMinutes(overlayAutoCloseMinutes);
    overlayAutoCloseSaved = true;
    setTimeout(() => (overlayAutoCloseSaved = false), 2000);
  }

  async function saveCheckinAutoClose(e: Event) {
    e.preventDefault();
    checkinAutoCloseMinutes = Math.max(1, checkinAutoCloseMinutes);
    await saveCheckinAutoCloseMinutes(checkinAutoCloseMinutes);
    checkinAutoCloseSaved = true;
    setTimeout(() => (checkinAutoCloseSaved = false), 2000);
  }

  async function exportData() {
    exportStatus = "idle";
    try {
      const path = await saveDialog({
        defaultPath: `reflectodoro-export-${localDateStamp()}.json`,
        filters: [{ name: "JSON", extensions: ["json"] }],
      });
      if (!path) return;
      const payload = await exportAllData(includeSettingsInTransfer);
      await writeTextFile(path, JSON.stringify(payload, null, 2));
      exportStatus = "success";
      setTimeout(() => (exportStatus = "idle"), 3000);
    } catch (e) {
      exportError = e instanceof Error ? e.message : String(e);
      exportStatus = "error";
    }
  }

  async function chooseImportFile() {
    importStatus = "idle";
    const path = await openDialog({
      multiple: false,
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (!path || Array.isArray(path)) return;
    importPath = path;
    importFileName = path.split(/[\\/]/).pop() ?? path;
  }

  async function runImport(mode: ImportMode) {
    if (!importPath) return;
    const settingsClause = includeSettingsInTransfer
      ? "reflections, task lists, and settings"
      : "reflections and task lists (settings will be left untouched)";
    const confirmed =
      mode === "replace"
        ? confirm(
            `This will permanently delete all existing ${settingsClause} and replace them with the contents of "${importFileName}". This cannot be undone. Continue?`,
          )
        : confirm(
            `Import "${importFileName}" and merge it into your existing data? Imported values win on conflict; nothing existing is deleted.`,
          );
    if (!confirmed) return;

    importBusy = true;
    importStatus = "idle";
    try {
      const raw = await readTextFile(importPath);
      const payload = parseAndValidateExport(raw);
      const result = await importData(payload, mode, includeSettingsInTransfer);
      await loadBreakitSettings();
      importMessage = `Imported ${result.reflectionCount} reflection${result.reflectionCount === 1 ? "" : "s"}, ${result.taskListCount} task list${result.taskListCount === 1 ? "" : "s"}, ${result.notToDoListCount} not-to-do list${result.notToDoListCount === 1 ? "" : "s"}, ${result.settingCount} setting${result.settingCount === 1 ? "" : "s"}, ${result.wellnessCheckCount} wellness check-in${result.wellnessCheckCount === 1 ? "" : "s"}.`;
      importStatus = "success";
      importPath = null;
      importFileName = "";
    } catch (e) {
      importMessage = e instanceof Error ? e.message : String(e);
      importStatus = "error";
    } finally {
      importBusy = false;
    }
  }
</script>

<div class="page">
  <section class="card">
    <h2>Break overlay</h2>
    <p class="hint">
      Typing a random code exactly is the early-exit alternative &mdash;
      it still requires the reflection ("what did I do?") too.
      If neither happens,
      the overlay auto-closes on its own after the timeout below.
    </p>

    {#if loaded}
      <form onsubmit={save}>
        <label>
          Code length
          <input type="number" min="4" max="64" bind:value={length} />
        </label>
        <label class="checkbox">
          <input type="checkbox" bind:checked={includeSpecial} />
          Include special characters
        </label>
        <button type="submit">Save</button>
        {#if saved}
          <span class="hint saved">Saved</span>
        {/if}
      </form>
    {/if}

    {#if overlayAutoCloseLoaded}
      <form onsubmit={saveOverlayAutoClose}>
        <label>
          Auto-close after (minutes past break end)
          <input type="number" min="1" bind:value={overlayAutoCloseMinutes} />
        </label>
        <button type="submit">Save</button>
        {#if overlayAutoCloseSaved}
          <span class="hint saved">Saved</span>
        {/if}
      </form>
    {/if}

    {#if mediaPauseOnBreakLoaded}
      <div class="data-row">
        <label class="checkbox">
          <input
            type="checkbox"
            checked={mediaPauseOnBreakEnabled}
            disabled={mediaPauseOnBreakBusy}
            onchange={toggleMediaPauseOnBreak}
          />
          Pause playing media (video/music) when a break starts
        </label>
      </div>
    {/if}

    {#if isAndroid && overlayChecked}
      <div class="data-row">
        <span>Break screen (draw over other apps): {overlayGranted ? "Granted" : "Not granted"}</span>
        {#if !overlayGranted}
          <button type="button" onclick={openOverlaySettings}>Open settings&hellip;</button>
        {/if}
      </div>
      <p class="hint">
        Recommended. Without it, a break can only reach you via a notification instead of
        appearing directly over whatever else you're doing.
      </p>
    {/if}

    {#if isAndroid && breakNotificationPersistentLoaded}
      <div class="data-row">
        <label class="checkbox">
          <input
            type="checkbox"
            checked={breakNotificationPersistentEnabled}
            disabled={breakNotificationPersistentBusy}
            onchange={toggleBreakNotificationPersistent}
          />
          Make the break notification non-dismissible until the break is resolved
        </label>
      </div>
      <p class="hint">
        Only affects a break that starts while you're using another app &mdash; it can't wake or
        take over a locked screen.
      </p>
    {/if}
  </section>

  <section class="card">
    <h2>Session schedule</h2>
    <p class="hint">
      Fixed for now (Not configurable in this build) <br/> 
      Work runs :00&ndash;:25 and :30&ndash;:55 each hour <br/> 
      Breaks run :25&ndash;:30 and :55&ndash;:00
    </p>
  </section>

  <section class="card">
    <h2>Wellness check-in</h2>
    <p class="hint">
      Comma-separated list of check-in items (Relaxed eyes, Exercise, Drank water, Washroom) that
      should stay quiet -- no "Let's Try Next Time :)" nudge when switched off.
    </p>

    {#if wellnessExclusionsLoaded}
      <form onsubmit={saveWellnessExclusions}>
        <label class="grow">
          Excluded items
          <input type="text" bind:value={wellnessExclusions} placeholder="e.g. Washroom, Exercise" />
        </label>
        <button type="submit">Save</button>
        {#if wellnessExclusionsSaved}
          <span class="hint saved">Saved</span>
        {/if}
      </form>
    {/if}

    {#if checkinAutoCloseLoaded}
      <form onsubmit={saveCheckinAutoClose}>
        <label>
          Auto-close after (minutes, if untouched)
          <input type="number" min="1" bind:value={checkinAutoCloseMinutes} />
        </label>
        <button type="submit">Save</button>
        {#if checkinAutoCloseSaved}
          <span class="hint saved">Saved</span>
        {/if}
      </form>
    {/if}
  </section>


  {#if !isAndroid}
  <section class="card">
    <h2>If the overlay ever gets stuck</h2>
    <ul class="hint">
      <li>Press {forceCloseShortcutLabel} to force-close the overlay.</li>
    </ul>

    {#if forceCloseShortcutLoaded}
      <div class="data-row slider-row">
        <div
          class="slide-track"
          class:busy={forceCloseShortcutBusy}
          role="switch"
          aria-checked={forceCloseShortcutEnabled}
          aria-label={`Enable ${forceCloseShortcutLabel} force-close shortcut`}
          tabindex="0"
          onpointerdown={onSliderPointerDown}
          onpointermove={onSliderPointerMove}
          onpointerup={onSliderPointerUp}
          onpointercancel={onSliderPointerUp}
          onkeydown={onSliderKeydown}
        >
          <span class="slide-track-label off">Disabled</span>
          <span class="slide-track-label on">Enabled</span>
          <div
            class="slide-thumb"
            class:accent={sliderOffset > SLIDER_MAX_OFFSET / 2}
            style={`transform: translateX(${sliderOffset}px)`}
          >
            {sliderOffset > SLIDER_MAX_OFFSET / 2 ? "Enabled" : "Disabled"}
          </div>
        </div>
        <span class="hint">Slide to enable/disable the force-close shortcut</span>
      </div>

      {#if forceCloseShortcutEnabled}
        <p class="hint warning">
          Only turn this off once overlay behavior has been confirmed good across log off/log on,
          system start, and restart &mdash; it's recommended to keep it enabled for at least a week
          first. It's a safety net, not something you'll trigger day to day.
        </p>
      {/if}
    {/if}
  </section>
  {/if}


  <section class="card">
    <h2>Startup</h2>
    <p class="hint">Launch Reflectodoro automatically when you log in.</p>

    {#if autostartLoaded}
      <div class="data-row">
        <label class="checkbox">
          <input
            type="checkbox"
            checked={autostartEnabled}
            disabled={autostartBusy}
            onchange={toggleAutostart}
          />
          Start automatically on login
        </label>
      </div>
      <p class="hint warning">Recommended to keep it off atleast for a week, It can help recover from bugs/screen overlay getting stuck.</p>
      {#if autostartError}
        <p class="hint error">{autostartError}</p>
      {/if}
    {/if}
  </section>

  

  <section class="card">
    <h2>Data</h2>
    <p class="hint">
      Export all reflections, task lists, and settings to a JSON file, or import one back in.
    </p>

    <div class="data-row">
      <label class="checkbox">
        <input type="checkbox" bind:checked={includeSettingsInTransfer} />
        Include settings (breakit code, timeouts, etc.) in export/import
      </label>
    </div>

    <div class="data-row">
      <button type="button" onclick={exportData}>Export data&hellip;</button>
      {#if exportStatus === "success"}
        <span class="hint saved">Exported</span>
      {:else if exportStatus === "error"}
        <span class="hint error">{exportError}</span>
      {/if}
    </div>

    <div class="data-row">
      <button type="button" onclick={chooseImportFile}>Import Data&hellip;</button>
      {#if importFileName}
        <span class="hint">{importFileName}</span>
      {/if}
    </div>

    {#if importPath}
      <div class="data-row">
        <button type="button" disabled={importBusy} onclick={() => runImport("merge")}>
          Merge (imported wins)
        </button>
        <button type="button" class="danger" disabled={importBusy} onclick={() => runImport("replace")}>
          Replace all data
        </button>
      </div>
    {/if}

    {#if importStatus === "success"}
      <p class="hint saved">{importMessage}</p>
    {:else if importStatus === "error"}
      <p class="hint error">{importMessage}</p>
    {/if}
  </section>
</div>

<style>
  .page {
    padding: 24px;
    display: flex;
    flex-direction: column;
    gap: 20px;
    max-width: 600px;
    margin: 0 auto;
  }

  .card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 14px;
    padding: 24px;
  }

  h2 {
    margin: 0 0 10px;
    font-size: 15px;
  }

  .hint {
    color: var(--text-dim);
    font-size: 13px;
    line-height: 1.5;
  }

  ul.hint {
    padding-left: 18px;
    margin: 0;
  }

  form {
    display: flex;
    align-items: flex-end;
    gap: 16px;
    margin-top: 16px;
    flex-wrap: wrap;
  }

  label {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 12px;
    color: var(--text-dim);
  }

  label.grow {
    flex: 1;
    min-width: 220px;
  }

  input {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 8px;
    color: inherit;
    padding: 8px 10px;
    font-size: 14px;
  }

  input[type="text"] {
    width: 100%;
  }

  input[type="number"] {
    width: 80px;
  }

  label.checkbox {
    flex-direction: row;
    align-items: center;
    gap: 8px;
    font-size: 13px;
    color: var(--text);
  }

  input[type="checkbox"] {
    width: 16px;
    height: 16px;
  }

  button {
    background: var(--accent);
    color: white;
    border: none;
    border-radius: 8px;
    padding: 9px 18px;
    font-size: 14px;
  }

  .saved {
    color: #3a9d5d;
  }

  .error {
    color: #d9534f;
  }

  .warning {
    color: #b8860b;
    margin-top: 12px;
  }

  .slider-row {
    flex-direction: column;
    align-items: flex-start;
    gap: 8px;
  }

  .slide-track {
    position: relative;
    width: 150px;
    height: 34px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 999px;
    touch-action: none;
    cursor: grab;
    user-select: none;
  }

  .slide-track:active {
    cursor: grabbing;
  }

  .slide-track.busy {
    opacity: 0.6;
    pointer-events: none;
  }

  .slide-track:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .slide-track-label {
    position: absolute;
    top: 50%;
    transform: translateY(-50%);
    font-size: 11px;
    color: var(--text-dim);
    pointer-events: none;
  }

  .slide-track-label.off {
    left: 12px;
  }

  .slide-track-label.on {
    right: 12px;
  }

  .slide-thumb {
    position: absolute;
    top: 3px;
    left: 3px;
    width: 70px;
    height: 26px;
    border-radius: 999px;
    background: var(--surface);
    border: 1px solid var(--border);
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 11px;
    font-weight: 600;
    color: var(--text);
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.2);
  }

  .slide-thumb.accent {
    background: var(--accent);
    color: white;
    border-color: var(--accent);
  }

  .data-row {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-top: 16px;
  }

  button.danger {
    background: #d9534f;
  }
</style>
