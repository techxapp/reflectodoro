<script lang="ts">
  /**
   * Settings -> App lock (every platform; see app_lock.rs). Sets, changes or
   * removes the PIN. Asking for it is AppLockModal's job.
   */
  import { onMount, onDestroy } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import {
    getAppLockStatus,
    setAppLockPin,
    disableAppLock,
    engageAppLock,
    sanitizePinInput,
    APP_LOCK_STATE_EVENT,
    type AppLockStatus,
  } from "$lib/db";

  let { isAndroid = false }: { isAndroid?: boolean } = $props();

  let status = $state<AppLockStatus | null>(null);
  let panel = $state<null | "set" | "change" | "disable">(null);
  let currentPin = $state("");
  let newPin = $state("");
  let confirmPin = $state("");
  let busy = $state(false);
  let error = $state("");
  let notice = $state("");
  let unlisten: UnlistenFn | null = null;

  const minDigits = $derived(status?.minPinDigits ?? 4);
  const maxDigits = $derived(status?.maxPinDigits ?? 12);

  async function refresh() {
    try {
      status = await getAppLockStatus();
    } catch (e) {
      console.error("app lock status check failed", e);
    }
  }

  onMount(async () => {
    unlisten = await listen(APP_LOCK_STATE_EVENT, () => void refresh());
    await refresh();
  });

  onDestroy(() => unlisten?.());

  function open(next: typeof panel) {
    panel = panel === next ? null : next;
    currentPin = "";
    newPin = "";
    confirmPin = "";
    error = "";
    notice = "";
  }

  function digits(e: Event): string {
    const el = e.target as HTMLInputElement;
    const v = sanitizePinInput(el.value, maxDigits);
    el.value = v;
    return v;
  }

  function newPinProblem(): string {
    if (newPin.length < minDigits) return `Use at least ${minDigits} digits.`;
    if (newPin !== confirmPin) return "The PINs don't match.";
    return "";
  }

  async function run(action: () => Promise<unknown>, done: string) {
    busy = true;
    error = "";
    notice = "";
    try {
      await action();
      panel = null;
      currentPin = "";
      newPin = "";
      confirmPin = "";
      notice = done;
      await refresh();
    } catch (e) {
      error = String(e);
      currentPin = "";
    } finally {
      busy = false;
    }
  }

  function submitSet(e: Event) {
    e.preventDefault();
    const problem = newPinProblem();
    if (problem) {
      error = problem;
      return;
    }
    const isChange = panel === "change";
    void run(
      () => setAppLockPin(newPin, isChange ? currentPin : undefined),
      isChange ? "PIN changed." : "App lock is on.",
    );
  }

  function submitDisable(e: Event) {
    e.preventDefault();
    void run(() => disableAppLock(currentPin), "App lock is off.");
  }
</script>

<section class="card">
  <h2>App lock</h2>
  <p class="hint">
    Ask for a numeric PIN ({minDigits}&ndash;{maxDigits} digits) every time you come back to Reflectodoro from another
    app, and whenever it starts, so someone using your {isAndroid ? "phone" : "device"} can't read your entries. The
    break screen and check-in ask too.
  </p>
  <p class="hint">
    After 5 wrong PINs within 15 minutes, PIN entry is blocked until those 15 minutes are up.
    <strong>A forgotten PIN can't be recovered</strong> &mdash; the only way back in is to erase all entries on this
    device. The PIN stays on this device: it isn't exported or synced.
  </p>
  {#if isAndroid}
    <p class="hint">
      The break screen drawn over other apps (when "Display over other apps" is granted) doesn't ask for the PIN; it
      doesn't show your earlier entries.
    </p>
  {/if}

  {#if status}
    <p class="state">Status: <strong>{status.enabled ? "On" : "Off"}</strong></p>
    <div class="actions">
      {#if status.enabled}
        <button type="button" class="secondary" disabled={busy} onclick={() => open("change")}>Change PIN&hellip;</button>
        <button type="button" class="secondary" disabled={busy} onclick={() => open("disable")}>Turn off&hellip;</button>
        <button type="button" class="secondary" disabled={busy} onclick={() => void engageAppLock()}>Lock now</button>
      {:else}
        <button type="button" disabled={busy} onclick={() => open("set")}>Set a PIN&hellip;</button>
      {/if}
    </div>

    {#if panel === "set" || panel === "change"}
      <form onsubmit={submitSet}>
        {#if panel === "change"}
          {@render pinField("Current PIN", currentPin, (v) => (currentPin = v))}
        {/if}
        {@render pinField("New PIN", newPin, (v) => (newPin = v))}
        {@render pinField("Confirm new PIN", confirmPin, (v) => (confirmPin = v))}
        <button type="submit" disabled={busy || (panel === "change" && currentPin.length < minDigits)}>
          {panel === "change" ? "Change PIN" : "Turn on app lock"}
        </button>
      </form>
    {:else if panel === "disable"}
      <form onsubmit={submitDisable}>
        {@render pinField("Current PIN", currentPin, (v) => (currentPin = v))}
        <button type="submit" disabled={busy || currentPin.length < minDigits}>Turn off app lock</button>
      </form>
    {/if}
  {/if}

  {#if error}
    <p class="hint error" role="alert">{error}</p>
  {:else if notice}
    <p class="hint saved">{notice}</p>
  {/if}
</section>

{#snippet pinField(label: string, value: string, set: (v: string) => void)}
  <input
    type="password"
    inputmode="numeric"
    pattern="[0-9]*"
    autocomplete="off"
    maxlength={maxDigits}
    placeholder={label}
    aria-label={label}
    {value}
    oninput={(e) => {
      set(digits(e));
      error = "";
    }}
  />
{/snippet}

<style>
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

  .hint.error {
    color: var(--danger);
  }

  .hint.saved {
    color: #3a9d5d;
  }

  .state {
    font-size: 14px;
    margin: 14px 0 0;
  }

  .actions {
    display: flex;
    gap: 10px;
    flex-wrap: wrap;
    margin-top: 14px;
  }

  form {
    display: flex;
    flex-direction: column;
    gap: 10px;
    margin-top: 16px;
    max-width: 320px;
  }

  form button {
    align-self: flex-start;
  }

  input[type="password"] {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 8px;
    color: inherit;
    padding: 8px 10px;
    font-size: 16px;
    letter-spacing: 0.2em;
    width: 100%;
    box-sizing: border-box;
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
    cursor: default;
  }

  button.secondary {
    background: var(--surface-2);
    color: var(--text);
    border: 1px solid var(--border);
  }
</style>
