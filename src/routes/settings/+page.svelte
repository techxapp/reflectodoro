<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { save as saveDialog, open as openDialog } from "@tauri-apps/plugin-dialog";
  import { isPermissionGranted, requestPermission } from "@tauri-apps/plugin-notification";
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
    getBreakNotificationPersistentEnabled,
    saveBreakNotificationPersistentEnabled,
    getOverlayAutoCloseMinutes,
    saveOverlayAutoCloseMinutes,
    getCheckinAutoCloseMinutes,
    saveCheckinAutoCloseMinutes,
    getMacosHideMenuBarDockEnabled,
    saveMacosHideMenuBarDockEnabled,
    getScreenTimeTrackingEnabled,
    saveScreenTimeTrackingEnabled,
    getScreenTimeAppThresholdMinutes,
    saveScreenTimeAppThresholdMinutes,
    getDeviceName,
    saveDeviceName,
    startPairing,
    cancelPairing,
    browsePairingCandidates,
    confirmPairing,
    getPairedDevices,
    browseOnlinePairedDevices,
    forgetPairedDevice,
    syncWithDevice,
    type BreakitSettings,
    type ImportMode,
    type DiscoveredDevice,
    type PairedDeviceInfo,
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

  let isAndroid = $state(false);
  let breakNotificationPersistentEnabled = $state(true);
  let breakNotificationPersistentLoaded = $state(false);
  let breakNotificationPersistentBusy = $state(false);

  let isMacos = $state(false);
  let isWindows = $state(false);
  // Gates the "not captured on this platform yet" screen-time hint on
  // current_os having actually resolved -- without it that hint flashes on
  // Windows too, since isWindows starts false.
  let osResolved = $state(false);
  let macosHideMenuBarDockEnabled = $state(false);
  let macosHideMenuBarDockLoaded = $state(false);
  let macosHideMenuBarDockBusy = $state(false);

  let screenTimeTrackingEnabled = $state(true);
  let screenTimeTrackingLoaded = $state(false);
  let screenTimeTrackingBusy = $state(false);

  let screenTimeAppThresholdMinutes = $state(5);
  let screenTimeAppThresholdLoaded = $state(false);
  let screenTimeAppThresholdSaved = $state(false);

  let deviceName = $state("");
  let deviceNameLoaded = $state(false);
  let deviceNameSaved = $state(false);

  let overlayGranted = $state(false);
  let overlayChecked = $state(false);

  let exactAlarmGranted = $state(false);
  let exactAlarmChecked = $state(false);

  let notificationGranted = $state(false);
  let notificationChecked = $state(false);

  let usageStatsGranted = $state(false);
  let usageStatsChecked = $state(false);

  let includeSettingsInTransfer = $state(true);

  let exportStatus = $state<"idle" | "success" | "error">("idle");
  let exportError = $state("");
  let importPath = $state<string | null>(null);
  let importFileName = $state("");
  let importBusy = $state(false);
  let importStatus = $state<"idle" | "success" | "error">("idle");
  let importMessage = $state("");

  // --- P2P LAN device pairing/sync ---------------------------------------

  let pairedDevices = $state<PairedDeviceInfo[]>([]);
  let pairedDevicesLoaded = $state(false);
  let pairedDevicesBusy = $state(false);

  let pairingOpen = $state(false);
  let hostPin = $state<string | null>(null);
  let hostPinBusy = $state(false);

  let pairingCandidates = $state<DiscoveredDevice[]>([]);
  let candidatesBusy = $state(false);
  let selectedCandidateId = $state("");
  let joinPin = $state("");
  let joinBusy = $state(false);
  let joinError = $state("");

  let syncDeviceId = $state("");
  let syncBusy = $state(false);
  let syncStatus = $state<"idle" | "success" | "error">("idle");
  let syncMessage = $state("");

  async function loadPairedDevices() {
    pairedDevicesBusy = true;
    try {
      pairedDevices = await getPairedDevices();
    } catch {
      // Best-effort: LAN discovery being unavailable (no network interface,
      // mDNS daemon failed to start on this device) shouldn't blank out the
      // paired-device list itself, just leave the previous snapshot in place.
    } finally {
      pairedDevicesBusy = false;
      pairedDevicesLoaded = true;
    }
  }

  onMount(loadPairedDevices);

  async function togglePairingPanel() {
    pairingOpen = !pairingOpen;
    if (!pairingOpen) {
      if (hostPin) await cancelPairing().catch(() => {});
      hostPin = null;
      pairingCandidates = [];
      selectedCandidateId = "";
      joinPin = "";
      joinError = "";
    } else {
      await refreshPairingCandidates();
    }
  }

  async function showPairingPin() {
    hostPinBusy = true;
    try {
      hostPin = await startPairing();
    } catch (e) {
      joinError = e instanceof Error ? e.message : String(e);
    } finally {
      hostPinBusy = false;
    }
  }

  async function refreshPairingCandidates() {
    candidatesBusy = true;
    try {
      pairingCandidates = await browsePairingCandidates();
    } catch {
      pairingCandidates = [];
    } finally {
      candidatesBusy = false;
    }
  }

  async function submitJoinPairing(e: Event) {
    e.preventDefault();
    if (!selectedCandidateId || !joinPin.trim()) return;
    joinBusy = true;
    joinError = "";
    try {
      await confirmPairing(selectedCandidateId, joinPin.trim());
      selectedCandidateId = "";
      joinPin = "";
      pairingOpen = false;
      hostPin = null;
      await loadPairedDevices();
    } catch (e) {
      joinError = e instanceof Error ? e.message : String(e);
    } finally {
      joinBusy = false;
    }
  }

  async function removePairedDevice(device: PairedDeviceInfo) {
    if (!confirm(`Forget "${deviceLabel(device.name, device.deviceId)}"? You'll need to pair again (a new PIN exchange) to sync with it.`))
      return;
    await forgetPairedDevice(device.deviceId);
    if (syncDeviceId === device.deviceId) syncDeviceId = "";
    await loadPairedDevices();
  }

  async function refreshOnlineDevices() {
    pairedDevicesBusy = true;
    try {
      const online = await browseOnlinePairedDevices();
      const onlineIds = new Set(online.map((d) => d.deviceId));
      pairedDevices = pairedDevices.map((d) => ({ ...d, online: onlineIds.has(d.deviceId) }));
    } catch {
      // leave the existing snapshot as-is
    } finally {
      pairedDevicesBusy = false;
    }
  }

  function formatLastSync(iso: string | null): string {
    if (!iso) return "Never";
    return new Date(iso).toLocaleString();
  }

  /** Label for a paired/candidate device when it has no device_name set --
   * most commonly Android, where hostname resolution isn't implemented yet
   * (get_hostname returns "" there), so device_name is empty by default.
   * Falling back to the raw 32-character device_id broke this page's
   * layout; an 8-character prefix is still enough to tell two blank-named
   * devices apart without it. */
  function deviceLabel(name: string, deviceId: string): string {
    return name || deviceId.slice(0, 8);
  }

  async function runDeviceSync() {
    if (!syncDeviceId) return;
    syncBusy = true;
    syncStatus = "idle";
    try {
      const result = await syncWithDevice(syncDeviceId);
      const receivedTotal =
        result.reflectionCount +
        result.taskListCount +
        result.notToDoListCount +
        result.wellnessCheckCount +
        result.screenTimeSessionCount;
      const mergedClause =
        result.mergedSlotCount > 0
          ? ` ${result.mergedSlotCount} reflection slot${result.mergedSlotCount === 1 ? "" : "s"} merged with existing entries.`
          : "";
      // Reports both directions explicitly -- a sync that only moved data
      // outward (this device had a new change, the peer had nothing new to
      // send back) has receivedTotal = 0, which used to read as "nothing
      // happened" even though the change was sent and applied on the peer.
      syncMessage = `Sent ${result.sentCount} row${result.sentCount === 1 ? "" : "s"}, received ${receivedTotal} row${receivedTotal === 1 ? "" : "s"} with the selected device.${mergedClause}`;
      syncStatus = "success";
      await loadPairedDevices();
    } catch (e) {
      syncMessage = e instanceof Error ? e.message : String(e);
      syncStatus = "error";
    } finally {
      syncBusy = false;
    }
  }

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
    isMacos = os === "macos";
    isWindows = os === "windows";
    osResolved = true;
  });

  onMount(async () => {
    macosHideMenuBarDockEnabled = await getMacosHideMenuBarDockEnabled();
    macosHideMenuBarDockLoaded = true;
  });

  onMount(async () => {
    breakNotificationPersistentEnabled = await getBreakNotificationPersistentEnabled();
    breakNotificationPersistentLoaded = true;
  });

  onMount(async () => {
    screenTimeTrackingEnabled = await getScreenTimeTrackingEnabled();
    screenTimeTrackingLoaded = true;
  });

  onMount(async () => {
    screenTimeAppThresholdMinutes = await getScreenTimeAppThresholdMinutes();
    screenTimeAppThresholdLoaded = true;
  });

  onMount(async () => {
    deviceName = await getDeviceName();
    deviceNameLoaded = true;
  });

  async function refreshOverlayPermission() {
    overlayGranted = await invoke<boolean>("can_draw_overlays");
    overlayChecked = true;
  }

  async function openOverlaySettings() {
    await invoke("request_draw_overlays_permission");
  }

  async function refreshExactAlarmPermission() {
    exactAlarmGranted = await invoke<boolean>("can_schedule_exact_alarms");
    exactAlarmChecked = true;
  }

  async function openExactAlarmSettings() {
    await invoke("request_schedule_exact_alarm_permission");
  }

  /** POST_NOTIFICATIONS -- unlike the overlay/exact-alarm grants above, this
   * one has a real in-app runtime dialog (no Settings deep link needed), via
   * tauri-plugin-notification's own JS API. Without it (denied by default on
   * API 33+, and nothing else in the app ever requests it), the break
   * notification and the process-recovery wake notification both silently
   * do nothing. Onboarding already offers this grant on first run; this is
   * the same check/request pair so a user who skipped it there, or revoked
   * it later, has a way back in without reinstalling. */
  async function refreshNotificationPermission() {
    notificationGranted = await isPermissionGranted();
    notificationChecked = true;
  }

  async function grantNotifications() {
    await requestPermission();
    await refreshNotificationPermission();
  }

  /** "Usage access" (PACKAGE_USAGE_STATS) -- same no-in-app-dialog shape as
   * the overlay/exact-alarm grants above; screen_time.rs's Android polling
   * (NativeBridgePlugin.kt::queryUsageEvents) simply returns nothing until
   * this is granted. */
  async function refreshUsageStatsPermission() {
    usageStatsGranted = await invoke<boolean>("can_query_usage_stats");
    usageStatsChecked = true;
  }

  async function openUsageStatsSettings() {
    await invoke("request_usage_stats_permission");
  }

  // Re-checks when the user comes back from the system settings screen --
  // same pattern as onboarding's exact-alarm re-check, needed since that
  // screen's return doesn't reliably resolve any promise here.
  function onOverlayVisibilityChange() {
    if (document.visibilityState === "visible") {
      void refreshOverlayPermission();
      void refreshExactAlarmPermission();
      void refreshNotificationPermission();
      void refreshUsageStatsPermission();
    }
  }

  onMount(() => {
    void refreshOverlayPermission();
    void refreshExactAlarmPermission();
    void refreshNotificationPermission();
    void refreshUsageStatsPermission();
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

  async function toggleMacosHideMenuBarDock() {
    const next = !macosHideMenuBarDockEnabled;
    macosHideMenuBarDockBusy = true;
    try {
      await saveMacosHideMenuBarDockEnabled(next);
      macosHideMenuBarDockEnabled = next;
    } finally {
      macosHideMenuBarDockBusy = false;
    }
  }

  async function toggleScreenTimeTracking() {
    const next = !screenTimeTrackingEnabled;
    screenTimeTrackingBusy = true;
    try {
      await saveScreenTimeTrackingEnabled(next);
      screenTimeTrackingEnabled = next;
    } finally {
      screenTimeTrackingBusy = false;
    }
  }

  // Upper bound of 1440 (24h) just keeps the field sane -- unlike the
  // auto-close timeouts above, nothing downstream of this value has a
  // functional ceiling to defend (it only filters what's rendered).
  async function saveScreenTimeAppThreshold(e: Event) {
    e.preventDefault();
    screenTimeAppThresholdMinutes = Math.min(1440, Math.max(0, screenTimeAppThresholdMinutes));
    await saveScreenTimeAppThresholdMinutes(screenTimeAppThresholdMinutes);
    screenTimeAppThresholdSaved = true;
    setTimeout(() => (screenTimeAppThresholdSaved = false), 2000);
  }

  async function saveDeviceNameSetting(e: Event) {
    e.preventDefault();
    await saveDeviceName(deviceName.trim());
    deviceNameSaved = true;
    setTimeout(() => (deviceNameSaved = false), 2000);
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

  // Upper bound of 60 (1h) on both: the overlay side mirrors Rust's own
  // clamp in set_overlay_auto_close_minutes (an unbounded value there defeats
  // the documented last-resort force-close), and the checkin side is a plain
  // JS setTimeout, which silently fires *immediately* past ~24.8 days
  // (2^31 ms) -- 60 stays well clear of that cliff on top of being a
  // reasonable grace period.
  async function saveOverlayAutoClose(e: Event) {
    e.preventDefault();
    overlayAutoCloseMinutes = Math.min(60, Math.max(1, overlayAutoCloseMinutes));
    await saveOverlayAutoCloseMinutes(overlayAutoCloseMinutes);
    overlayAutoCloseSaved = true;
    setTimeout(() => (overlayAutoCloseSaved = false), 2000);
  }

  async function saveCheckinAutoClose(e: Event) {
    e.preventDefault();
    checkinAutoCloseMinutes = Math.min(60, Math.max(1, checkinAutoCloseMinutes));
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
            `Import "${importFileName}" and merge it into your existing data? Overlapping reflections and task lists are combined (nothing is lost); settings use the imported value when they conflict.`,
          );
    if (!confirmed) return;

    importBusy = true;
    importStatus = "idle";
    try {
      const raw = await readTextFile(importPath);
      const payload = parseAndValidateExport(raw);
      const result = await importData(payload, mode, includeSettingsInTransfer);
      await loadBreakitSettings();
      const mergedClause =
        result.mergedSlotCount > 0
          ? ` ${result.mergedSlotCount} reflection slot${result.mergedSlotCount === 1 ? "" : "s"} merged with existing entries.`
          : "";
      const duplicateClause =
        result.screenTimeDuplicateCount > 0
          ? ` ${result.screenTimeDuplicateCount} duplicate screen time session${result.screenTimeDuplicateCount === 1 ? "" : "s"} skipped.`
          : "";
      const wellnessDuplicateClause =
        result.wellnessCheckDuplicateCount > 0
          ? ` ${result.wellnessCheckDuplicateCount} duplicate wellness check-in${result.wellnessCheckDuplicateCount === 1 ? "" : "s"} skipped.`
          : "";
      importMessage = `Imported ${result.reflectionCount} reflection${result.reflectionCount === 1 ? "" : "s"}, ${result.taskListCount} task list${result.taskListCount === 1 ? "" : "s"}, ${result.notToDoListCount} not-to-do list${result.notToDoListCount === 1 ? "" : "s"}, ${result.settingCount} setting${result.settingCount === 1 ? "" : "s"}, ${result.wellnessCheckCount} wellness check-in${result.wellnessCheckCount === 1 ? "" : "s"}, ${result.screenTimeSessionCount} screen time session${result.screenTimeSessionCount === 1 ? "" : "s"}.${mergedClause}${duplicateClause}${wellnessDuplicateClause}`;
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
    <h2>Session schedule</h2>
    <p class="hint">
      (Fixed for now)<br/> 
      Work runs :00&ndash;:25 and :30&ndash;:55 each hour <br/> 
      Breaks run :25&ndash;:30 and :55&ndash;:00
    </p>
  </section>

  <section class="card">
    <h2>Break screen</h2>
    <p class="hint">
      Typing a captcha is the way of early-exit in case of emergency &mdash;
      it still requires the reflection ("what did I do?") too.
      If neither happens,
      the screen auto-closes on its own after the timeout below.
    </p>

    {#if loaded}
      <form onsubmit={save}>
        <label>
          Captcha length
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
          <input type="number" min="1" max="60" bind:value={overlayAutoCloseMinutes} />
        </label>
        <button type="submit">Save</button>
        {#if overlayAutoCloseSaved}
          <span class="hint saved">Saved</span>
        {/if}
      </form>
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

    {#if isAndroid && exactAlarmChecked}
      <div class="data-row">
        <span>Alarms &amp; reminders: {exactAlarmGranted ? "Granted" : "Not granted"}</span>
        {#if !exactAlarmGranted}
          <button type="button" onclick={openExactAlarmSettings}>Open settings&hellip;</button>
        {/if}
      </div>
      <p class="hint">
        Recommended. Without it, the background timer's wake alarm is inexact and can't reliably
        bring the break screen forward over another app you're actively using.
      </p>
    {/if}

    {#if isAndroid && notificationChecked}
      <div class="data-row">
        <span>Notifications: {notificationGranted ? "Granted" : "Not granted"}</span>
        {#if !notificationGranted}
          <button type="button" onclick={grantNotifications}>Enable notifications</button>
        {/if}
      </div>
      <p class="hint">
        Without it, the background timer's running indicator and the break reminder notification
        both silently don't show. Onboarding offers this grant on first run; this is here for
        anyone who skipped it or revoked it since.
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

    {#if isMacos}
      <p class="hint">
        Cmd+Tab is always blocked during a break, and the break screen always stays visible if you
        swipe to another desktop Space or into another app's full-screen window.
      </p>
    {/if}

    {#if isMacos && macosHideMenuBarDockLoaded}
      <div class="data-row">
        <label class="checkbox">
          <input
            type="checkbox"
            checked={macosHideMenuBarDockEnabled}
            disabled={macosHideMenuBarDockBusy}
            onchange={toggleMacosHideMenuBarDock}
          />
          Also hide the menu bar &amp; Dock during a break
        </label>
      </div>
      <p class="hint">
        Off by default since it's a bigger change to your desktop than anything else here. Either
        way the Dock auto-hides for the duration of a break &mdash; macOS won't let an app block
        Cmd+Tab without that. Like the rest of the break screen, it's a strong deterrent, not an
        absolute lock: Activity Monitor/Force Quit always still works.
      </p>
    {/if}
  </section>



  <section class="card">
    <h2>Wellness check-in</h2>
    
    {#if checkinAutoCloseLoaded}
      <form onsubmit={saveCheckinAutoClose}>
        <label>
          Auto-close after (minutes, if untouched)
          <input type="number" min="1" max="60" bind:value={checkinAutoCloseMinutes} />
        </label>
        <button type="submit">Save</button>
        {#if checkinAutoCloseSaved}
          <span class="hint saved">Saved</span>
        {/if}
      </form>
    {/if}

    <!-- <p class="hint">
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
    {/if} -->

  </section>


  {#if !isAndroid}
  <section class="card">
    <h2>If the break screen ever gets stuck</h2>
    <ul class="hint">
      <li>Press {forceCloseShortcutLabel} to force-close the break screen.</li>
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
          Only turn this off once break screen behavior has been confirmed good across log off/log on,
          system start, and restart &mdash; it's recommended to keep it enabled for at least a week
          first. It's a safety net, not something you'll trigger day to day.
        </p>
      {/if}
    {/if}
  </section>
  {/if}


  <section class="card">
    <h2>Screen time</h2>
    <p class="hint">
      Records which app has focus and for how long, so the Entries tab can show where your day
      actually went. Everything stays on this device &mdash; nothing is uploaded, and only the app's
      name is recorded, never window titles or anything you type.
    </p>

    {#if screenTimeTrackingLoaded}
      <div class="data-row">
        <label class="checkbox">
          <input
            type="checkbox"
            checked={screenTimeTrackingEnabled}
            disabled={screenTimeTrackingBusy}
            onchange={toggleScreenTimeTracking}
          />
          Track screen time
        </label>
      </div>
    {/if}

    {#if osResolved && !isWindows && !isAndroid}
      <p class="hint warning">
        Not captured on this platform yet &mdash; Windows and Android are the only ones recording
        so far. The setting is here, but nothing lands until support for this platform ships.
      </p>
    {/if}

    {#if isAndroid && usageStatsChecked}
      <div class="data-row">
        <span>Usage access: {usageStatsGranted ? "Granted" : "Not granted"}</span>
        {#if !usageStatsGranted}
          <button type="button" onclick={openUsageStatsSettings}>Open settings&hellip;</button>
        {/if}
      </div>
      <p class="hint">
        Required for screen time on Android &mdash; there's no push notification for "which app
        just took focus" without this special-access grant, so tracking records nothing until
        it's on.
      </p>
    {/if}

    {#if screenTimeAppThresholdLoaded}
      <form onsubmit={saveScreenTimeAppThreshold}>
        <label>
          Hide apps under (minutes)
          <input type="number" min="0" max="1440" bind:value={screenTimeAppThresholdMinutes} />
        </label>
        <button type="submit">Save</button>
        {#if screenTimeAppThresholdSaved}
          <span class="hint saved">Saved</span>
        {/if}
      </form>
      <p class="hint">
        Apps with less focus time than this on a given day are left out of the Entries tab's
        breakdown for that day.
      </p>
    {/if}

    {#if deviceNameLoaded}
      <form onsubmit={saveDeviceNameSetting}>
        <label class="grow">
          Device name
          <input type="text" bind:value={deviceName} placeholder="e.g. Work laptop" />
        </label>
        <button type="submit">Save</button>
        {#if deviceNameSaved}
          <span class="hint saved">Saved</span>
        {/if}
      </form>
      <p class="hint">
        Stamped onto new screen time rows so the same app on two machines stays distinguishable if
        you ever import one device's data into another. Defaults to this computer's name.
      </p>
    {/if}
  </section>

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
      <p class="hint warning">Recommended to keep it off atleast for a week, It can help recover from bugs/break screen getting stuck.</p>
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
          Merge
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

  <section class="card">
    <h2>Paired devices</h2>
    <p class="hint">
      Sync reflections, task lists, wellness check-ins, and screen time directly with another
      device on the same wifi network &mdash; no account, no cloud. Settings are never included.
    </p>

    {#if pairedDevicesLoaded && pairedDevices.length > 0}
      <ul class="paired-device-list">
        {#each pairedDevices as device (device.deviceId)}
          <li>
            <span class="paired-device-status" class:online={device.online} title={device.online ? "Online" : "Offline"}
            ></span>
            <span class="paired-device-name">{deviceLabel(device.name, device.deviceId)} <span class="hint">({device.platform})</span></span>
            <span class="hint">Last synced: {formatLastSync(device.lastSyncAt)}</span>
            <button type="button" class="danger" onclick={() => removePairedDevice(device)}>Forget</button>
          </li>
        {/each}
      </ul>
    {:else if pairedDevicesLoaded}
      <p class="hint">No paired devices yet.</p>
    {/if}

    <div class="data-row">
      <button type="button" onclick={refreshOnlineDevices} disabled={pairedDevicesBusy}>Refresh</button>
      <button type="button" onclick={togglePairingPanel}>
        {pairingOpen ? "Cancel pairing" : "Pair a new device…"}
      </button>
    </div>

    {#if pairingOpen}
      <div class="pairing-panel">
        <div class="pairing-column">
          <h3>Show a PIN on this device</h3>
          <p class="hint">
            Read this PIN to whoever is pairing from the other device and have them enter it there.
          </p>
          {#if hostPin}
            <p class="pairing-pin">{hostPin}</p>
            <p class="hint">Waiting for the other device to enter this PIN&hellip; (expires in about a minute)</p>
          {:else}
            <button type="button" onclick={showPairingPin} disabled={hostPinBusy}>Show PIN</button>
          {/if}
        </div>

        <div class="pairing-column">
          <h3>Enter a PIN from another device</h3>
          <p class="hint">Pick the device that's showing a PIN, then type it in here.</p>
          <div class="data-row">
            <button type="button" onclick={refreshPairingCandidates} disabled={candidatesBusy}>
              {candidatesBusy ? "Searching…" : "Search again"}
            </button>
          </div>
          {#if pairingCandidates.length === 0}
            <p class="hint">{candidatesBusy ? "Searching the local network…" : "No unpaired devices found nearby."}</p>
          {:else}
            <form onsubmit={submitJoinPairing}>
              <label class="grow">
                Device
                <select bind:value={selectedCandidateId}>
                  <option value="" disabled>Select a device&hellip;</option>
                  {#each pairingCandidates as candidate (candidate.deviceId)}
                    <option value={candidate.deviceId}>{deviceLabel(candidate.name, candidate.deviceId)} ({candidate.platform})</option>
                  {/each}
                </select>
              </label>
              <label>
                PIN
                <input type="text" inputmode="numeric" maxlength="6" bind:value={joinPin} placeholder="123456" />
              </label>
              <button type="submit" disabled={joinBusy || !selectedCandidateId || !joinPin.trim()}>
                {joinBusy ? "Pairing…" : "Pair"}
              </button>
            </form>
          {/if}
          {#if joinError}
            <p class="hint error">{joinError}</p>
          {/if}
        </div>
      </div>
    {/if}

    <h3>Sync devices</h3>
    <p class="hint">
      Pull the selected device's changes and send yours back in one step &mdash; only devices
      currently online show up below.
    </p>
    <div class="data-row">
      <label class="grow">
        Device
        <select bind:value={syncDeviceId}>
          <option value="">Select a device&hellip;</option>
          {#each pairedDevices.filter((d) => d.online) as device (device.deviceId)}
            <option value={device.deviceId}>{deviceLabel(device.name, device.deviceId)} ({device.platform})</option>
          {/each}
        </select>
      </label>
      <button type="button" onclick={runDeviceSync} disabled={syncBusy || !syncDeviceId}>
        {syncBusy ? "Syncing…" : "Sync"}
      </button>
    </div>
    {#if syncStatus === "success"}
      <p class="hint saved">{syncMessage}</p>
    {:else if syncStatus === "error"}
      <p class="hint error">{syncMessage}</p>
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

  h3 {
    margin: 20px 0 8px;
    font-size: 13px;
    color: var(--text);
  }

  select {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 8px;
    color: inherit;
    padding: 8px 10px;
    font-size: 14px;
    width: 100%;
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

  .pairing-panel {
    margin-top: 16px;
    display: flex;
    gap: 24px;
    flex-wrap: wrap;
  }

  .pairing-column {
    flex: 1;
    min-width: 220px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 16px;
  }

  .pairing-column h3 {
    margin-top: 0;
  }

  .pairing-pin {
    font-size: 28px;
    font-weight: 700;
    letter-spacing: 4px;
    margin: 8px 0;
  }
</style>
