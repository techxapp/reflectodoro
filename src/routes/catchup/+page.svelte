<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { info } from "@tauri-apps/plugin-log";
  import {
    findMissedSlots,
    saveReflection,
    getTaskList,
    saveTaskList,
    localDateStamp,
    listenForTaskListUpdates,
  } from "$lib/db";

  let slots = $state<string[]>([]);
  let text = $state("");
  let saving = $state(false);
  let ready = $state(false);
  let taskListContent = $state("");

  let unlistenSlot: UnlistenFn | null = null;
  let unlistenTasks: UnlistenFn | null = null;
  let taskSaveTimer: ReturnType<typeof setTimeout> | null = null;

  const promptLabel = $derived(
    slots.length > 1 ? `last ${slots.length} pomodoros` : "last pomodoro",
  );

  async function loadForSlot(slot: string) {
    text = "";
    saving = false;
    slots = await findMissedSlots(slot);
    ready = true;
    void info(`catchup: loaded slot=${slot}, covering ${slots.length} slot(s)`);
  }

  function scheduleTaskSave() {
    if (taskSaveTimer) clearTimeout(taskSaveTimer);
    taskSaveTimer = setTimeout(() => {
      void saveTaskList(localDateStamp(), taskListContent);
    }, 800);
  }

  onMount(async () => {
    // This window is hidden rather than destroyed when dismissed (so its
    // webview stays warm), meaning it mounts once per app run -- often
    // *before* the main window's startup check has even run, since both are
    // racing DB calls of similar cost. The listener MUST be attached before
    // the fallback invoke below: `open_catchup_window` (commands.rs) sets
    // `catchup_slot` state and then emits "catchup://slot" in that order, so
    // once the listener is live, any emit that already fired is still
    // covered by the invoke's read of the now-set state -- see
    // best_practices.md race #2. Attaching the listener after the invoke, as
    // this used to, left a gap where an emit could land in between and be
    // silently dropped, which is what made this window come up blank.
    void info("catchup: onMount, attaching listener before fallback invoke");
    unlistenSlot = await listen<string>("catchup://slot", (event) => {
      void info(`catchup: received catchup://slot event, slot=${event.payload}`);
      void loadForSlot(event.payload);
    });
    unlistenTasks = await listenForTaskListUpdates((content) => {
      taskListContent = content;
    });

    const slot = await invoke<string | null>("get_catchup_slot");
    void info(`catchup: fallback get_catchup_slot -> ${slot}`);
    if (slot) await loadForSlot(slot);
    taskListContent = await getTaskList(localDateStamp());
  });

  onDestroy(() => {
    unlistenSlot?.();
    unlistenTasks?.();
    if (taskSaveTimer) clearTimeout(taskSaveTimer);
  });

  async function submit(e: Event) {
    e.preventDefault();
    if (!text.trim() || slots.length === 0 || saving) return;
    saving = true;
    try {
      await saveReflection(slots, text.trim());
      // slots is oldest-first (see findMissedSlots) -- the last entry is the
      // current/most-recent slot, the only one the check-in popup asks about.
      await invoke("open_checkin_window", { slotStartIso: slots[slots.length - 1] });
      await getCurrentWindow().close();
    } finally {
      // If saving or closing failed, the window is still open -- don't
      // leave the button stuck disabled with no way to retry.
      saving = false;
    }
  }

  async function dismiss() {
    await getCurrentWindow().close();
  }
</script>

<div class="catchup">
  {#if ready}
    <section class="panel primary">
      <h1>You didn't log your {promptLabel}</h1>
      <p class="hint">What did you do? Closing this window without saving skips it.</p>
      <form onsubmit={submit}>
        <textarea bind:value={text} rows="5" placeholder="Write a couple of bullet points or type 'Skip' to skip it"
        ></textarea>
        <div class="actions">
          <button type="button" class="secondary" onclick={dismiss}>Dismiss</button>
          <button type="submit" disabled={!text.trim() || saving}>Save &amp; close</button>
        </div>
      </form>
    </section>

    <section class="panel side">
      <h2>Most Important Tasks Today</h2>
      <textarea
        bind:value={taskListContent}
        oninput={scheduleTaskSave}
        placeholder="1.&#10;2.&#10;3."
      ></textarea>
      <p class="hint">Shared with Timer &amp; the break overlay.</p>
    </section>
  {/if}
</div>

<style>
  .catchup {
    padding: 20px;
    height: 100%;
    box-sizing: border-box;
    display: grid;
    grid-template-columns: 3fr 2fr;
    gap: 16px;
  }

  .panel {
    display: flex;
    flex-direction: column;
    min-height: 0;
  }

  h1 {
    font-size: 16px;
    margin: 0 0 6px;
  }

  h2 {
    font-size: 14px;
    margin: 0 0 8px;
    opacity: 0.85;
  }

  .hint {
    font-size: 12px;
    color: var(--text-dim);
    margin: 0 0 14px;
  }

  form {
    display: flex;
    flex-direction: column;
    flex: 1;
  }

  textarea {
    width: 100%;
    box-sizing: border-box;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 10px;
    color: inherit;
    font-size: 14px;
    padding: 10px 12px;
    resize: none;
    flex: 1;
  }

  .side textarea {
    margin-bottom: 8px;
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 12px;
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
