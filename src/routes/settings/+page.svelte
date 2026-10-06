<script lang="ts">
  import { onMount, onDestroy, tick } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { save as saveDialog, open as openDialog, ask } from "@tauri-apps/plugin-dialog";
  import { isPermissionGranted, requestPermission } from "@tauri-apps/plugin-notification";
  import { sanitizeAttributionHtml } from "$lib/sanitizeHtml";
  import {
    getBreakitSettings,
    saveBreakitSettings,
    exportAllData,
    countEntriesOlderThan,
    deleteEntriesOlderThan,
    deleteAllData,
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
    getHideOverlayOnCallEnabled,
    saveHideOverlayOnCallEnabled,
    getOverlayDueHabitsEnabled,
    saveOverlayDueHabitsEnabled,
    getNightPauseConfig,
    saveNightPauseConfig,
    getAutoPauseOnWakeConfig,
    saveAutoPauseOnWakeConfig,
    getOverlayAutoCloseMinutes,
    saveOverlayAutoCloseMinutes,
    getCheckinAutoCloseMinutes,
    saveCheckinAutoCloseMinutes,
    getMacosHideMenuBarDockEnabled,
    getMacosMediaKeyFallbackEnabled,
    saveMacosMediaKeyFallbackEnabled,
    getMediaPauseStatus,
    saveMacosHideMenuBarDockEnabled,
    getScreenTimeTrackingEnabled,
    saveScreenTimeTrackingEnabled,
    getScreenTimeAppThresholdMinutes,
    saveScreenTimeAppThresholdMinutes,
    getDeviceName,
    saveDeviceName,
    getQuoteApiUrl,
    saveQuoteApiUrl,
    getQuoteApiAttribution,
    saveQuoteApiAttribution,
    getLlmSummaryConfig,
    saveLlmSummaryBasic,
    saveLlmSummaryAdvanced,
    LLM_SUMMARY_MIN_TIMEOUT_SECS,
    LLM_SUMMARY_MAX_TIMEOUT_SECS,
    getCalendarConfig,
    saveCalendarSources,
    saveCalendarPrefs,
    validateCalendarUrl,
    listHolidayCountries,
    CALENDAR_DEFAULT_LOOKAHEAD_DAYS,
    CALENDAR_MIN_LOOKAHEAD_DAYS,
    CALENDAR_MAX_LOOKAHEAD_DAYS,
    CALENDAR_DEFAULT_MAX_ITEMS,
    CALENDAR_MIN_MAX_ITEMS,
    CALENDAR_MAX_MAX_ITEMS,
    type CalendarSource,
    type HolidayCountry,
    getThemePreference,
    saveThemePreference,
    type ThemePreference,
    startPairing,
    cancelPairing,
    browsePairingCandidates,
    confirmPairing,
    getPairedDevices,
    browseOnlinePairedDevices,
    forgetPairedDevice,
    syncWithDevice,
    setDeviceAutoSyncEnabled,
    type BreakitSettings,
    type ImportMode,
    type DiscoveredDevice,
    type PairedDeviceInfo,
  } from "$lib/db";
  import EncryptionKeyCard from "$lib/EncryptionKeyCard.svelte";
  import AppLockCard from "$lib/AppLockCard.svelte";

  /** Settings -> Advanced is collapsed by default; the open/closed choice is
   * a per-viewer convenience, so browser storage (which can throw or come
   * back empty) is fine and only ever a nicety. */
  const ADVANCED_OPEN_KEY = "settings.advancedOpen";
  let advancedOpen = $state(false);
  try {
    advancedOpen = localStorage.getItem(ADVANCED_OPEN_KEY) === "1";
  } catch {
    // storage unavailable -- stay collapsed
  }

  function toggleAdvanced() {
    advancedOpen = !advancedOpen;
    try {
      localStorage.setItem(ADVANCED_OPEN_KEY, advancedOpen ? "1" : "0");
    } catch {
      // storage unavailable -- the toggle still works for this visit
    }
  }

  /** Section list in the left-hand navigation panel. Each id is on its card in
   * the markup below; the desktop-only cards are dropped on mobile, the same
   * condition that hides the cards themselves. */
  const sectionNav = $derived(
    [
      { id: "appearance", label: "Appearance" },
      { id: "session-schedule", label: "Schedule" },
      { id: "break-screen", label: "Break screen" },
      { id: "screen-time", label: "Screen time" },
      { id: "app-lock", label: "App lock" },
      { id: "daily-summary", label: "Daily summary" },
      { id: "calendar", label: "Calendar" },
      { id: "auto-pause", label: "Auto-pause", desktopOnly: true },
      { id: "wellness-checkin", label: "Check-in" },
      { id: "stuck-break-screen", label: "Stuck break screen", desktopOnly: true },
      { id: "startup", label: "Startup", desktopOnly: true },
      { id: "data", label: "Data" },
      { id: "paired-devices", label: "Paired devices" },
      { id: "advanced", label: "Advanced" },
    ].filter((s) => !(s.desktopOnly && isMobile)),
  );

  /** Scrolls rather than following the hash, so SvelteKit's router never
   * sees a navigation. "Advanced" also expands that section, since its
   * cards don't exist until it's open. */
  async function jumpToSection(event: MouseEvent, id: string) {
    event.preventDefault();
    if (id === "advanced" && !advancedOpen) {
      toggleAdvanced();
      await tick();
    }
    activeSection = id;
    document
      .getElementById(`settings-${id}`)
      ?.scrollIntoView({ behavior: "smooth", block: "start" });
  }

  /** Highlights the section currently at the top of the content. The page
   * scrolls inside the layout's <main>, not the window, so listen there. */
  let activeSection = $state("appearance");
  let navEl = $state<HTMLElement | null>(null);

  $effect(() => {
    const scroller = navEl?.closest("main");
    if (!scroller) return;
    const ids = sectionNav.map((s) => s.id);
    let frame = 0;
    const update = () => {
      frame = 0;
      const top = scroller.getBoundingClientRect().top + 96;
      let current = ids[0];
      for (const id of ids) {
        const el = document.getElementById(`settings-${id}`);
        if (el && el.getBoundingClientRect().top <= top) current = id;
      }
      // Scrolled to the bottom: the last sections may never reach the top.
      if (scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 2) {
        current = ids[ids.length - 1];
      }
      activeSection = current;
    };
    const onScroll = () => {
      if (!frame) frame = requestAnimationFrame(update);
    };
    scroller.addEventListener("scroll", onScroll, { passive: true });
    update();
    return () => {
      scroller.removeEventListener("scroll", onScroll);
      if (frame) cancelAnimationFrame(frame);
    };
  });

  /** Narrow screens only, where the panel is one sideways-scrolling row with
   * its scrollbar hidden: fade whichever edge has more chips beyond it, and
   * keep the active chip in view as the page scrolls. On the side panel the
   * track never overflows sideways, so all of this is a no-op there. */
  let trackEl = $state<HTMLElement | null>(null);
  let fadeStart = $state(false);
  let fadeEnd = $state(false);

  function updateFades() {
    if (!trackEl) return;
    const max = trackEl.scrollWidth - trackEl.clientWidth;
    fadeStart = trackEl.scrollLeft > 1;
    fadeEnd = trackEl.scrollLeft < max - 1;
  }

  $effect(() => {
    if (!trackEl) return;
    const observer = new ResizeObserver(updateFades);
    observer.observe(trackEl);
    updateFades();
    return () => observer.disconnect();
  });

  $effect(() => {
    const id = activeSection;
    if (!trackEl || trackEl.scrollWidth <= trackEl.clientWidth + 1) return;
    const chip = trackEl.querySelector<HTMLElement>(
      `a[href="#settings-${id}"]`,
    );
    if (!chip) return;
    trackEl.scrollTo({
      left: chip.offsetLeft - (trackEl.clientWidth - chip.offsetWidth) / 2,
      behavior: "smooth",
    });
  });

  let themePreference = $state<ThemePreference>("auto");
  let themeLoaded = $state(false);

  async function handleThemeSelect(event: Event) {
    const select = event.currentTarget as HTMLSelectElement;
    const next = select.value === "light" || select.value === "dark" ? select.value : "auto";
    themePreference = next;
    await saveThemePreference(next);
  }

  let length = $state(15);
  let includeSpecial = $state(false);
  let maxPerDay = $state(5);
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
  let isIos = $state(false);
  /** Hides desktop-only controls (force-close shortcut, launch at login). */
  const isMobile = $derived(isAndroid || isIos);
  let breakNotificationPersistentEnabled = $state(true);
  let breakNotificationPersistentLoaded = $state(false);
  let breakNotificationPersistentBusy = $state(false);
  let hideOverlayOnCallEnabled = $state(true);
  let hideOverlayOnCallLoaded = $state(false);
  let hideOverlayOnCallBusy = $state(false);
  let overlayDueHabitsEnabled = $state(true);
  let overlayDueHabitsLoaded = $state(false);
  let overlayDueHabitsBusy = $state(false);

  let nightPauseEnabled = $state(true);
  let nightPauseStart = $state("22:00");
  let nightPauseEnd = $state("08:00");
  let nightPauseLoaded = $state(false);
  let nightPauseBusy = $state(false);
  let nightPauseSaved = $state(false);

  let autoPauseOnWakeEnabled = $state(true);
  let autoPauseOnWakeOffMinutes = $state(15);
  let autoPauseOnWakeRemainingMinutes = $state(10);
  let autoPauseOnWakePauseMinutes = $state(20);
  let autoPauseOnWakeIncludeScreenOff = $state(true);
  let autoPauseOnWakeLoaded = $state(false);
  let autoPauseOnWakeBusy = $state(false);
  let autoPauseOnWakeSaved = $state(false);

  function minutesToHhMm(minutes: number): string {
    const h = Math.floor(minutes / 60) % 24;
    const m = minutes % 60;
    return `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}`;
  }

  function hhMmToMinutes(hhMm: string): number {
    const [h, m] = hhMm.split(":").map(Number);
    return (h % 24) * 60 + (m % 60);
  }

  let isMacos = $state(false);
  let isWindows = $state(false);
  // Gates the "not captured on this platform yet" screen-time hint on
  // current_os having actually resolved -- without it that hint flashes on
  // Windows too, since isWindows starts false.
  let osResolved = $state(false);
  let macosHideMenuBarDockEnabled = $state(false);
  let macosHideMenuBarDockLoaded = $state(false);
  let macosHideMenuBarDockBusy = $state(false);

  let macosMediaKeyFallbackEnabled = $state(false);
  let macosMediaKeyFallbackLoaded = $state(false);
  let macosMediaKeyFallbackBusy = $state(false);
  // Whether the permission-free path is even usable on this Mac, so the copy
  // below can say "you turned this on" vs "this Mac can't do it the easy way".
  let mediaRemoteAvailable = $state<boolean | null>(null);

  let screenTimeTrackingEnabled = $state(true);
  let screenTimeTrackingLoaded = $state(false);
  let screenTimeTrackingBusy = $state(false);

  let screenTimeAppThresholdMinutes = $state(5);
  let screenTimeAppThresholdLoaded = $state(false);
  let screenTimeAppThresholdSaved = $state(false);

  let deviceName = $state("");
  let deviceNameLoaded = $state(false);
  let deviceNameSaved = $state(false);

  let quoteApiUrl = $state("");
  let quoteApiUrlLoaded = $state(false);
  let quoteApiUrlSaved = $state(false);

  let quoteApiAttribution = $state("");
  let quoteApiAttributionLoaded = $state(false);
  let quoteApiAttributionSaved = $state(false);
  // Fetched from Rust (single source of truth is db.rs's
  // DEFAULT_QUOTE_API_ATTRIBUTION const), not hardcoded here -- see
  // resetQuoteApiAttributionToDefault below.
  let defaultQuoteApiAttribution = $state("");

  let llmSummaryLoaded = $state(false);
  let llmSummaryApiUrl = $state("");
  let llmSummaryModel = $state("");
  let llmSummaryBasicSaved = $state(false);
  let llmSummaryApiKey = $state("");
  let llmSummaryTimeoutSecs = $state(60);
  let llmSummarySystemPrompt = $state("");
  let llmSummaryAdvancedSaved = $state(false);
  // Fetched from Rust (db.rs's DEFAULT_LLM_SUMMARY_SYSTEM_PROMPT), same
  // single-source-of-truth pattern as defaultQuoteApiAttribution above.
  let defaultLlmSummarySystemPrompt = $state("");

  // Calendar events (home "Upcoming events" card). Sources are encrypted at
  // rest and device-local -- see db.ts's "Calendar events" section.
  let calendarLoaded = $state(false);
  let calendarLoadError = $state("");
  let calendarSources = $state<CalendarSource[]>([]);
  let holidayCountries = $state<HolidayCountry[]>([]);
  let calendarLookaheadDays = $state(CALENDAR_DEFAULT_LOOKAHEAD_DAYS);
  let calendarMaxItems = $state(CALENDAR_DEFAULT_MAX_ITEMS);
  let calendarHolidayPublicOnly = $state(true);
  let calendarPrefsSaved = $state(false);
  let calendarNewName = $state("");
  let calendarNewUrl = $state("");
  let calendarAdding = $state(false);
  let calendarMessage = $state("");
  let calendarMessageIsError = $state(false);

  const holidaySource = $derived(calendarSources.find((s) => s.kind === "holiday"));
  const personalSources = $derived(calendarSources.filter((s) => s.kind === "personal"));
  const holidayCountryCode = $derived(
    holidayCountries.find((c) => c.url === holidaySource?.url)?.code ?? "",
  );
  /** A hint only -- picking a country is what turns the feed on, so nothing
   * is fetched from Google until the user opts in. */
  const suggestedHolidayCountry = $derived.by(() => {
    const region = (typeof navigator !== "undefined" ? navigator.language : "").split("-")[1];
    return region ? holidayCountries.find((c) => c.code === region.toUpperCase()) : undefined;
  });

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

  // --- Delete data (Advanced) ---------------------------------------------

  let deleteOlderDays = $state(90);
  let deleteBusy = $state(false);
  let deleteStatus = $state<"idle" | "checking" | "cancelled" | "success" | "error">("idle");
  let deleteMessage = $state("");

  async function runDeleteOlder() {
    const days = Math.max(1, Math.floor(Number(deleteOlderDays) || 0));
    if (!days) return;
    deleteOlderDays = days;
    deleteBusy = true;
    deleteStatus = "checking";
    deleteMessage = "Checking…";
    try {
      const count = await countEntriesOlderThan(days);
      if (count === 0) {
        deleteStatus = "success";
        deleteMessage = `Nothing older than ${days} day${days === 1 ? "" : "s"}.`;
        return;
      }
      // Native dialog, not window.confirm(): webview JS dialogs aren't reliable on every platform.
      const ok = await ask(
        `Delete ${count} record${count === 1 ? "" : "s"} (reflections, check-ins, task lists, screen time) ` +
          `from before ${days} day${days === 1 ? "" : "s"} ago?\n\n` +
          `This can't be undone. It only affects this device: a paired device that still has these ` +
          `entries will send them back on its next sync.`,
        { title: "Delete older entries", kind: "warning", okLabel: "Delete", cancelLabel: "Cancel" },
      );
      if (!ok) {
        deleteStatus = "cancelled";
        deleteMessage = "Cancelled. Nothing was deleted.";
        return;
      }
      await deleteEntriesOlderThan(days);
      deleteStatus = "success";
      deleteMessage = `Deleted ${count} record${count === 1 ? "" : "s"}.`;
    } catch (e) {
      deleteStatus = "error";
      deleteMessage = e instanceof Error ? e.message : String(e);
    } finally {
      deleteBusy = false;
    }
  }

  async function runDeleteAll() {
    deleteBusy = true;
    deleteStatus = "idle";
    try {
      const ok = await ask(
        "Delete ALL reflections, check-ins, task lists, screen time, saved prefill presets, habits and habit logs?\n\n" +
          "This can't be undone. Consider exporting your data first. Settings and paired devices are kept, " +
          "and a paired device that still has your entries will send them back on its next sync.",
        { title: "Delete all data", kind: "warning", okLabel: "Delete all", cancelLabel: "Cancel" },
      );
      if (!ok) {
        deleteStatus = "cancelled";
        deleteMessage = "Cancelled. Nothing was deleted.";
        return;
      }
      await deleteAllData();
      deleteStatus = "success";
      deleteMessage = "All data deleted.";
    } catch (e) {
      deleteStatus = "error";
      deleteMessage = e instanceof Error ? e.message : String(e);
    } finally {
      deleteBusy = false;
    }
  }

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

  let syncingDeviceId = $state<string | null>(null);
  let syncStatus = $state<"idle" | "success" | "error">("idle");
  let syncMessage = $state("");
  let autoSyncBusyId = $state<string | null>(null);

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
        result.bulkEditPresetCount +
        result.habitCount +
        result.habitLogCount;
      const mergedClause =
        result.mergedSlotCount > 0
          ? ` ${result.mergedSlotCount} reflection slot${result.mergedSlotCount === 1 ? "" : "s"} merged with existing entries.`
          : "";
      // Reports both directions explicitly -- a sync that only moved data
      // outward (this device had a new change, the peer had nothing new to
      // send back) has receivedTotal = 0, which used to read as "nothing
      // happened" even though the change was sent and applied on the peer.
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

  async function loadBreakitSettings() {
    const settings: BreakitSettings = await getBreakitSettings();
    length = settings.length;
    includeSpecial = settings.includeSpecial;
    maxPerDay = settings.maxPerDay;
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
    isIos = os === "ios";
    isMacos = os === "macos";
    isWindows = os === "windows";
    osResolved = true;
  });

  onMount(async () => {
    macosHideMenuBarDockEnabled = await getMacosHideMenuBarDockEnabled();
    macosHideMenuBarDockLoaded = true;
  });

  onMount(async () => {
    macosMediaKeyFallbackEnabled = await getMacosMediaKeyFallbackEnabled();
    macosMediaKeyFallbackLoaded = true;
    mediaRemoteAvailable = (await getMediaPauseStatus()).mediaRemoteAvailable;
  });

  onMount(async () => {
    breakNotificationPersistentEnabled = await getBreakNotificationPersistentEnabled();
    breakNotificationPersistentLoaded = true;
    hideOverlayOnCallEnabled = await getHideOverlayOnCallEnabled();
    hideOverlayOnCallLoaded = true;
  });

  onMount(async () => {
    const config = await getNightPauseConfig();
    nightPauseEnabled = config.enabled;
    nightPauseStart = minutesToHhMm(config.startMinutes);
    nightPauseEnd = minutesToHhMm(config.endMinutes);
    nightPauseLoaded = true;
  });

  onMount(async () => {
    const config = await getAutoPauseOnWakeConfig();
    autoPauseOnWakeEnabled = config.enabled;
    autoPauseOnWakeOffMinutes = config.offMinutes;
    autoPauseOnWakeRemainingMinutes = config.remainingMinutes;
    autoPauseOnWakePauseMinutes = config.pauseMinutes;
    autoPauseOnWakeIncludeScreenOff = config.includeScreenOff;
    autoPauseOnWakeLoaded = true;
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

  onMount(async () => {
    overlayDueHabitsEnabled = await getOverlayDueHabitsEnabled();
    overlayDueHabitsLoaded = true;
  });

  onMount(async () => {
    quoteApiUrl = await getQuoteApiUrl();
    quoteApiUrlLoaded = true;
  });

  onMount(async () => {
    quoteApiAttribution = await getQuoteApiAttribution();
    quoteApiAttributionLoaded = true;
  });

  onMount(async () => {
    defaultQuoteApiAttribution = await invoke<string>("default_quote_api_attribution");
  });

  onMount(async () => {
    const cfg = await getLlmSummaryConfig();
    llmSummaryApiUrl = cfg.apiUrl;
    llmSummaryModel = cfg.model;
    llmSummaryApiKey = cfg.apiKey;
    llmSummaryTimeoutSecs = cfg.timeoutSecs;
    // Pre-populate the editor with the built-in default when there's no
    // override, so the user sees (and can edit) the real prompt.
    defaultLlmSummarySystemPrompt = await invoke<string>("default_llm_summary_system_prompt");
    llmSummarySystemPrompt = cfg.systemPrompt.trim() || defaultLlmSummarySystemPrompt;
    llmSummaryLoaded = true;
  });

  onMount(async () => {
    try {
      const cfg = await getCalendarConfig();
      calendarSources = cfg.sources;
      calendarLookaheadDays = cfg.lookaheadDays;
      calendarMaxItems = cfg.maxItems;
      calendarHolidayPublicOnly = cfg.holidayPublicOnly;
      holidayCountries = await listHolidayCountries();
      calendarLoaded = true;
    } catch (e) {
      // Most likely a locked encryption key (the sources are encrypted); the
      // unlock modal handles that, and a revisit loads normally afterwards.
      calendarLoadError = String(e);
    }
  });

  onMount(async () => {
    themePreference = await getThemePreference();
    themeLoaded = true;
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

  async function toggleOverlayDueHabits() {
    const next = !overlayDueHabitsEnabled;
    overlayDueHabitsBusy = true;
    try {
      await saveOverlayDueHabitsEnabled(next);
      overlayDueHabitsEnabled = next;
    } finally {
      overlayDueHabitsBusy = false;
    }
  }

  async function toggleHideOverlayOnCall() {
    const next = !hideOverlayOnCallEnabled;
    hideOverlayOnCallBusy = true;
    try {
      await saveHideOverlayOnCallEnabled(next);
      hideOverlayOnCallEnabled = next;
    } finally {
      hideOverlayOnCallBusy = false;
    }
  }

  async function toggleNightPauseEnabled() {
    const next = !nightPauseEnabled;
    nightPauseBusy = true;
    try {
      await saveNightPauseConfig({
        enabled: next,
        startMinutes: hhMmToMinutes(nightPauseStart),
        endMinutes: hhMmToMinutes(nightPauseEnd),
      });
      nightPauseEnabled = next;
    } finally {
      nightPauseBusy = false;
    }
  }

  async function saveNightPauseWindow(e: Event) {
    e.preventDefault();
    await saveNightPauseConfig({
      enabled: nightPauseEnabled,
      startMinutes: hhMmToMinutes(nightPauseStart),
      endMinutes: hhMmToMinutes(nightPauseEnd),
    });
    nightPauseSaved = true;
    setTimeout(() => (nightPauseSaved = false), 2000);
  }

  async function toggleAutoPauseOnWakeEnabled() {
    const next = !autoPauseOnWakeEnabled;
    autoPauseOnWakeBusy = true;
    try {
      await saveAutoPauseOnWakeConfig({
        enabled: next,
        offMinutes: autoPauseOnWakeOffMinutes,
        remainingMinutes: autoPauseOnWakeRemainingMinutes,
        pauseMinutes: autoPauseOnWakePauseMinutes,
        includeScreenOff: autoPauseOnWakeIncludeScreenOff,
      });
      autoPauseOnWakeEnabled = next;
    } finally {
      autoPauseOnWakeBusy = false;
    }
  }

  async function toggleAutoPauseOnWakeIncludeScreenOff() {
    const next = !autoPauseOnWakeIncludeScreenOff;
    autoPauseOnWakeBusy = true;
    try {
      await saveAutoPauseOnWakeConfig({
        enabled: autoPauseOnWakeEnabled,
        offMinutes: autoPauseOnWakeOffMinutes,
        remainingMinutes: autoPauseOnWakeRemainingMinutes,
        pauseMinutes: autoPauseOnWakePauseMinutes,
        includeScreenOff: next,
      });
      autoPauseOnWakeIncludeScreenOff = next;
    } finally {
      autoPauseOnWakeBusy = false;
    }
  }

  async function saveAutoPauseOnWakeThresholds(e: Event) {
    e.preventDefault();
    autoPauseOnWakeOffMinutes = Math.min(180, Math.max(1, autoPauseOnWakeOffMinutes));
    autoPauseOnWakeRemainingMinutes = Math.min(60, Math.max(1, autoPauseOnWakeRemainingMinutes));
    autoPauseOnWakePauseMinutes = Math.min(240, Math.max(5, autoPauseOnWakePauseMinutes));
    await saveAutoPauseOnWakeConfig({
      enabled: autoPauseOnWakeEnabled,
      offMinutes: autoPauseOnWakeOffMinutes,
      remainingMinutes: autoPauseOnWakeRemainingMinutes,
      pauseMinutes: autoPauseOnWakePauseMinutes,
      includeScreenOff: autoPauseOnWakeIncludeScreenOff,
    });
    autoPauseOnWakeSaved = true;
    setTimeout(() => (autoPauseOnWakeSaved = false), 2000);
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

  async function toggleMacosMediaKeyFallback() {
    const next = !macosMediaKeyFallbackEnabled;
    macosMediaKeyFallbackBusy = true;
    try {
      await saveMacosMediaKeyFallbackEnabled(next);
      macosMediaKeyFallbackEnabled = next;
    } finally {
      macosMediaKeyFallbackBusy = false;
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

  async function saveQuoteApiUrlSetting(e: Event) {
    e.preventDefault();
    await saveQuoteApiUrl(quoteApiUrl.trim());
    quoteApiUrlSaved = true;
    setTimeout(() => (quoteApiUrlSaved = false), 2000);
  }

  async function saveQuoteApiAttributionSetting(e: Event) {
    e.preventDefault();
    await saveQuoteApiAttribution(quoteApiAttribution.trim());
    quoteApiAttributionSaved = true;
    setTimeout(() => (quoteApiAttributionSaved = false), 2000);
  }

  function resetQuoteApiAttributionToDefault() {
    quoteApiAttribution = defaultQuoteApiAttribution;
  }

  async function saveLlmSummaryBasicSetting(e: Event) {
    e.preventDefault();
    llmSummaryApiUrl = llmSummaryApiUrl.trim();
    llmSummaryModel = llmSummaryModel.trim();
    await saveLlmSummaryBasic(llmSummaryApiUrl, llmSummaryModel);
    llmSummaryBasicSaved = true;
    setTimeout(() => (llmSummaryBasicSaved = false), 2000);
  }

  async function saveLlmSummaryAdvancedSetting(e: Event) {
    e.preventDefault();
    const timeout = Math.round(Number(llmSummaryTimeoutSecs));
    llmSummaryTimeoutSecs = Math.min(
      LLM_SUMMARY_MAX_TIMEOUT_SECS,
      Math.max(LLM_SUMMARY_MIN_TIMEOUT_SECS, Number.isFinite(timeout) ? timeout : 60),
    );
    llmSummaryApiKey = llmSummaryApiKey.trim();
    llmSummarySystemPrompt = llmSummarySystemPrompt.trim() || defaultLlmSummarySystemPrompt;
    // An unedited default is stored as blank (an override of "none"), so a
    // future change to the built-in default still reaches this user.
    const promptOverride =
      llmSummarySystemPrompt === defaultLlmSummarySystemPrompt.trim() ? "" : llmSummarySystemPrompt;
    await saveLlmSummaryAdvanced(llmSummaryApiKey, llmSummaryTimeoutSecs, promptOverride);
    llmSummaryAdvancedSaved = true;
    setTimeout(() => (llmSummaryAdvancedSaved = false), 2000);
  }

  function resetLlmSummarySystemPromptToDefault() {
    // Shows the default in the editor; saving it stores a blank override
    // (see saveLlmSummaryAdvancedSetting), so future default changes still apply.
    llmSummarySystemPrompt = defaultLlmSummarySystemPrompt;
  }

  function setCalendarMessage(text: string, isError: boolean) {
    calendarMessage = text;
    calendarMessageIsError = isError;
  }

  /** Persists a new source list; on failure the in-memory list is rolled back
   * so the UI never shows a calendar that wasn't actually saved. */
  async function persistCalendarSources(next: CalendarSource[]): Promise<boolean> {
    const previous = calendarSources;
    calendarSources = next;
    try {
      await saveCalendarSources(next);
      return true;
    } catch (e) {
      calendarSources = previous;
      setCalendarMessage(`Couldn't save: ${e}`, true);
      return false;
    }
  }

  async function changeHolidayCountry(e: Event) {
    const code = (e.currentTarget as HTMLSelectElement).value;
    const others = calendarSources.filter((s) => s.kind !== "holiday");
    const country = holidayCountries.find((c) => c.code === code);
    const next: CalendarSource[] = country
      ? [
          ...others,
          {
            id: crypto.randomUUID(),
            name: `Holidays in ${country.name}`,
            kind: "holiday",
            url: country.url,
            enabled: true,
          },
        ]
      : others;
    setCalendarMessage("", false);
    await persistCalendarSources(next);
  }

  async function addPersonalCalendar(e: Event) {
    e.preventDefault();
    const url = calendarNewUrl.trim();
    if (!url) return;
    calendarAdding = true;
    setCalendarMessage("", false);
    try {
      // Fetching it once doubles as validation: a typo or a non-iCal URL is
      // rejected here instead of silently showing nothing on the home screen.
      const check = await validateCalendarUrl(url);
      const name = calendarNewName.trim() || check.name || "Calendar";
      const ok = await persistCalendarSources([
        ...calendarSources,
        { id: crypto.randomUUID(), name, kind: "personal", url, enabled: true },
      ]);
      if (ok) {
        calendarNewName = "";
        calendarNewUrl = "";
        setCalendarMessage(`Added “${name}” (${check.eventCount} events found).`, false);
      }
    } catch (err) {
      setCalendarMessage(String(err), true);
    } finally {
      calendarAdding = false;
    }
  }

  async function toggleCalendarSource(id: string, enabled: boolean) {
    setCalendarMessage("", false);
    await persistCalendarSources(calendarSources.map((s) => (s.id === id ? { ...s, enabled } : s)));
  }

  async function removeCalendarSource(id: string) {
    setCalendarMessage("", false);
    await persistCalendarSources(calendarSources.filter((s) => s.id !== id));
  }

  async function saveCalendarPreferences(e: Event) {
    e.preventDefault();
    const clamp = (raw: number, min: number, max: number, fallback: number) => {
      const n = Math.round(Number(raw));
      return Number.isFinite(n) ? Math.min(max, Math.max(min, n)) : fallback;
    };
    calendarLookaheadDays = clamp(
      calendarLookaheadDays,
      CALENDAR_MIN_LOOKAHEAD_DAYS,
      CALENDAR_MAX_LOOKAHEAD_DAYS,
      CALENDAR_DEFAULT_LOOKAHEAD_DAYS,
    );
    calendarMaxItems = clamp(
      calendarMaxItems,
      CALENDAR_MIN_MAX_ITEMS,
      CALENDAR_MAX_MAX_ITEMS,
      CALENDAR_DEFAULT_MAX_ITEMS,
    );
    await saveCalendarPrefs(calendarLookaheadDays, calendarMaxItems, calendarHolidayPublicOnly);
    calendarPrefsSaved = true;
    setTimeout(() => (calendarPrefsSaved = false), 2000);
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
    await saveBreakitSettings({
      length: Math.min(64, Math.max(4, length)),
      includeSpecial,
      maxPerDay: Math.min(100, Math.max(1, maxPerDay)),
    });
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
      const presetStaleClause =
        result.bulkEditPresetStaleCount > 0
          ? ` ${result.bulkEditPresetStaleCount} bulk-edit preset${result.bulkEditPresetStaleCount === 1 ? "" : "s"} already up to date, skipped.`
          : "";
      importMessage = `Imported ${result.reflectionCount} reflection${result.reflectionCount === 1 ? "" : "s"}, ${result.taskListCount} task list${result.taskListCount === 1 ? "" : "s"}, ${result.notToDoListCount} not-to-do list${result.notToDoListCount === 1 ? "" : "s"}, ${result.settingCount} setting${result.settingCount === 1 ? "" : "s"}, ${result.wellnessCheckCount} wellness check-in${result.wellnessCheckCount === 1 ? "" : "s"}, ${result.screenTimeSessionCount} screen time session${result.screenTimeSessionCount === 1 ? "" : "s"}, ${result.bulkEditPresetCount} bulk-edit preset${result.bulkEditPresetCount === 1 ? "" : "s"}, ${result.habitCount} habit${result.habitCount === 1 ? "" : "s"}, ${result.habitLogCount} habit log${result.habitLogCount === 1 ? "" : "s"}.${mergedClause}${duplicateClause}${wellnessDuplicateClause}${presetStaleClause}`;
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
  <nav class="section-nav" aria-label="Settings sections" bind:this={navEl}>
    <div
      class="nav-track"
      class:fade-start={fadeStart}
      class:fade-end={fadeEnd}
      bind:this={trackEl}
      onscroll={updateFades}
    >
      {#each sectionNav as section (section.id)}
        <a
          href={`#settings-${section.id}`}
          class:active={activeSection === section.id}
          aria-current={activeSection === section.id ? "location" : undefined}
          onclick={(e) => jumpToSection(e, section.id)}>{section.label}</a
        >
      {/each}
    </div>
  </nav>

  <div class="content">

    <section class="card" id="settings-appearance">
      <h2>Appearance</h2>
      {#if themeLoaded}
        <div class="data-row">
          <label>
            Theme
            <select value={themePreference} onchange={handleThemeSelect}>
              <option value="auto">Auto (based on OS theme)</option>
              <option value="light">Light</option>
              <option value="dark">Dark</option>
            </select>
          </label>
        </div>
      {/if}
    </section>


    <section class="card" id="settings-session-schedule">
      <h2>Session schedule</h2>
      <p class="hint">
        <strong>Normal</strong><br/>
        Work runs :00&ndash;:25 and :30&ndash;:55 each hour<br/>
        Breaks run :25&ndash;:30 (5 min) and :55&ndash;:00 (5 min)
      </p>
      <p class="hint">
        <strong>Concentration</strong><br/>
        Work runs :00&ndash;:25 and :26&ndash;:50 each hour<br/>
        Breaks run :25&ndash;:26 (1 min) and :50&ndash;:00 (10 min)
      </p>
      <p class="hint">To change the mode, use the mode selector on the Timer screen.</p>
    </section>


    <section class="card" id="settings-break-screen">
      <h2>Break screen</h2>
      <p class="hint">
        Typing a captcha is the way of early-exit in case of emergency &mdash;
        it still requires the reflection ("what did I do?") too.
        Emergency exits are capped per day &mdash; once used up, only the
        reflection-plus-timer path is left for the rest of the day.
        If neither happens,
        the screen auto-closes on its own after the timeout below.
      </p>

      {#if loaded}
        <form onsubmit={save}>
          <label>
            Captcha length
            <input type="number" min="8" max="25" bind:value={length} />
          </label>
          <label class="checkbox">
            <input type="checkbox" bind:checked={includeSpecial} />
            Include special characters
          </label>
          <label>
            Emergency exits per day
            <input type="number" min="1" max="48" bind:value={maxPerDay} />
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
            <input type="number" min="1" max="15" bind:value={overlayAutoCloseMinutes} />
          </label>
          <button type="submit">Save</button>
          {#if overlayAutoCloseSaved}
            <span class="hint saved">Saved</span>
          {/if}
        </form>
      {/if}

      {#if overlayDueHabitsLoaded}
        <p class="hint">
          Lists habits from the Habits tab that have reached their &ldquo;every N days&rdquo; target,
          with a button to log each one. Turn it off if others can see your screen. Not shown on
          Android when the break screen draws over other apps.
        </p>
        <label class="checkbox">
          <input
            type="checkbox"
            checked={overlayDueHabitsEnabled}
            disabled={overlayDueHabitsBusy}
            onchange={toggleOverlayDueHabits}
          />
          Show due habits on the break screen
        </label>
      {/if}

      {#if quoteApiUrlLoaded}
        <form onsubmit={saveQuoteApiUrlSetting}>
          <label class="grow">
            Quote API URL
            <input
              type="text"
              bind:value={quoteApiUrl}
              placeholder="https://api.example.com/quote"
            />
          </label>
          <button type="submit">Save</button>
          {#if quoteApiUrlSaved}
            <span class="hint saved">Saved</span>
          {/if}
        </form>
        <p class="hint">
          Shown at the end of the break screen. Leave blank to disable. Expects a JSON response with
          a quote field (e.g. <code>quote</code>/<code>content</code>/<code>text</code>, optionally
          <code>author</code>) &mdash; falls back to showing the raw response text otherwise.
        </p>
      {/if}

      {#if quoteApiAttributionLoaded}
        <form onsubmit={saveQuoteApiAttributionSetting}>
          <label class="grow">
            Quote API attribution
            <textarea
              rows="2"
              bind:value={quoteApiAttribution}
              placeholder={defaultQuoteApiAttribution}
            ></textarea>
          </label>
          <button type="submit">Save</button>
          <button type="button" onclick={resetQuoteApiAttributionToDefault}>Reset to default</button>
          {#if quoteApiAttributionSaved}
            <span class="hint saved">Saved</span>
          {/if}
        </form>
        {#if quoteApiAttribution.trim()}
          <p class="hint quote-attribution-preview">
            Preview: {@html sanitizeAttributionHtml(quoteApiAttribution)}
          </p>
        {/if}
        <p class="hint">
          Shown under the quote, credited to whichever API you're calling above. Accepts a small set
          of HTML (links and basic formatting) &mdash; anything else is stripped before it's shown.
          Leave blank to show no attribution. <strong
            >If you change the Quote API URL, check that service's own documentation and update this
            attribution to match its requirements &mdash; we are not responsible for any compliance
            issues arising from missing or incorrect attribution.</strong
          >
        </p>
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

      {#if (isAndroid || isIos) && notificationChecked}
        <div class="data-row">
          <span>Notifications: {notificationGranted ? "Granted" : "Not granted"}</span>
          {#if !notificationGranted}
            <button type="button" onclick={grantNotifications}>Enable notifications</button>
          {/if}
        </div>
        {#if isIos}
          <p class="hint">
            Required on iOS: a break notification is the only way a break can reach you while the app
            isn't open. iOS asks only once &mdash; if you declined, turn them on in the iOS Settings app
            under Reflectodoro &rarr; Notifications.
          </p>
        {:else}
          <p class="hint">
            Without it, the background timer's running indicator and the break reminder notification
            both silently don't show. Onboarding offers this grant on first run; this is here for
            anyone who skipped it or revoked it since.
          </p>
        {/if}
      {/if}

      {#if isIos}
        <p class="hint">
          On iPhone the break screen can only appear inside this app: iOS doesn't let apps cover other
          apps or open themselves. When a break starts you get a notification, and a Live Activity on
          the Lock Screen and Dynamic Island counts down to the next break &mdash; open the app to
          reflect. If Live Activities don't show, turn them on in the iOS Settings app under
          Reflectodoro.
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
          Only used when "Display over other apps" isn't granted &mdash; with it, the break screen
          itself covers everything and the notification is a plain, dismissible one. Only affects a
          break that starts while you're using another app &mdash; it can't wake or take over a
          locked screen.
        </p>
      {/if}

      {#if isAndroid && hideOverlayOnCallLoaded}
        <div class="data-row">
          <label class="checkbox">
            <input
              type="checkbox"
              checked={hideOverlayOnCallEnabled}
              disabled={hideOverlayOnCallBusy}
              onchange={toggleHideOverlayOnCall}
            />
            Hide the break screen during phone calls
          </label>
        </div>
        <p class="hint">
          While a call is ringing or in progress, the break screen steps aside so you can answer,
          and returns when the call ends. Applies from the next break.
        </p>
      {/if}

      {#if isAndroid && nightPauseLoaded}
        <div class="data-row">
          <label class="checkbox">
            <input
              type="checkbox"
              checked={nightPauseEnabled}
              disabled={nightPauseBusy}
              onchange={toggleNightPauseEnabled}
            />
            Pause breaks overnight
          </label>
        </div>
        <form onsubmit={saveNightPauseWindow}>
          <label>
            Starts at
            <input type="time" bind:value={nightPauseStart} />
          </label>
          <label>
            Ends at
            <input type="time" bind:value={nightPauseEnd} />
          </label>
          <button type="submit">Save</button>
          {#if nightPauseSaved}
            <span class="hint saved">Saved</span>
          {/if}
        </form>
        <p class="hint">
          Pomodoro mode pauses itself for this window (it can wrap past midnight, like the default
          10pm&ndash;8am) and resumes on its own at the end, exactly like picking a pause from the
          Timer tab's dropdown. To resume early, set Pomodoro back to On &mdash; it stays on for the
          rest of that night. A break already open when the window starts still runs its course, and
          nothing missed is made up afterward.
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

      {#if isMacos && macosMediaKeyFallbackLoaded}
        <div class="data-row">
          <label class="checkbox">
            <input
              type="checkbox"
              checked={macosMediaKeyFallbackEnabled}
              disabled={macosMediaKeyFallbackBusy}
              onchange={toggleMacosMediaKeyFallback}
            />
            Pause media the old way (needs Accessibility access)
          </label>
        </div>
        <p class="hint">
          {#if mediaRemoteAvailable === false}
            This Mac can't use the permission-free method, so Reflectodoro is already falling back to
            the old one automatically &mdash; you don't need this switch.
          {:else}
            Leave this off. Reflectodoro normally pauses media through a method that needs no
            permission at all. Turning this on switches to sending a Play/Pause key instead, which
            needs Accessibility access and is a blind toggle &mdash; it can resume media you'd
            already paused yourself. Only worth trying if media isn't pausing on your breaks.
          {/if}
        </p>
      {/if}
    </section>


    <section class="card" id="settings-screen-time">
      <h2>Screen time</h2>
      <p class="hint">
        Records which app has focus and for how long, so the Entries tab can show where your day
        actually went. Everything stays on this device &mdash; nothing is uploaded, and only the app's
        name is recorded, never window titles or anything you type.
      </p>
      {#if isWindows}
        <p class="hint">
          Time with the screen locked or turned off isn't counted. When the screen turns off on its own,
          the idle minutes before it (up to your power plan's screen timeout) are left out too.
        </p>
      {/if}

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

      {#if isIos}
        <p class="hint warning">
          Not available on iPhone: iOS doesn't let apps see which other apps are in use.
        </p>
      {:else if osResolved && !isWindows && !isAndroid && !isMacos}
        <p class="hint warning">
          Not captured on this platform yet &mdash; Windows, macOS and Android are the only ones recording
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
          <label>
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

    <div id="settings-app-lock" class="anchor-wrap"><AppLockCard {isAndroid} /></div>


    <section class="card" id="settings-daily-summary">
      <h2>Daily summary (local AI)</h2>
      <p class="hint">
        Adds a <strong>Summarize</strong> button to the Entries tab that sends that day's reflections
        to an AI model you run yourself &mdash; e.g. Ollama or LM Studio &mdash; through its
        OpenAI-compatible chat endpoint. Nothing is sent until you click it, and the summary isn't
        saved.
      </p>

      {#if llmSummaryLoaded}
        <form onsubmit={saveLlmSummaryBasicSetting}>
          <label class="grow">
            Endpoint URL
            <input
              type="text"
              bind:value={llmSummaryApiUrl}
              placeholder="http://localhost:11434/v1/chat/completions"
            />
          </label>
          <label>
            Model
            <input type="text" bind:value={llmSummaryModel} placeholder="llama3.1" />
          </label>
          <button type="submit">Save</button>
          {#if llmSummaryBasicSaved}
            <span class="hint saved">Saved</span>
          {/if}
        </form>
        <p class="hint">
          Leave the URL blank to turn this off. Ollama:
          <code>http://localhost:11434/v1/chat/completions</code>. LM Studio:
          <code>http://localhost:1234/v1/chat/completions</code>. The model name must match one
          installed on that server. An API key, timeout and custom prompt are under Advanced settings.
        </p>
      {/if}
    </section>

    <section class="card" id="settings-calendar">
      <h2>Calendar</h2>
      <p class="hint">
        Shows your upcoming events and holidays in an <strong>Upcoming events</strong> card on the
        home screen. Nothing is shown until you add a calendar below, and the card is hidden
        entirely when none is on. Events are fetched directly from the calendar's own address, kept
        only in memory, and never saved to disk.
      </p>

      {#if calendarLoadError}
        <p class="hint error">Couldn't load calendar settings: {calendarLoadError}</p>
      {:else if calendarLoaded}
        <h3>Holidays</h3>
        <label class="medium">
          Country
          <select value={holidayCountryCode} onchange={changeHolidayCountry}>
            <option value="">None</option>
            {#each holidayCountries as c (c.code)}
              <option value={c.code}>{c.name}</option>
            {/each}
          </select>
        </label>
        {#if !holidaySource && suggestedHolidayCountry}
          <p class="hint">Your system region looks like {suggestedHolidayCountry.name}.</p>
        {/if}
        <p class="hint">
          Uses Google's public holiday calendars. Picking a country is what turns this on.
        </p>

        <h3>Personal calendars</h3>
        {#if personalSources.length > 0}
          <ul class="paired-device-list">
            {#each personalSources as s (s.id)}
              <li>
                <label class="checkbox">
                  <input
                    type="checkbox"
                    checked={s.enabled}
                    onchange={(e) => toggleCalendarSource(s.id, e.currentTarget.checked)}
                  />
                  <span class="paired-device-name">{s.name}</span>
                </label>
                <button type="button" class="danger" onclick={() => removeCalendarSource(s.id)}>
                  Remove
                </button>
              </li>
            {/each}
          </ul>
        {/if}

        <form onsubmit={addPersonalCalendar}>
          <label class="medium">
            Name (optional)
            <input type="text" bind:value={calendarNewName} placeholder="Work" />
          </label>
          <label class="grow">
            Secret address in iCal format
            <input
              type="text"
              bind:value={calendarNewUrl}
              placeholder="https://calendar.google.com/calendar/ical/…/basic.ics"
              autocomplete="off"
              spellcheck="false"
            />
          </label>
          <button type="submit" disabled={calendarAdding || !calendarNewUrl.trim()}>
            {calendarAdding ? "Checking…" : "Add calendar"}
          </button>
        </form>
        {#if calendarMessage}
          <p class="hint" class:error={calendarMessageIsError} class:saved={!calendarMessageIsError}>
            {calendarMessage}
          </p>
        {/if}
        <p class="hint">
          In Google Calendar, open <strong>Settings</strong>, pick the calendar under
          <em>Settings for my calendars</em>, then copy <strong>Secret address in iCal format</strong>
          from <em>Integrate calendar</em>. Any iCal link (<code>https://</code> or
          <code>webcal://</code>) works. <strong>Anyone with that link can read the calendar</strong>,
          so it's stored encrypted on this device only, and is never exported, synced or logged. To
          revoke it, reset the secret address in Google Calendar.
        </p>

        <h3>Display</h3>
        <form onsubmit={saveCalendarPreferences}>
          <label>
            Look ahead (days)
            <input
              type="number"
              bind:value={calendarLookaheadDays}
              min={CALENDAR_MIN_LOOKAHEAD_DAYS}
              max={CALENDAR_MAX_LOOKAHEAD_DAYS}
            />
          </label>
          <label>
            Max events shown
            <input
              type="number"
              bind:value={calendarMaxItems}
              min={CALENDAR_MIN_MAX_ITEMS}
              max={CALENDAR_MAX_MAX_ITEMS}
            />
          </label>
          <label class="checkbox">
            <input type="checkbox" bind:checked={calendarHolidayPublicOnly} />
            Public holidays only (hide observances)
          </label>
          <button type="submit">Save</button>
          {#if calendarPrefsSaved}
            <span class="hint saved">Saved</span>
          {/if}
        </form>
      {/if}
    </section>

    {#if !isMobile}
      <section class="card" id="settings-auto-pause">
        <h2>Auto-pause pomodoro on wakeup after sleep</h2>
        {#if autoPauseOnWakeLoaded}
          <div class="data-row">
            <label class="checkbox">
              <input
                type="checkbox"
                checked={autoPauseOnWakeEnabled}
                disabled={autoPauseOnWakeBusy}
                onchange={toggleAutoPauseOnWakeEnabled}
              />
              Auto-pause after waking from sleep near a boundary
            </label>
          </div>
          {#if isWindows}
            <div class="data-row">
              <label class="checkbox">
                <input
                  type="checkbox"
                  checked={autoPauseOnWakeIncludeScreenOff}
                  disabled={autoPauseOnWakeBusy || !autoPauseOnWakeEnabled}
                  onchange={toggleAutoPauseOnWakeIncludeScreenOff}
                />
                Also count time with the screen locked or turned off
              </label>
            </div>
          {/if}
          <form onsubmit={saveAutoPauseOnWakeThresholds}>
            <label>
              PC was off for more than
              <input type="number" min="1" max="180" bind:value={autoPauseOnWakeOffMinutes} />
              minutes
            </label>
            <label>
              and less than
              <input type="number" min="1" max="60" bind:value={autoPauseOnWakeRemainingMinutes} />
              minutes remain in the current session
            </label>
            <label>
              Pause for
              <input type="number" min="5" max="240" bind:value={autoPauseOnWakePauseMinutes} />
              minutes
            </label>
            <button type="submit">Save</button>
            {#if autoPauseOnWakeSaved}
              <span class="hint saved">Saved</span>
            {/if}
          </form>
          <p class="hint">
            If you reopen this device after it's been asleep for a while{isWindows &&
            autoPauseOnWakeIncludeScreenOff
              ? " (or locked, or with its screen off)"
              : ""}, right before a work or break boundary, Pomodoro mode pauses itself for the duration above instead of dropping
            you straight into a session you never chose to start. Resumes on its own, exactly like
            picking a pause from the Timer tab's dropdown &mdash; to resume early, set Pomodoro back
            to On.
          </p>
        {/if}
      </section>
    {/if}

    <section class="card" id="settings-wellness-checkin">
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


    {#if !isMobile}
    <section class="card" id="settings-stuck-break-screen">
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



    {#if !isMobile}
    <section class="card" id="settings-startup">
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
    {/if}

  

    <section class="card" id="settings-data">
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

    <section class="card" id="settings-paired-devices">
      <h2>Paired devices</h2>
      <p class="hint">
        Sync reflections, task lists, wellness check-ins, and screen time directly with another
        device on the same wifi network &mdash; no account, no cloud. Settings are never included.
        {#if !isIos}
          <br/> For auto-sync to work, both laptop and phone should be active at start of break.
        {/if}
      </p>
      {#if isIos}
        <p class="hint">
          On iPhone this is manual only, and works while Reflectodoro is open &mdash; iOS suspends
          apps in the background, so a sync started on your laptop won't reach a phone in your
          pocket. The first time it runs, iOS asks for Local Network permission; if that was
          declined, turn it back on in iOS Settings &rarr; Reflectodoro &rarr; Local Network, then
          reopen this page. A denied permission looks exactly like an empty network here: every
          device stays offline.
        </p>
      {/if}

      {#if pairedDevicesLoaded && pairedDevices.length > 0}
        <ul class="paired-device-list">
          {#each pairedDevices as device (device.deviceId)}
            <li>
              <span class="paired-device-status" class:online={device.online} title={device.online ? "Online" : "Offline"}
              ></span>
              <span class="paired-device-name">{deviceLabel(device.name, device.deviceId)} <span class="hint">({device.platform})</span></span>
              <span class="hint">Last synced: {formatLastSync(device.lastSyncAt)}</span>
              {#if !isIos}
                <label class="checkbox auto-sync-checkbox">
                  <input
                    type="checkbox"
                    checked={device.autoSyncEnabled}
                    disabled={autoSyncBusyId === device.deviceId}
                    onchange={() => toggleDeviceAutoSync(device)}
                  />
                  Auto-sync
                </label>
              {/if}
              <button
                type="button"
                onclick={() => runDeviceSync(device)}
                disabled={syncingDeviceId !== null || !device.online}
                title={device.online ? "" : "Device is offline"}
              >
                {syncingDeviceId === device.deviceId ? "Syncing…" : "Sync"}
              </button>
              <button type="button" class="danger" onclick={() => removePairedDevice(device)}>Forget</button>
            </li>
          {/each}
        </ul>
        {#if syncStatus === "success"}
          <p class="hint saved">{syncMessage}</p>
        {:else if syncStatus === "error"}
          <p class="hint error">{syncMessage}</p>
        {/if}
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
                <label class="medium">
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
                  <input class="pin-input" type="text" inputmode="numeric" maxlength="6" bind:value={joinPin} placeholder="123456" />
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
    </section>

    <!-- Last on the page by design: set-once, rarely-needed controls. -->
    <button
      id="settings-advanced"
      type="button"
      class="advanced-toggle"
      aria-expanded={advancedOpen}
      aria-controls="advanced-settings"
      onclick={toggleAdvanced}
    >
      {advancedOpen ? "Hide advanced settings" : "Advanced settings"}
    </button>
    {#if advancedOpen}
      <div id="advanced-settings" class="advanced">
        {#if !isAndroid}
          <EncryptionKeyCard />
        {/if}

        {#if llmSummaryLoaded}
          <section class="card">
            <h2>Daily summary: advanced</h2>
            <form onsubmit={saveLlmSummaryAdvancedSetting}>
              <label class="grow">
                API key (optional)
                <input
                  type="password"
                  autocomplete="off"
                  bind:value={llmSummaryApiKey}
                  placeholder="Leave blank for Ollama / LM Studio"
                />
              </label>
              <label>
                Timeout (seconds)
                <input
                  type="number"
                  min={LLM_SUMMARY_MIN_TIMEOUT_SECS}
                  max={LLM_SUMMARY_MAX_TIMEOUT_SECS}
                  bind:value={llmSummaryTimeoutSecs}
                />
              </label>
              <label class="grow">
                System prompt
                <textarea
                  rows="5"
                  bind:value={llmSummarySystemPrompt}
                  placeholder={defaultLlmSummarySystemPrompt}
                ></textarea>
              </label>
              <div class="data-row">
                <button type="submit">Save</button>
                <button type="button" onclick={resetLlmSummarySystemPromptToDefault}>
                  Reset prompt to default
                </button>
                {#if llmSummaryAdvancedSaved}
                  <span class="hint saved">Saved</span>
                {/if}
              </div>
            </form>
            <p class="hint">
              The API key is sent as a Bearer token, only if set, and is stored unencrypted on this
              device like other settings. Local models can be slow on long days &mdash; raise the
              timeout ({LLM_SUMMARY_MIN_TIMEOUT_SECS}&ndash;{LLM_SUMMARY_MAX_TIMEOUT_SECS}s) if
              summaries time out. Leave the prompt blank to use the built-in one shown as the
              placeholder. Each entry is sent as
              <code>HH:MM&ndash;HH:MM (duration): what you wrote</code>.
            </p>
          </section>
        {/if}

        <section class="card">
          <h2>Delete data</h2>
          <p class="hint">
            Free up space or start fresh. Deletes reflections, wellness check-ins, task lists and screen
            time on this device only. Settings, paired devices and the encryption key are kept.
            &ldquo;Older than&rdquo; never touches habit logs (their history drives each habit's stats);
            &ldquo;Delete all&rdquo; removes habits too.
          </p>

          <div class="data-row">
            <label>
              Delete entries older than
              <input type="number" min="1" step="1" bind:value={deleteOlderDays} style="width: 5em" />
              days
            </label>
            <button type="button" class="danger" disabled={deleteBusy} onclick={runDeleteOlder}>
              Delete older entries&hellip;
            </button>
          </div>

          <div class="data-row">
            <button type="button" class="danger" disabled={deleteBusy} onclick={runDeleteAll}>
              Delete all data&hellip;
            </button>
          </div>

          {#if deleteStatus === "checking" || deleteStatus === "cancelled"}
            <p class="hint">{deleteMessage}</p>
          {:else if deleteStatus === "success"}
            <p class="hint saved">{deleteMessage}</p>
          {:else if deleteStatus === "error"}
            <p class="hint error">{deleteMessage}</p>
          {/if}
        </section>
      </div>
    {/if}
  </div>
</div>

<style>
  /* Two columns: the section navigation on the left, the cards on the right.
     minmax(0, 1fr) lets the content column shrink instead of forcing the
     page wider than <main>. */
  .page {
    padding: 24px;
    display: grid;
    grid-template-columns: 190px minmax(0, 1fr);
    gap: 24px;
    align-items: start;
    max-width: 1200px;
    margin: 0 auto;
  }

  .content {
    display: flex;
    flex-direction: column;
    gap: 20px;
    min-width: 0;
  }

  /* Large windows: scale the whole page (text, controls, spacing) up together
     rather than overriding each hard-coded px size. The cap is divided by the
     same factor so the rendered width stays 1200px. min-width only, so small
     windows and Android keep the base sizes. */
  @media (min-width: 1200px) {
    .page {
      zoom: 1.15;
      max-width: calc(1200px / 1.15);
    }
  }

  /* Sticks within the layout's scrolling <main>, so it stays in view while
     the cards scroll past. Scrolls itself if the list is taller than the
     window. */
  .section-nav {
    position: sticky;
    top: 24px;
    border-right: 1px solid var(--border);
  }

  .nav-track {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 2px;
    max-height: calc(100vh - 48px);
    overflow-y: auto;
    padding-right: 4px;
  }

  .section-nav a {
    flex: none;
    padding: 7px 12px;
    border-radius: 8px;
    color: var(--text-dim);
    font-size: 13px;
    text-decoration: none;
  }

  .section-nav a:hover,
  .section-nav a:focus-visible {
    background: var(--surface-2);
    color: var(--text);
  }

  .section-nav a.active {
    background: var(--accent-soft);
    color: var(--accent);
    font-weight: 600;
  }

  /* A little air above a jumped-to card's heading. */
  .content > [id^="settings-"] {
    scroll-margin-top: 24px;
  }

  /* Matches the page zoom on large windows (see .page). vh is scaled by the
     zoom too, so undo it to keep the panel's bottom on screen. */
  @media (min-width: 1200px) {
    .section-nav {
      top: calc(24px / 1.15);
    }

    .nav-track {
      max-height: calc((100vh - 48px) / 1.15);
    }
  }

  /* Phones and narrow windows: a side column would leave the cards too
     cramped, so the navigation becomes a single sticky row above them that
     scrolls sideways. */
  @media (max-width: 700px) {
    .page {
      grid-template-columns: minmax(0, 1fr);
      gap: 20px;
    }

    .section-nav {
      top: 0;
      z-index: 2;
      margin: -24px -24px 0;
      background: var(--bg);
      border-right: none;
      border-bottom: 1px solid var(--border);
    }

    /* Swipeable row with no visible scrollbar; the edge fades (set from
       script as the row scrolls) are what tell you there's more. The mask is
       on this inner track, not the nav, so the bar's own background and
       bottom border stay solid. */
    .nav-track {
      --fade-start: 0px;
      --fade-end: 0px;
      flex-direction: row;
      gap: 6px;
      max-height: none;
      overflow-x: auto;
      overflow-y: hidden;
      padding: 12px 24px;
      scrollbar-width: none;
      -webkit-overflow-scrolling: touch;
      overscroll-behavior-x: contain;
      scroll-padding-inline: 24px;
      -webkit-mask-image: linear-gradient(
        to right,
        transparent 0,
        #000 var(--fade-start),
        #000 calc(100% - var(--fade-end)),
        transparent 100%
      );
      mask-image: linear-gradient(
        to right,
        transparent 0,
        #000 var(--fade-start),
        #000 calc(100% - var(--fade-end)),
        transparent 100%
      );
    }

    .nav-track::-webkit-scrollbar {
      display: none;
    }

    .nav-track.fade-start {
      --fade-start: 32px;
    }

    .nav-track.fade-end {
      --fade-end: 32px;
    }

    .section-nav a {
      padding: 5px 12px;
      border-radius: 999px;
      border: 1px solid var(--border);
      background: var(--surface);
      white-space: nowrap;
    }

    .section-nav a.active {
      border-color: transparent;
    }

    .content > [id^="settings-"] {
      scroll-margin-top: 72px;
    }
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
    width: 320px;
    max-width: 100%;
  }

  label.medium {
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
    width: 240px;
    max-width: 100%;
  }

  label.grow input[type="text"] {
    width: 100%;
  }

  input.pin-input {
    width: 100px;
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

  button.advanced-toggle {
    align-self: flex-start;
    background: var(--surface-2);
    color: var(--text);
    border: 1px solid var(--border);
  }

  .advanced {
    display: flex;
    flex-direction: column;
    gap: 20px;
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
    width: 240px;
    max-width: 100%;
  }

  label.grow select,
  label.medium select {
    width: 100%;
    max-width: 420px;
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
