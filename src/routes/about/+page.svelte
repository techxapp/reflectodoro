<script lang="ts">
  import { onMount } from "svelte";
  import { getVersion } from "@tauri-apps/api/app";

  let version = $state("");

  onMount(async () => {
    version = await getVersion();
  });
</script>

<div class="page">
  <section class="card">
    <h2>Reflectodoro</h2>
    <p class="version">{version ? `Version ${version}` : "Loading version…"}</p>
    <p class="hint">
      A Pomodoro app that forces a short self-reflection at the end of every break.
    </p>
    <p class="hint">Licensed under PolyForm Noncommercial 1.0.0.</p>
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

  .version {
    font-size: 13px;
    color: var(--text-dim);
    margin: 0 0 16px;
  }

  .hint {
    color: var(--text-dim);
    font-size: 13px;
    line-height: 1.5;
    margin: 0 0 8px;
  }
</style>
