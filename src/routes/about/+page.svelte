<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getVersion } from "@tauri-apps/api/app";
  import { check } from "@tauri-apps/plugin-updater";
  import { relaunch } from "@tauri-apps/plugin-process";

  let version = $state("");
  let isAndroid = $state(false);

  type UpdateStatus = "idle" | "checking" | "up-to-date" | "available" | "error";
  let updateStatus = $state<UpdateStatus>("idle");
  let updateError = $state("");
  let latestVersion = $state("");
  let installing = $state(false);

  onMount(async () => {
    version = await getVersion();
    const os = await invoke<string>("current_os");
    isAndroid = os === "android";
  });

  async function checkForUpdates() {
    updateStatus = "checking";
    updateError = "";
    try {
      const update = await check();
      if (update) {
        latestVersion = update.version;
        updateStatus = "available";
      } else {
        updateStatus = "up-to-date";
      }
    } catch (e) {
      updateError = e instanceof Error ? e.message : String(e);
      updateStatus = "error";
    }
  }

  async function installUpdate() {
    installing = true;
    updateError = "";
    try {
      const update = await check();
      if (!update) {
        updateStatus = "up-to-date";
        return;
      }
      await update.downloadAndInstall();
      await relaunch();
    } catch (e) {
      updateError = e instanceof Error ? e.message : String(e);
      updateStatus = "error";
    } finally {
      installing = false;
    }
  }
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

  {#if isAndroid}
  <section class="card">
    <h2>Updates</h2>
    <p class="hint">Install updates from the same source you got this APK from (e.g. the GitHub Release : https://github.com/techxapp/reflectodoro/releases )</p>
  </section>
  {:else}
  <section class="card">
    <h2>Updates</h2>
    <div class="data-row">
      <button type="button" disabled={updateStatus === "checking" || installing} onclick={checkForUpdates}>
        {updateStatus === "checking" ? "Checking…" : "Check for updates"}
      </button>
      {#if updateStatus === "up-to-date"}
        <span class="hint saved">You're on the latest version.</span>
      {:else if updateStatus === "error"}
        <span class="hint error">{updateError}</span>
      {/if}
    </div>

    {#if updateStatus === "available"}
      <div class="data-row">
        <span class="hint">Version {latestVersion} is available.</span>
        <button type="button" disabled={installing} onclick={installUpdate}>
          {installing ? "Installing…" : "Install and restart"}
        </button>
      </div>
    {/if}
  </section>
  {/if}
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

  .data-row {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-top: 8px;
  }

  button {
    background: var(--accent);
    color: white;
    border: none;
    border-radius: 8px;
    padding: 9px 18px;
    font-size: 14px;
  }

  button:disabled {
    opacity: 0.6;
  }

  .saved {
    color: #3a9d5d;
  }

  .error {
    color: #d9534f;
  }
</style>
