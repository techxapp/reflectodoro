<script lang="ts">
  import { onMount } from "svelte";
  import { getBreakitSettings, saveBreakitSettings, type BreakitSettings } from "$lib/db";

  let length = $state(15);
  let includeSpecial = $state(false);
  let saved = $state(false);
  let loaded = $state(false);

  onMount(async () => {
    const settings: BreakitSettings = await getBreakitSettings();
    length = settings.length;
    includeSpecial = settings.includeSpecial;
    loaded = true;
  });

  async function save(e: Event) {
    e.preventDefault();
    await saveBreakitSettings({ length: Math.min(64, Math.max(4, length)), includeSpecial });
    saved = true;
    setTimeout(() => (saved = false), 2000);
  }
</script>

<div class="page">
  <section class="card">
    <h2>Break overlay</h2>
    <p class="hint">
      Reflection ("what did I do?") is always required to end a break. Typing a random code exactly
      (no pasting) is the early-exit alternative &mdash; it still requires the reflection too. A new
      code is generated for every break, so it can't become muscle memory.
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
  </section>

  <section class="card">
    <h2>Session schedule</h2>
    <p class="hint">
      Fixed for now: work runs :00&ndash;:25 and :30&ndash;:55 each hour; breaks run :25&ndash;:30 and
      :55&ndash;:00. Not configurable in this build.
    </p>
  </section>

  <section class="card">
    <h2>If the overlay ever gets stuck</h2>
    <ul class="hint">
      <li>Press Ctrl+Alt+Shift+F12 to force-close the overlay.</li>
    </ul>
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

  input {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 8px;
    color: inherit;
    padding: 8px 10px;
    font-size: 14px;
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
</style>
