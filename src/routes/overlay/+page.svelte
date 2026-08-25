<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import {
    findMissedSlots,
    saveReflection,
    getTaskList,
    saveTaskList,
    localDateStamp,
    listenForTaskListUpdates,
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
  let missedSlots = $state<string[]>([]);
  let nowTick = $state(Date.now());

  let unlisten: UnlistenFn | null = null;
  let unlistenTasks: UnlistenFn | null = null;
  let taskSaveTimer: ReturnType<typeof setTimeout> | null = null;
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

  async function devClose() {
    await invoke("dev_force_close");
  }

  onMount(async () => {
    devMode = await invoke<boolean>("is_dev_mode");
    overlayState = await invoke<OverlayState>("get_overlay_state");
    await refreshCoverage();
    taskListContent = await getTaskList(localDateStamp());

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

    tickInterval = setInterval(() => (nowTick = Date.now()), 1000);
  });

  onDestroy(() => {
    unlisten?.();
    unlistenTasks?.();
    if (tickInterval) clearInterval(tickInterval);
    if (taskSaveTimer) clearTimeout(taskSaveTimer);
  });
</script>

<div class="overlay">
  {#if devMode}
    <button class="dev-close" onclick={devClose}>Close (DEV)</button>
  {/if}

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
            placeholder="type the code above, then press Enter"
          />
        {/if}
        <p class="hint">Reflection is still required either way.</p>
      </div>
    </section>

    <section class="panel side">
      <h2>Most Important Tasks Today</h2>
      <textarea
        bind:value={taskListContent}
        oninput={scheduleTaskSave}
        placeholder="1.&#10;2.&#10;3."
        rows="10"
      ></textarea>
      <p class="hint">Auto-saves as you type.</p>
    </section>
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
    align-items: center;
    justify-content: center;
    font-family: Inter, Avenir, Helvetica, Arial, sans-serif;
    user-select: none;
  }

  .dev-close {
    position: absolute;
    top: 12px;
    right: 12px;
    background: #b3261e;
    color: white;
    border: none;
    border-radius: 6px;
    padding: 6px 12px;
    cursor: pointer;
    z-index: 10;
  }

  .grid {
    display: grid;
    grid-template-columns: 2fr 1fr;
    gap: 24px;
    width: min(1000px, 90vw);
  }

  .panel {
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 16px;
    padding: 28px;
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
