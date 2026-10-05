<script lang="ts">
  /**
   * The app lock's PIN prompt (app_lock.rs). Mounted by the root layout in
   * every window, break overlay and check-in included, and covers the whole
   * window with an opaque backdrop while the app is locked, so nothing behind
   * it can be read. Rust decides when to lock (leaving for another app) and
   * checks the PIN; this only asks for it.
   *
   * On Android/iOS this is also where the lock is engaged: the webview going
   * hidden is the only "left the app" signal there.
   */
  import { onMount, onDestroy, tick } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { ask } from "@tauri-apps/plugin-dialog";
  import RecoveryCodeDisplay from "$lib/RecoveryCodeDisplay.svelte";
  import {
    getAppLockStatus,
    unlockAppLock,
    engageAppLock,
    forgotAppLockPinErase,
    recoverAppLock,
    sanitizePinInput,
    sanitizeRecoveryCodeInput,
    isCompleteRecoveryCode,
    APP_LOCK_STATE_EVENT,
    APP_LOCK_ERASED_EVENT,
    type AppLockStatus,
  } from "$lib/db";

  let status = $state<AppLockStatus | null>(null);
  let view = $state<"pin" | "forgot" | "recover">("pin");
  let pin = $state("");
  let recoveryCode = $state("");
  let newPin = $state("");
  let confirmPin = $state("");
  /** The replacement code issued by a successful recovery. Rust has already
   * unlocked by then, so this is what keeps the prompt up until the user has
   * saved it. */
  let pendingRecoveryCode = $state<string | null>(null);
  let busy = $state(false);
  let error = $state("");
  let eraseUnderstood = $state(false);
  let now = $state(Date.now());
  let inputEl: HTMLInputElement | undefined = $state();
  let modalEl: HTMLDivElement | undefined = $state();

  let unlistenState: UnlistenFn | null = null;
  let unlistenErased: UnlistenFn | null = null;
  let clock: ReturnType<typeof setInterval> | null = null;
  let isMobile = false;

  const visible = $derived(
    (status !== null && status.enabled && status.locked) || pendingRecoveryCode !== null,
  );
  const lockedOutUntil = $derived(
    status?.lockedOutUntilMs != null && status.lockedOutUntilMs > now ? status.lockedOutUntilMs : null,
  );
  const minDigits = $derived(status?.minPinDigits ?? 4);
  const maxDigits = $derived(status?.maxPinDigits ?? 12);

  function lockoutText(until: number): string {
    const secs = Math.max(0, Math.ceil((until - now) / 1000));
    const m = Math.floor(secs / 60);
    const s = secs % 60;
    return `${m}:${String(s).padStart(2, "0")}`;
  }

  async function refresh() {
    try {
      const wasVisible = visible;
      status = await getAppLockStatus();
      if (visible && !wasVisible) {
        view = "pin";
        pin = "";
        recoveryCode = "";
        newPin = "";
        confirmPin = "";
        error = "";
        eraseUnderstood = false;
        await focusInput();
      }
    } catch (e) {
      console.error("app lock status check failed", e);
    }
  }

  async function focusInput() {
    await tick();
    inputEl?.focus();
  }

  function onVisibilityChange() {
    if (isMobile && document.visibilityState === "hidden") {
      void engageAppLock().catch((e) => console.error("app lock engage failed", e));
    }
  }

  /** Keeps keyboard focus inside the prompt, so typing can't land in a
   * field hidden behind it (e.g. the break screen's reflection box). */
  function onFocusIn(e: FocusEvent) {
    if (!visible || !modalEl) return;
    if (e.target instanceof Node && modalEl.contains(e.target)) return;
    if (view === "pin") inputEl?.focus();
    else modalEl.querySelector<HTMLElement>("button, input")?.focus();
  }

  function onWindowFocus() {
    if (visible && view === "pin") inputEl?.focus();
  }

  onMount(async () => {
    unlistenState = await listen(APP_LOCK_STATE_EVENT, () => void refresh());
    unlistenErased = await listen(APP_LOCK_ERASED_EVENT, () => location.reload());
    document.addEventListener("focusin", onFocusIn);
    window.addEventListener("focus", onWindowFocus);
    clock = setInterval(() => {
      now = Date.now();
      // The lockout just ran out: pick up the fresh attempt count.
      if (status?.lockedOutUntilMs != null && status.lockedOutUntilMs <= now) {
        error = "";
        void refresh().then(focusInput);
      }
    }, 1000);
    try {
      const os = await invoke<string>("current_os");
      isMobile = os === "android" || os === "ios";
    } catch (e) {
      console.error("current_os failed", e);
    }
    document.addEventListener("visibilitychange", onVisibilityChange);
    await refresh();
  });

  onDestroy(() => {
    unlistenState?.();
    unlistenErased?.();
    document.removeEventListener("focusin", onFocusIn);
    document.removeEventListener("visibilitychange", onVisibilityChange);
    window.removeEventListener("focus", onWindowFocus);
    if (clock) clearInterval(clock);
  });

  function onPinInput(e: Event) {
    const el = e.target as HTMLInputElement;
    pin = sanitizePinInput(el.value, maxDigits);
    el.value = pin;
    error = "";
  }

  async function submit(e: Event) {
    e.preventDefault();
    if (busy || lockedOutUntil) return;
    if (pin.length < minDigits) {
      error = `Enter at least ${minDigits} digits.`;
      return;
    }
    busy = true;
    error = "";
    try {
      const next = await unlockAppLock(pin);
      status = next;
      pin = "";
      if (next.locked && next.lockedOutUntilMs == null) {
        error = `Wrong PIN. ${next.attemptsLeft} attempt${next.attemptsLeft === 1 ? "" : "s"} left.`;
      }
    } catch (err) {
      error = String(err);
    } finally {
      busy = false;
    }
    // After busy clears: a disabled input can't take focus.
    if (visible) await focusInput();
  }

  async function confirmErase(): Promise<boolean> {
    const message =
      "Erase every reflection, task list, check-in, habit and screen-time entry on this device, forget all paired devices, and remove the PIN? This can't be undone.";
    try {
      return await ask(message, { title: "Forgot PIN", kind: "warning" });
    } catch {
      return confirm(message);
    }
  }

  async function erase() {
    if (!eraseUnderstood) {
      error = "Tick the box to confirm you understand.";
      return;
    }
    if (!(await confirmErase())) return;
    busy = true;
    error = "";
    try {
      await forgotAppLockPinErase();
      // Rust also broadcasts applock://erased, which reloads every window.
    } catch (err) {
      error = String(err);
    } finally {
      busy = false;
    }
  }

  function onRecoveryCodeInput(e: Event) {
    const el = e.target as HTMLInputElement;
    recoveryCode = sanitizeRecoveryCodeInput(el.value);
    el.value = recoveryCode;
    error = "";
  }

  function onNewPinInput(e: Event) {
    const el = e.target as HTMLInputElement;
    newPin = sanitizePinInput(el.value, maxDigits);
    el.value = newPin;
    error = "";
  }

  function onConfirmPinInput(e: Event) {
    const el = e.target as HTMLInputElement;
    confirmPin = sanitizePinInput(el.value, maxDigits);
    el.value = confirmPin;
    error = "";
  }

  async function submitRecover(e: Event) {
    e.preventDefault();
    if (busy) return;
    if (!isCompleteRecoveryCode(recoveryCode)) {
      error = "Enter the whole recovery code.";
      return;
    }
    if (newPin.length < minDigits) {
      error = `Use at least ${minDigits} digits for the new PIN.`;
      return;
    }
    if (newPin !== confirmPin) {
      error = "The PINs don't match.";
      return;
    }
    busy = true;
    error = "";
    try {
      const result = await recoverAppLock(recoveryCode, newPin);
      // Set before status: status flips `locked` off, and `visible` must
      // already be held open by the pending code when that happens.
      pendingRecoveryCode = result.recoveryCode;
      status = result.status;
      recoveryCode = "";
      newPin = "";
      confirmPin = "";
    } catch (err) {
      error = String(err);
    } finally {
      busy = false;
    }
  }

  function finishRecovery() {
    pendingRecoveryCode = null;
    view = "pin";
    error = "";
  }

  function goRecover() {
    view = "recover";
    error = "";
    recoveryCode = "";
    newPin = "";
    confirmPin = "";
  }

  function goForgot() {
    view = "forgot";
    error = "";
    eraseUnderstood = false;
  }

  function goPin() {
    view = "pin";
    error = "";
    void focusInput();
  }
</script>

{#if visible && status}
  <div class="lock-backdrop" role="presentation">
    <div class="lock-modal" role="dialog" aria-modal="true" aria-labelledby="lock-title" bind:this={modalEl}>
      {#if pendingRecoveryCode}
        <h2 id="lock-title">PIN reset</h2>
        <p class="hint">Your new PIN is set and Reflectodoro is unlocked. Here is your new recovery code.</p>
        <RecoveryCodeDisplay code={pendingRecoveryCode} onDone={finishRecovery} />
      {:else if view === "recover"}
        <h2 id="lock-title">Use your recovery code</h2>
        <p class="hint">Enter the recovery code you saved, then choose a new PIN. Nothing is erased.</p>
        <form onsubmit={submitRecover}>
          <input
            class="code"
            type="text"
            autocomplete="off"
            autocapitalize="characters"
            spellcheck="false"
            placeholder="XXXX-XXXX-XXXX-XXXX"
            aria-label="Recovery code"
            value={recoveryCode}
            oninput={onRecoveryCodeInput}
            disabled={busy}
          />
          <input
            class="newpin"
            type="password"
            inputmode="numeric"
            pattern="[0-9]*"
            autocomplete="off"
            maxlength={maxDigits}
            placeholder="New PIN"
            aria-label="New PIN"
            value={newPin}
            oninput={onNewPinInput}
            disabled={busy}
          />
          <input
            class="newpin"
            type="password"
            inputmode="numeric"
            pattern="[0-9]*"
            autocomplete="off"
            maxlength={maxDigits}
            placeholder="Confirm new PIN"
            aria-label="Confirm new PIN"
            value={confirmPin}
            oninput={onConfirmPinInput}
            disabled={busy}
          />
          {#if error}
            <p class="hint error" role="alert">{error}</p>
          {/if}
          <div class="row">
            <button type="submit" disabled={busy || !isCompleteRecoveryCode(recoveryCode) || newPin.length < minDigits}>
              {busy ? "Checking…" : "Reset PIN"}
            </button>
            <button type="button" class="secondary" disabled={busy} onclick={goForgot}>Back</button>
          </div>
        </form>
      {:else if view === "forgot"}
        <h2 id="lock-title">Forgot your PIN?</h2>
        {#if status.hasRecoveryCode}
          <p class="hint">
            If you saved the recovery code shown when you set your PIN, use it to choose a new PIN. Nothing is erased.
          </p>
          <div class="row recover-row">
            <button type="button" onclick={goRecover}>Use my recovery code</button>
          </div>
          <p class="hint warn">
            No recovery code? The only other way back in is to erase everything this app has stored on this device
            &mdash; reflections, task lists, check-ins, habits and screen time &mdash; and forget your paired devices, so
            they can't send it all back. Your settings are kept.
          </p>
        {:else}
          <p class="hint warn">
            There's no recovery code on this device, so the PIN can't be recovered. The only way back in is to erase
            everything this app has stored on this device &mdash; reflections, task lists, check-ins, habits and screen
            time &mdash; and forget your paired devices, so they can't send it all back. Your settings are kept.
          </p>
        {/if}
        <label class="check">
          <input type="checkbox" bind:checked={eraseUnderstood} />
          I understand all my entries on this device will be erased
        </label>
        {#if error}
          <p class="hint error" role="alert">{error}</p>
        {/if}
        <div class="row">
          <button type="button" class="danger" disabled={busy} onclick={erase}>Erase and remove PIN</button>
          <button type="button" class="secondary" disabled={busy} onclick={goPin}>Back</button>
        </div>
      {:else}
        <h2 id="lock-title">Reflectodoro is locked</h2>
        <p class="hint">Enter your PIN to continue.</p>
        <form onsubmit={submit}>
          <input
            bind:this={inputEl}
            class="pin"
            type="password"
            inputmode="numeric"
            pattern="[0-9]*"
            autocomplete="off"
            maxlength={maxDigits}
            placeholder="PIN"
            aria-label="PIN"
            value={pin}
            oninput={onPinInput}
            disabled={busy || lockedOutUntil !== null}
          />
          {#if lockedOutUntil}
            <p class="hint error" role="alert">
              Too many wrong PINs. Try again in {lockoutText(lockedOutUntil)}.
            </p>
          {:else if error}
            <p class="hint error" role="alert">{error}</p>
          {/if}
          <button type="submit" disabled={busy || lockedOutUntil !== null || pin.length < minDigits}>
            {busy ? "Checking…" : "Unlock"}
          </button>
        </form>
        <p class="links">
          <button type="button" class="link" onclick={goForgot}>Forgot PIN?</button>
        </p>
      {/if}
    </div>
  </div>
{/if}

<style>
  .lock-backdrop {
    position: fixed;
    inset: 0;
    /* Above KeyUnlockModal (10000): this one hides content, that one only
       asks for a password. */
    z-index: 10001;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 16px;
    /* Opaque on purpose -- the whole point is that nothing behind shows. */
    background: var(--bg);
  }

  .lock-modal {
    background: var(--surface);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 14px;
    padding: 24px;
    width: 100%;
    max-width: 360px;
    max-height: 100%;
    overflow-y: auto;
    box-sizing: border-box;
  }

  h2 {
    margin: 0 0 8px;
    font-size: 17px;
  }

  .hint {
    color: var(--text-dim);
    font-size: 13px;
    line-height: 1.5;
    margin: 0 0 14px;
  }

  .hint.warn {
    color: var(--danger);
  }

  .hint.error {
    color: var(--danger);
    margin: 0;
  }

  form {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  input.code,
  input.newpin {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 8px;
    color: inherit;
    padding: 10px 12px;
    width: 100%;
    box-sizing: border-box;
  }

  input.code {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 16px;
    letter-spacing: 0.08em;
    text-align: center;
  }

  input.newpin {
    font-size: 18px;
    letter-spacing: 0.3em;
    text-align: center;
  }

  .recover-row {
    margin-bottom: 14px;
  }

  input.pin {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 8px;
    color: inherit;
    padding: 10px 12px;
    font-size: 22px;
    letter-spacing: 0.3em;
    text-align: center;
    width: 100%;
    box-sizing: border-box;
  }

  .check {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
    margin-bottom: 12px;
  }

  .row {
    display: flex;
    gap: 10px;
    flex-wrap: wrap;
    margin-top: 4px;
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

  button.danger {
    background: var(--danger);
  }

  .links {
    margin: 14px 0 0;
    font-size: 13px;
  }

  button.link {
    background: none;
    border: none;
    padding: 0;
    color: var(--accent);
    font-size: 13px;
  }
</style>
