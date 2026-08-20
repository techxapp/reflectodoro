<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { slotFor } from "$lib/grid";
  import {
    getTaskList,
    saveTaskList,
    localDateStamp,
    loadAndSyncBreakitSettings,
    listenForTaskListUpdates,
  } from "$lib/db";

  let now = $state(new Date());
  let enabled = $state(true);
  let taskListContent = $state("");
  let unlisten: UnlistenFn | null = null;
  let unlistenTasks: UnlistenFn | null = null;
  let tickInterval: ReturnType<typeof setInterval> | null = null;
  let taskSaveTimer: ReturnType<typeof setTimeout> | null = null;

  const slot = $derived(slotFor(now));
  const remainingLabel = $derived.by(() => {
    const ms = slot.end.getTime() - now.getTime();
    const totalSec = Math.max(0, Math.round(ms / 1000));
    const m = Math.floor(totalSec / 60);
    const s = totalSec % 60;
    return `${m}:${String(s).padStart(2, "0")}`;
  });

  function scheduleTaskSave() {
    if (taskSaveTimer) clearTimeout(taskSaveTimer);
    taskSaveTimer = setTimeout(() => {
      void saveTaskList(localDateStamp(), taskListContent);
    }, 800);
  }

  async function toggleEnabled() {
    enabled = !enabled;
    await invoke("set_enabled", { enabled });
  }

  onMount(async () => {
    await loadAndSyncBreakitSettings();
    enabled = await invoke<boolean>("get_enabled");
    taskListContent = await getTaskList(localDateStamp());

    unlisten = await listen<boolean>("pomodoro://enabled-changed", (event) => {
      enabled = event.payload;
    });
    unlistenTasks = await listenForTaskListUpdates((content) => {
      taskListContent = content;
    });

    tickInterval = setInterval(() => (now = new Date()), 1000);
  });

  onDestroy(() => {
    unlisten?.();
    unlistenTasks?.();
    if (tickInterval) clearInterval(tickInterval);
    if (taskSaveTimer) clearTimeout(taskSaveTimer);
  });
</script>

<div class="page">
  <section class="card timer-card">
    <p class="label">{slot.phase === "work" ? "Working" : "On break"}</p>
    <p class="big">{remainingLabel}</p>
    <p class="sub">
      {slot.phase === "work" ? "until break" : "until work resumes"}
      &middot; sessions run :00-:25 and :30-:55, breaks :25-:30 and :55-:00
    </p>
    <button class="toggle" class:off={!enabled} onclick={toggleEnabled}>
      {enabled ? "Pomodoro mode: On" : "Pomodoro mode: Off"}
    </button>
  </section>

  <section class="card">
    <h2>Most Important Tasks Today</h2>
    <textarea
      bind:value={taskListContent}
      oninput={scheduleTaskSave}
      placeholder="1.&#10;2.&#10;3."
      rows="8"
    ></textarea>
    <p class="hint">Shared with the break overlay &mdash; auto-saves as you type.</p>
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
