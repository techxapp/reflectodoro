<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { goto } from "$app/navigation";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { info } from "@tauri-apps/plugin-log";
  import {
    canonicalIso,
    getCheckinAutoCloseMinutes,
    getReflectionIdForSlot,
    getWellnessTextExclusions,
    parseWellnessTextExclusions,
    saveWellnessCheck,
    syncLastWellnessCheckAtToBackend,
    type WellnessCheckValues,
  } from "$lib/db";

  const DEFAULTS: WellnessCheckValues = {
    relaxedEyes: true,
    exercise: true,
    drankWater: true,
    washroom: false,
  };

  interface ToggleField {
    key: keyof WellnessCheckValues;
    label: string;
  }

  const fields: ToggleField[] = [
    { key: "relaxedEyes", label: "Relaxed eyes" },
    { key: "exercise", label: "Exercise" },
    { key: "drankWater", label: "Drank water" },
    { key: "washroom", label: "Washroom" },
  ];

  let slot = $state<string | null>(null);
  let values = $state<WellnessCheckValues>({ ...DEFAULTS });
  let ready = $state(false);
  let saving = $state(false);
  let error = $state("");
  let excludedLabels = $state<string[]>([]);
  // Raw (pre-canonicalization) slot string last loaded, so a focus resync
  // (see onFocusChanged below) can tell "still the same occurrence" from
  // "backend has moved on to a slot we never actually loaded" without
  // re-running loadForSlot (and wiping in-progress toggles) on every focus.
  let lastRawSlot: string | null = null;

  let unlistenSlot: UnlistenFn | null = null;
  let unlistenFocus: UnlistenFn | null = null;
  let autoCloseTimer: ReturnType<typeof setTimeout> | null = null;
  // loadForSlot awaits scheduleAutoClose, and both the "checkin://slot"
  // listener and the onMount fallback invoke call loadForSlot on the same
  // mount (see the comment on that race below) -- so two calls can overlap
  // here. clearAutoCloseTimer() at the top of each is a no-op against a timer
  // the *other* call hasn't created yet, so without this guard the earlier
  // call's setTimeout survives un-tracked once the later call's assignment
  // overwrites `autoCloseTimer`, and it still fires skip() later even after
  // a real user interaction calls clearAutoCloseTimer() on what it thinks is
  // the only outstanding timer.
  let autoCloseGeneration = 0;

  function clearAutoCloseTimer() {
    autoCloseGeneration++;
    if (autoCloseTimer) {
      clearTimeout(autoCloseTimer);
      autoCloseTimer = null;
    }
  }

  async function scheduleAutoClose() {
    clearAutoCloseTimer();
    const generation = autoCloseGeneration;
    const minutes = await getCheckinAutoCloseMinutes();
    // Superseded by a newer scheduleAutoClose/clearAutoCloseTimer call while
    // this await was in flight -- don't resurrect a timer for a stale call.
    if (generation !== autoCloseGeneration) return;
    autoCloseTimer = setTimeout(() => void skip(), minutes * 60 * 1000);
  }

  function textAllowed(field: ToggleField): boolean {
    return !excludedLabels.includes(field.label.toLowerCase());
  }

  // Drag-to-slide switch. Track/knob sizing here must match the .switch/.knob
  // CSS below -- kept as plain constants rather than measured via
  // getBoundingClientRect since the layout is fixed and this avoids a
  // measure-before-first-paint race.
  const TRACK_WIDTH = 150;
  const KNOB_SIZE = 22;
  const TRACK_PAD = 3;
  const MAX_TRAVEL = TRACK_WIDTH - KNOB_SIZE - TRACK_PAD * 2;
  const DRAG_THRESHOLD = 3; // px of movement before a press counts as a drag, not a tap

  let dragKey = $state<keyof WellnessCheckValues | null>(null);
  let dragX = $state(0);
  let dragMoved = false;
  let dragStartClientX = 0;
  let dragStartOffset = 0;

  function knobOffset(key: keyof WellnessCheckValues): number {
    if (dragKey === key) return dragX;
    return values[key] ? MAX_TRAVEL : 0;
  }

  function onSwitchPointerDown(e: PointerEvent, key: keyof WellnessCheckValues) {
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    dragKey = key;
    dragMoved = false;
    dragStartClientX = e.clientX;
    dragStartOffset = values[key] ? MAX_TRAVEL : 0;
    dragX = dragStartOffset;
  }

  function onSwitchPointerMove(e: PointerEvent, key: keyof WellnessCheckValues) {
    if (dragKey !== key) return;
    const delta = e.clientX - dragStartClientX;
    if (Math.abs(delta) > DRAG_THRESHOLD) dragMoved = true;
    dragX = Math.min(MAX_TRAVEL, Math.max(0, dragStartOffset + delta));
  }

  function onSwitchPointerUp(key: keyof WellnessCheckValues) {
    if (dragKey !== key) return;
    // A real slide past the halfway point snaps to whichever side it
    // crossed toward; a press that never moved past DRAG_THRESHOLD counts as
    // a plain click/tap and just flips the current value. Either way it
    // resolves through the same $state assignment, so the knob/text still
    // animate via the normal CSS transition (only suppressed while dragging).
    values = {
      ...values,
      [key]: dragMoved ? dragX > MAX_TRAVEL / 2 : !values[key],
    };
    // Any real interaction counts as "input received" -- stop the
    // auto-close countdown so a user actively filling this out doesn't
    // get closed out from under them.
    clearAutoCloseTimer();
    dragKey = null;
  }

  function onSwitchPointerCancel(key: keyof WellnessCheckValues) {
    if (dragKey === key) dragKey = null;
  }

  async function loadForSlot(newSlot: string) {
    lastRawSlot = newSlot;
    // Rust hands us current_slot_start as a local-offset RFC3339 string, but
    // reflection.slot_start_at is always stored canonicalized to UTC (see
    // findMissedSlots in $lib/db) -- without this, the lookup in submit()
    // never string-matches and the save silently no-ops.
    slot = canonicalIso(newSlot);
    values = { ...DEFAULTS };
    saving = false;
    error = "";
    // Re-read every time rather than once at mount: this window is reused
    // across occurrences without remounting, so a Settings change made
    // between two breaks should still apply to the next one.
    excludedLabels = parseWellnessTextExclusions(await getWellnessTextExclusions());
    ready = true;
    void info(`checkin: loaded slot=${slot}`);
    await scheduleAutoClose();
  }

  onMount(async () => {
    // This window is hidden rather than destroyed when dismissed (so its
    // webview stays warm), meaning it mounts once per app run -- often
    // before whatever triggers the first check-in has even run. The
    // listener MUST be attached before the fallback invoke below: the
    // trigger (open_checkin_for_slot in overlay.rs) sets `checkin_slot`
    // state and then emits "checkin://slot" in that order, so once the
    // listener is live, any emit that already fired is still covered by the
    // invoke's read of the now-set state -- see best_practices.md race #2.
    // Attaching the listener after the invoke, as this used to, left a gap
    // where an emit could land in between and be silently dropped, leaving
    // the window blank.
    void info("checkin: onMount, attaching listener before fallback invoke");
    unlistenSlot = await listen<string>("checkin://slot", (event) => {
      void info(`checkin: received checkin://slot event, slot=${event.payload}`);
      void loadForSlot(event.payload);
    });

    const initialSlot = await invoke<string | null>("get_checkin_slot");
    void info(`checkin: fallback get_checkin_slot -> ${initialSlot}`);
    if (initialSlot) void loadForSlot(initialSlot);

    // Safety net for a missed `checkin://slot` emit (observed in the wild:
    // the window was shown/focused by Rust but the frontend never logged
    // receiving the event, leaving it permanently blank since loadForSlot
    // never ran). set_focus() is called every time spawn_checkin_window
    // shows this reused window, so re-checking the backend's slot on every
    // focus catches that case without depending on the event arriving.
    // Guarded to no-op when we're already showing that same slot, so it
    // doesn't reset in-progress toggles on an ordinary refocus.
    unlistenFocus = await getCurrentWindow().onFocusChanged(({ payload: focused }) => {
      if (!focused) return;
      void (async () => {
        const backendSlot = await invoke<string | null>("get_checkin_slot");
        if (backendSlot && (!ready || backendSlot !== lastRawSlot)) {
          void info(`checkin: focus resync found unloaded slot=${backendSlot}`);
          await loadForSlot(backendSlot);
        }
      })();
    });
  });

  onDestroy(() => {
    unlistenSlot?.();
    unlistenFocus?.();
    clearAutoCloseTimer();
  });

  /** Desktop: this window has its own close-requested handler (overlay.rs's
   * build_popup_window) that hides rather than destroys it, so .close() is
   * the right call there. Android has no separate checkin window at all
   * (see +layout.svelte) -- this same route is just navigated to within the
   * single window, so "closing" it means navigating back to "/" instead. */
  async function dismiss() {
    const os = await invoke<string>("current_os");
    if (os === "android") {
      await goto("/");
    } else {
      await getCurrentWindow().close();
    }
  }

  async function submit(e: Event) {
    e.preventDefault();
    if (!slot || saving) return;
    clearAutoCloseTimer();
    saving = true;
    error = "";
    try {
      const reflectionId = await getReflectionIdForSlot(slot);
      const createdAt = await saveWellnessCheck(reflectionId, values);
      // Resets the macOS media-toggle guard for the next break -- see
      // media.rs. Deliberately not called from skip()/auto-close: those
      // don't save a wellness_check row, so they shouldn't reset it either.
      await syncLastWellnessCheckAtToBackend(createdAt);
      await dismiss();
    } catch (e) {
      // Surface it on-page rather than failing silently -- a save that
      // just does nothing is indistinguishable from a broken button.
      error = e instanceof Error ? e.message : String(e);
    } finally {
      // If saving or closing failed, the window is still open -- don't
      // leave the button stuck disabled with no way to retry.
      saving = false;
    }
  }

  async function skip() {
    clearAutoCloseTimer();
    await dismiss();
  }
</script>

<div class="checkin">
  {#if ready}
    <section class="panel">
      <h1>Quick wellness check-in</h1>
      <p class="hint">For your last pomodoro. Closing this window without saving skips it.</p>

      <form onsubmit={submit}>
        <div class="toggles">
          {#each fields as field (field.key)}
            <div class="toggle-row">
              <span class="toggle-label">{field.label}</span>
              <span
                class="switch"
                class:on={values[field.key]}
                role="presentation"
                onpointerdown={(e) => onSwitchPointerDown(e, field.key)}
                onpointermove={(e) => onSwitchPointerMove(e, field.key)}
                onpointerup={() => onSwitchPointerUp(field.key)}
                onpointercancel={() => onSwitchPointerCancel(field.key)}
              >
                {#if textAllowed(field)}
                  <span
                    class="switch-text"
                    class:dragging={dragKey === field.key}
                    style="opacity: {1 - knobOffset(field.key) / MAX_TRAVEL}"
                  >
                    Let's Try Next Time :)
                  </span>
                {/if}
                <span
                  class="knob"
                  class:dragging={dragKey === field.key}
                  style="transform: translateX({knobOffset(field.key)}px)"
                ></span>
              </span>
            </div>
          {/each}
        </div>

        {#if error}
          <p class="error">Couldn't save: {error}</p>
        {/if}

        <div class="actions">
          <button type="button" class="secondary" onclick={skip}>Skip</button>
          <button type="submit" disabled={saving}>Save &amp; close</button>
        </div>
      </form>
    </section>
  {/if}
</div>

<style>
  .checkin {
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    box-sizing: border-box;
    overflow-y: auto;
    /* Same reasoning as overlay/+page.svelte: scrollable rather than a hard
       centered box, and insets to clear the status bar / gesture nav on
       Android (0 on desktop). */
    padding: calc(20px + var(--safe-top)) calc(20px + var(--safe-right))
      calc(20px + var(--safe-bottom)) calc(20px + var(--safe-left));
  }

  .panel {
    width: min(480px, 100%);
    margin: auto 0;
  }

  h1 {
    font-size: 18px;
    margin: 0 0 6px;
  }

  .error {
    font-size: 12px;
    color: var(--danger);
    margin: 0 0 12px;
  }

  .hint {
    font-size: 12px;
    color: var(--text-dim);
    margin: 0 0 20px;
  }

  form {
    display: flex;
    flex-direction: column;
  }

  .toggles {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-bottom: 20px;
  }

  .toggle-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    width: 100%;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 12px 14px;
    font-size: 14px;
    color: inherit;
    text-align: left;
    /* Selected text inside the switch blocks a drag started from within the
       selection (the browser starts extending the selection instead), so
       nothing in this row should be text-selectable. */
    user-select: none;
    -webkit-user-select: none;
  }

  .toggle-label {
    font-weight: 500;
  }

  /* Track/knob sizing here must match TRACK_WIDTH/KNOB_SIZE/TRACK_PAD in the
     script above -- the knob's position is driven by an inline transform
     computed from those constants, not by CSS alone. */
  .switch {
    position: relative;
    width: 150px;
    height: 28px;
    border-radius: 999px;
    background: var(--border);
    flex-shrink: 0;
    transition: background 0.15s ease;
    touch-action: none;
    cursor: grab;
  }

  .switch:active {
    cursor: grabbing;
  }

  .switch.on {
    background: var(--accent);
  }

  /* Fades in as the knob slides left (off) -- opacity is driven inline from
     the same drag position as the knob's transform, see knobOffset() above. */
  .switch-text {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0 6px 0 26px;
    font-size: 10px;
    font-weight: 600;
    letter-spacing: -0.1px;
    color: var(--text-dim);
    white-space: nowrap;
    pointer-events: none;
    transition: opacity 0.1s linear;
  }

  .switch-text.dragging {
    transition: none;
  }

  .knob {
    position: absolute;
    top: 3px;
    left: 3px;
    width: 22px;
    height: 22px;
    border-radius: 50%;
    background: var(--surface);
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.3);
    transition: transform 1s ease;
  }

  .knob.dragging {
    transition: none;
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }

  button {
    border: none;
    border-radius: 8px;
    padding: 9px 16px;
    font-size: 14px;
  }

  button[type="submit"] {
    background: var(--accent);
    color: white;
  }

  button[type="submit"]:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  button.secondary {
    background: var(--surface-2);
    color: var(--text-dim);
  }
</style>
