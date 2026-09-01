<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import {
    findMissedSlots,
    saveReflection,
    getTaskList,
    saveTaskList,
    getNotToDoList,
    saveNotToDoList,
    localDateStamp,
    listenForTaskListUpdates,
    listenForNotToDoListUpdates,
  } from "$lib/db";

  interface OverlayState {
    open: boolean;
    reflection_entered: boolean;
    breakit_challenge: string;
    breakit_matched: boolean;
    time_expired: boolean;
    current_slot_start: string;
  }

  let overlayState = $state<OverlayState | null>(null);
  let devMode = $state(false);
  let reflectionText = $state("");
  let breakitInput = $state("");
  let breakitShake = $state(false);
  let taskListContent = $state("");
  let notToDoContent = $state("");
  let missedSlots = $state<string[]>([]);
  let nowTick = $state(Date.now());
  // How much of the viewport the on-screen keyboard is currently covering.
  // Android's WebView doesn't reliably auto-scroll a focused input above the
  // keyboard the way native views do, so this pads the scroll area instead
  // (see updateKeyboardInset/scrollFieldIntoView below).
  let keyboardInset = $state(0);

  let unlisten: UnlistenFn | null = null;
  let unlistenTasks: UnlistenFn | null = null;
  let unlistenNotToDo: UnlistenFn | null = null;
  let taskSaveTimer: ReturnType<typeof setTimeout> | null = null;
  let notToDoSaveTimer: ReturnType<typeof setTimeout> | null = null;
  let tickInterval: ReturnType<typeof setInterval> | null = null;

  const promptLabel = $derived(
    missedSlots.length > 1 ? `last ${missedSlots.length} pomodoros` : "last 1 pomodoro",
  );

  const remainingSeconds = $derived.by(() => {
    if (!overlayState?.current_slot_start) return 0;
    const end = new Date(overlayState.current_slot_start).getTime() + 5 * 60 * 1000;
    return Math.max(0, Math.round((end - nowTick) / 1000));
  });

  const remainingLabel = $derived.by(() => {
    const m = Math.floor(remainingSeconds / 60);
    const s = remainingSeconds % 60;
    return `${m}:${String(s).padStart(2, "0")}`;
  });

  const clockLabel = $derived.by(() =>
    new Date(nowTick).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }),
  );

  async function refreshCoverage() {
    if (!overlayState?.current_slot_start) return;
    missedSlots = await findMissedSlots(overlayState.current_slot_start);
  }

  async function submitReflection(e: Event) {
    e.preventDefault();
    if (!reflectionText.trim() || !overlayState) return;
    await saveReflection(missedSlots, reflectionText.trim());
    overlayState = await invoke<OverlayState>("mark_reflection_entered");
  }

  function updateKeyboardInset() {
    const vv = window.visualViewport;
    if (!vv) return;
    keyboardInset = Math.max(0, window.innerHeight - vv.height);
  }

  /** Gives the on-screen keyboard's resize/animation a moment to start
   * before scrolling -- scrolling immediately on focus can land at the
   * pre-keyboard scroll position instead of the post-keyboard one. */
  function scrollFieldIntoView(e: FocusEvent) {
    const el = e.currentTarget as HTMLElement;
    setTimeout(() => el.scrollIntoView({ block: "center", behavior: "smooth" }), 150);
  }

  function blockPaste(e: ClipboardEvent) {
    e.preventDefault();
  }
  function blockDrop(e: DragEvent) {
    e.preventDefault();
  }
  function blockContextMenu(e: MouseEvent) {
    e.preventDefault();
  }

  async function onBreakitKeydown(e: KeyboardEvent) {
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "v") {
      e.preventDefault();
      return;
    }
    if (e.shiftKey && e.key === "Insert") {
      e.preventDefault();
      return;
    }
    if (e.key !== "Enter") return;
    e.preventDefault();
    const val = breakitInput.trim();
    breakitInput = "";
    if (!val || !overlayState) return;
    const wasMatched = overlayState.breakit_matched;
    overlayState = await invoke<OverlayState>("breakit_attempt", { input: val });
    if (!overlayState.breakit_matched && !wasMatched) {
      breakitShake = true;
      setTimeout(() => (breakitShake = false), 300);
    }
  }

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

  async function devClose() {
    await invoke("dev_force_close");
  }

  onMount(async () => {
    devMode = await invoke<boolean>("is_dev_mode");
    overlayState = await invoke<OverlayState>("get_overlay_state");
    await refreshCoverage();
    taskListContent = await getTaskList(localDateStamp());
    notToDoContent = await getNotToDoList(localDateStamp());

    unlisten = await listen<OverlayState>("overlay://state", async (event) => {
      const prevSlot = overlayState?.current_slot_start;
      overlayState = event.payload;
      if (overlayState.current_slot_start !== prevSlot) {
        reflectionText = "";
        breakitInput = "";
        await refreshCoverage();
      }
    });
    unlistenTasks = await listenForTaskListUpdates((content) => {
      taskListContent = content;
    });
    unlistenNotToDo = await listenForNotToDoListUpdates((content) => {
      notToDoContent = content;
    });

    tickInterval = setInterval(() => (nowTick = Date.now()), 1000);

    window.visualViewport?.addEventListener("resize", updateKeyboardInset);
    updateKeyboardInset();
  });

  onDestroy(() => {
    unlisten?.();
    unlistenTasks?.();
    unlistenNotToDo?.();
    if (tickInterval) clearInterval(tickInterval);
    if (taskSaveTimer) clearTimeout(taskSaveTimer);
    if (notToDoSaveTimer) clearTimeout(notToDoSaveTimer);
    window.visualViewport?.removeEventListener("resize", updateKeyboardInset);
  });
</script>

<div class="overlay" style="--kb-inset: {keyboardInset}px">
  {#if devMode}
    <button class="dev-close" onclick={devClose}>Close (DEV)</button>
  {/if}

  <p class="clock">{clockLabel}</p>

  <div class="grid">
    <section class="panel primary">
      <p class="timer">{remainingSeconds > 0 ? remainingLabel : "Time's up"}</p>

      <h1>What did I do in the {promptLabel}?</h1>
      <form onsubmit={submitReflection}>
        <textarea
          bind:value={reflectionText}
          placeholder="Write a couple of bullet points or type 'Skip' to skip it. "
          rows="5"
          disabled={overlayState?.reflection_entered}
          onfocus={scrollFieldIntoView}
        ></textarea>
        {#if overlayState?.reflection_entered}
          <p class="hint ok">Saved. Waiting on the other condition to finish the break.</p>
        {:else}
          <button type="submit" disabled={!reflectionText.trim()}>Submit reflection</button>
        {/if}
      </form>

      <div class="breakit">
        {#if overlayState?.breakit_matched}
          <p class="hint ok">Code matched.</p>
        {:else}
          <label for="breakit-field">To close this window early, type this code and press Enter</label>
          <p class="challenge">{overlayState?.breakit_challenge ?? ""}</p>
          <input
            id="breakit-field"
            type="text"
            autocomplete="off"
            spellcheck="false"
            class:shake={breakitShake}
            bind:value={breakitInput}
            onkeydown={onBreakitKeydown}
            onpaste={blockPaste}
            ondrop={blockDrop}
            oncontextmenu={blockContextMenu}
            onfocus={scrollFieldIntoView}
            placeholder="type the code above, then press Enter"
          />
        {/if}
        <p class="hint">Reflection is still required either way.</p>
      </div>
    </section>

    <div class="side-col">
      <section class="panel side">
        <h2>Most Important Tasks Today</h2>
        <textarea
          bind:value={taskListContent}
          oninput={scheduleTaskSave}
          placeholder="1.&#10;2.&#10;3."
          rows="5"
          onfocus={scrollFieldIntoView}
        ></textarea>
        <p class="hint">Auto-saves as you type.</p>
      </section>

      <section class="panel side">
        <h2>Not To Do Tasks Today</h2>
        <textarea
          bind:value={notToDoContent}
          oninput={scheduleNotToDoSave}
          placeholder="1.&#10;2.&#10;3."
          rows="3"
          onfocus={scrollFieldIntoView}
        ></textarea>
        <p class="hint">Auto-saves as you type.</p>
      </section>
    </div>
  </div>
</div>

<style>
  :global(html, body) {
    margin: 0;
    height: 100%;
    background: #0b0b12;
  }

  .overlay {
    position: fixed;
    inset: 0;
    background: linear-gradient(160deg, #10111a 0%, #1b1c2b 100%);
    color: #f3f3f7;
    display: flex;
    flex-direction: column;
    align-items: center;
    font-family: Inter, Avenir, Helvetica, Arial, sans-serif;
    user-select: none;
    box-sizing: border-box;
    /* Scrollable rather than a hard-centered box: on a short/narrow phone,
       or once the keyboard opens, the reflection/breakit content can be
       taller than the viewport, and the mandatory reflection field must
       stay reachable either way. --kb-inset is set from JS (see
       updateKeyboardInset) since Android's WebView doesn't reliably shrink
       the layout viewport under an open keyboard the way it does for
       visualViewport. */
    overflow-y: auto;
    padding: calc(20px + var(--safe-top)) calc(20px + var(--safe-right))
      calc(20px + var(--safe-bottom) + var(--kb-inset, 0px)) calc(20px + var(--safe-left));
  }

  .dev-close {
    /* Fixed to the viewport, not .overlay's (scrollable) content box, so it
       stays put while the user scrolls to reach the submit button. */
    position: fixed;
    top: calc(12px + var(--safe-top));
    right: calc(12px + var(--safe-right));
    background: #b3261e;
    color: white;
    border: none;
    border-radius: 6px;
    padding: 6px 12px;
    cursor: pointer;
    z-index: 10;
  }

  .clock {
    position: fixed;
    top: calc(16px + var(--safe-top));
    left: calc(20px + var(--safe-left));
    margin: 0;
    font-variant-numeric: tabular-nums;
    font-size: 13px;
    letter-spacing: 0.04em;
    opacity: 0.55;
  }

  .grid {
    display: grid;
    grid-template-columns: 2fr 1fr;
    gap: 24px;
    width: min(1000px, 90vw);
    /* Vertically centers when it fits; unlike justify-content: center on
       the parent, margin: auto on a flex child keeps the top/bottom edges
       reachable by scroll once content is taller than the viewport. */
    margin: auto 0;
  }

  .panel {
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 16px;
    padding: 28px;
  }

  .side-col {
    display: flex;
    flex-direction: column;
    gap: 20px;
  }

  @media (max-width: 600px) {
    .grid {
      grid-template-columns: 1fr;
      width: 100%;
      gap: 16px;
    }

    .panel {
      padding: 20px;
    }
  }

  .timer {
    font-variant-numeric: tabular-nums;
    font-size: 14px;
    opacity: 0.7;
    margin: 0 0 8px;
  }

  h1 {
    font-size: 22px;
    margin: 0 0 16px;
  }

  h2 {
    font-size: 16px;
    margin: 0 0 12px;
    opacity: 0.85;
  }

  textarea,
  input {
    width: 100%;
    box-sizing: border-box;
    background: rgba(0, 0, 0, 0.25);
    border: 1px solid rgba(255, 255, 255, 0.15);
    border-radius: 10px;
    color: inherit;
    font-family: inherit;
    font-size: 14px;
    padding: 10px 12px;
    resize: vertical;
  }

  button {
    margin-top: 10px;
    background: #5865f2;
    color: white;
    border: none;
    border-radius: 8px;
    padding: 10px 18px;
    font-size: 14px;
    cursor: pointer;
  }

  button:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .breakit {
    margin-top: 24px;
    padding-top: 20px;
    border-top: 1px solid rgba(255, 255, 255, 0.08);
  }

  label {
    display: block;
    font-size: 13px;
    opacity: 0.8;
    margin-bottom: 8px;
  }

  .hint {
    font-size: 12px;
    opacity: 0.6;
    margin-top: 8px;
  }

  .hint.ok {
    color: #8fd19e;
  }

  .challenge {
    font-family: "Cascadia Code", "Consolas", ui-monospace, monospace;
    font-size: 20px;
    letter-spacing: 0.12em;
    background: rgba(0, 0, 0, 0.35);
    border: 1px solid rgba(255, 255, 255, 0.15);
    border-radius: 8px;
    padding: 12px 14px;
    margin: 0 0 12px;
    word-break: break-all;
  }

  input.shake {
    animation: shake 0.3s;
    border-color: #e15b5b;
  }

  @keyframes shake {
    0%,
    100% {
      transform: translateX(0);
    }
    25% {
      transform: translateX(-6px);
    }
    75% {
      transform: translateX(6px);
    }
  }
</style>
