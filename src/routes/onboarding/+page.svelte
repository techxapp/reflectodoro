<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { goto } from "$app/navigation";
  import { invoke } from "@tauri-apps/api/core";
  import { isPermissionGranted, requestPermission } from "@tauri-apps/plugin-notification";
  import { markOnboardingCompleted } from "$lib/db";

  let overlayGranted = $state(false);
  let overlayChecked = $state(false);
  let notificationGranted = $state(false);
  let notificationChecked = $state(false);
  let finishing = $state(false);

  async function refreshStatus() {
    overlayGranted = await invoke<boolean>("can_draw_overlays");
    overlayChecked = true;
    notificationGranted = await isPermissionGranted();
    notificationChecked = true;
  }

  async function grantOverlay() {
    // No in-app dialog exists for this one -- opens the system settings
    // screen instead. Re-checked via the visibilitychange listener below
    // once the user comes back.
    await invoke("request_draw_overlays_permission");
  }

  async function grantNotifications() {
    await requestPermission();
    await refreshStatus();
  }

  // Catches the return from the overlay settings screen or the notification
  // request dialog, both of which take the user out of (and back into) the
  // app, and neither reliably resolves its own promise with the final state
  // on every Android version.
  function onVisibilityChange() {
    if (document.visibilityState === "visible") void refreshStatus();
  }

  onMount(() => {
    void refreshStatus();
    document.addEventListener("visibilitychange", onVisibilityChange);
  });

  onDestroy(() => {
    document.removeEventListener("visibilitychange", onVisibilityChange);
  });

  async function finish() {
    finishing = true;
    await markOnboardingCompleted();
    await goto("/");
  }
</script>

<div class="onboarding">
  <div class="content">
    <h1>Set up Reflectodoro</h1>
    <p class="intro">
      A few optional permissions make break enforcement more reliable. All are skippable &mdash;
      Reflectodoro keeps working without them, just less effectively in the background.
    </p>

    <section class="card">
      <div class="card-head">
        <h2>Break screen</h2>
        {#if overlayChecked}
          <span class="status" class:ok={overlayGranted}>
            {overlayGranted ? "Granted" : "Not granted"}
          </span>
        {/if}
      </div>
      <p class="hint">
        Recommended. Lets the break screen appear directly over whatever else you're doing when a
        break starts, instead of just a notification you could miss or dismiss. It can't wake or
        take over a locked screen &mdash; only interrupts active use.
      </p>
      {#if !overlayGranted}
        <button type="button" onclick={grantOverlay}>Open settings&hellip;</button>
      {/if}
    </section>

    <section class="card">
      <div class="card-head">
        <h2>Notifications</h2>
        {#if notificationChecked}
          <span class="status" class:ok={notificationGranted}>
            {notificationGranted ? "Granted" : "Not granted"}
          </span>
        {/if}
      </div>
      <p class="hint">
        Lets the background timer show a running indicator, and lets a break reminder reach you
        if the app isn't open.
      </p>
      {#if !notificationGranted}
        <button type="button" onclick={grantNotifications}>Enable notifications</button>
      {/if}
    </section>

    <button type="button" class="continue" disabled={finishing} onclick={finish}>
      {finishing ? "Continuing…" : "Continue"}
    </button>
  </div>
</div>

<style>
  .onboarding {
    min-height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    box-sizing: border-box;
    overflow-y: auto;
    background: var(--bg);
    color: var(--text);
    padding: calc(32px + var(--safe-top)) calc(20px + var(--safe-right))
      calc(32px + var(--safe-bottom)) calc(20px + var(--safe-left));
  }

  .content {
    width: min(480px, 100%);
    margin: auto 0;
  }

  h1 {
    font-size: 22px;
    margin: 0 0 8px;
  }

  .intro {
    color: var(--text-dim);
    font-size: 14px;
    line-height: 1.5;
    margin: 0 0 24px;
  }

  .card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 14px;
    padding: 20px;
    margin-bottom: 16px;
  }

  .card-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 8px;
  }

  h2 {
    font-size: 16px;
    margin: 0;
  }

  .status {
    font-size: 12px;
    font-weight: 600;
    color: var(--text-dim);
    background: var(--surface-2);
    padding: 3px 10px;
    border-radius: 999px;
    white-space: nowrap;
  }

  .status.ok {
    color: var(--accent);
    background: var(--accent-soft);
  }

  .hint {
    color: var(--text-dim);
    font-size: 13px;
    line-height: 1.5;
    margin: 0 0 14px;
  }

  button {
    background: var(--accent-soft);
    color: var(--accent);
    border: none;
    border-radius: 8px;
    padding: 10px 16px;
    font-size: 14px;
    font-weight: 500;
  }

  .continue {
    width: 100%;
    background: var(--accent);
    color: white;
    padding: 12px 16px;
    font-size: 15px;
    font-weight: 600;
    margin-top: 8px;
  }

  .continue:disabled {
    opacity: 0.6;
  }
</style>
