<script lang="ts">
  /**
   * Blocks the window while the desktop encryption key is locked, needs a
   * password, or is missing (key_store.rs). Mounted by the root layout in
   * *every* window, including the break overlay: a locked key means a
   * reflection can't be saved, and a saved reflection is what lets the break
   * screen close, so the prompt has to be reachable from there too. Kill
   * switches and the overlay's save-failure escape hatch are unaffected.
   * Never shown on Android (its Keystore key always reports "unlocked").
   */
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { page } from "$app/stores";
  import {
    getKeyStorageStatus,
    retryKeyResolution,
    unlockKeyFile,
    setKeyFilePassword,
    migrateKeyToVault,
    restoreEncryptionKey,
    resetEncryptionKey,
    KEY_STATE_EVENT,
    MIN_KEY_PASSWORD_CHARS,
    type KeyStorageStatus,
    type KeyLocation,
  } from "$lib/db";

  let status = $state<KeyStorageStatus | null>(null);
  /** A sub-flow reached from the main prompt. */
  let view = $state<"main" | "restore" | "reset">("main");
  let password = $state("");
  let confirmPassword = $state("");
  let backupKey = $state("");
  let target = $state<KeyLocation>("vault");
  let resetUnderstood = $state(false);
  let busy = $state(false);
  let error = $state("");
  let unlisten: UnlistenFn | null = null;

  /** Only the break-screen copy of the prompt offers Skip -- it closes the
   * break without a reflection (commands.rs's close_overlay_key_locked, which
   * refuses unless the key really is locked). On Android the overlay route
   * never shows this prompt at all (its Keystore key is always unlocked). */
  const onBreakScreen = $derived($page.url.pathname === "/overlay");

  const visible = $derived(status !== null && status.mode !== "keystore" && status.state !== "unlocked");

  async function refresh() {
    try {
      status = await getKeyStorageStatus();
    } catch (e) {
      console.error("key status check failed", e);
    }
  }

  onMount(async () => {
    unlisten = await listen(KEY_STATE_EVENT, () => {
      void refresh();
    });
    await refresh();
  });

  onDestroy(() => unlisten?.());

  function resetFields() {
    password = "";
    confirmPassword = "";
    backupKey = "";
    resetUnderstood = false;
    error = "";
  }

  function go(next: typeof view) {
    resetFields();
    target = status?.mode === "password_file" ? "file" : "vault";
    view = next;
  }

  /** Checks a new password pair; returns an error message or "". */
  function newPasswordProblem(): string {
    if (password.length < MIN_KEY_PASSWORD_CHARS) return `Use at least ${MIN_KEY_PASSWORD_CHARS} characters.`;
    if (password !== confirmPassword) return "The passwords don't match.";
    return "";
  }

  async function run(action: () => Promise<void>) {
    busy = true;
    error = "";
    try {
      await action();
      resetFields();
      view = "main";
      await refresh();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function skipBreak() {
    busy = true;
    error = "";
    try {
      await invoke("close_overlay_key_locked");
      resetFields();
      view = "main";
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  function submitUnlock(e: Event) {
    e.preventDefault();
    if (!password) return;
    void run(() => unlockKeyFile(password));
  }

  function submitSetPassword(e: Event) {
    e.preventDefault();
    const problem = newPasswordProblem();
    if (problem) {
      error = problem;
      return;
    }
    void run(() => setKeyFilePassword(password));
  }

  function submitRestore(e: Event) {
    e.preventDefault();
    if (!backupKey.trim()) {
      error = "Paste your backed-up key first.";
      return;
    }
    if (target === "file") {
      const problem = newPasswordProblem();
      if (problem) {
        error = problem;
        return;
      }
    }
    void run(() => restoreEncryptionKey(backupKey, target, target === "file" ? password : undefined));
  }

  function submitReset(e: Event) {
    e.preventDefault();
    if (target === "file") {
      const problem = newPasswordProblem();
      if (problem) {
        error = problem;
        return;
      }
    }
    if (!resetUnderstood) {
      error = "Tick the box to confirm you understand.";
      return;
    }
    if (!confirm("Start over with a new key? Entries saved so far will stay unreadable. This can't be undone.")) return;
    void run(() => resetEncryptionKey(target, target === "file" ? password : undefined));
  }
</script>

{#if visible && status}
  <div class="key-modal-backdrop" role="presentation">
    <div class="key-modal" role="dialog" aria-modal="true" aria-labelledby="key-modal-title">
      {#if view === "restore"}
        <h2 id="key-modal-title">Restore from a backed-up key</h2>
        <p class="hint">
          Paste the key you saved from Settings &rarr; Advanced &rarr; Show encryption key. It's checked
          against your existing entries before anything is changed.
        </p>
        <form onsubmit={submitRestore}>
          <textarea bind:value={backupKey} rows="2" spellcheck="false" autocomplete="off" placeholder="Backed-up key"
          ></textarea>
          {@render targetPicker()}
          {#if target === "file"}
            {@render newPasswordFields()}
          {/if}
          {@render errorLine()}
          <div class="row">
            <button type="submit" disabled={busy}>Restore key</button>
            <button type="button" class="secondary" disabled={busy} onclick={() => go("main")}>Back</button>
          </div>
        </form>
      {:else if view === "reset"}
        <h2 id="key-modal-title">Start over with a new key</h2>
        <p class="hint warn">
          Everything encrypted with your old key &mdash; reflections, task lists, check-ins, screen time &mdash;
          stays unreadable unless you later restore that key. You can clear unreadable days from the Entries tab.
        </p>
        <form onsubmit={submitReset}>
          {@render targetPicker()}
          {#if target === "file"}
            {@render newPasswordFields()}
          {/if}
          <label class="check">
            <input type="checkbox" bind:checked={resetUnderstood} />
            I understand my existing entries will stay unreadable
          </label>
          {@render errorLine()}
          <div class="row">
            <button type="submit" class="danger" disabled={busy}>Start over</button>
            <button type="button" class="secondary" disabled={busy} onclick={() => go("main")}>Back</button>
          </div>
        </form>
      {:else if status.state === "locked"}
        <h2 id="key-modal-title">Unlock your entries</h2>
        <p class="hint">
          Your encryption key is kept in a password-protected file. Enter its password to read and save entries.
          You'll be asked again the next time the app starts.
        </p>
        <form onsubmit={submitUnlock}>
          <!-- svelte-ignore a11y_autofocus -->
          <input type="password" bind:value={password} placeholder="Password" autocomplete="current-password" autofocus />
          {@render errorLine()}
          <div class="row">
            <button type="submit" disabled={busy || !password}>{busy ? "Unlocking…" : "Unlock"}</button>
          </div>
        </form>
        <p class="links">
          <button type="button" class="link" onclick={() => go("restore")}>Use backup key</button>
          <span aria-hidden="true">&middot;</span>
          <button type="button" class="link" onclick={() => go("reset")}>Forgot password?</button>
        </p>
      {:else if status.state === "needs_password"}
        <h2 id="key-modal-title">Protect your encryption key</h2>
        <p class="hint">
          {#if status.legacy}
            Your encryption key is stored in a file that isn't protected yet. Anyone who copies the app's folder
            could read your entries. Choose a password to protect it.
          {:else}
            The system password vault didn't keep your encryption key, so it will be stored in a file protected
            by a password you choose.
          {/if}
          You'll enter it each time the app starts. <strong>If you forget it, there's no way to recover it</strong>
          &mdash; back the key up from Settings &rarr; Advanced once you're in.
        </p>
        <form onsubmit={submitSetPassword}>
          {@render newPasswordFields()}
          {@render errorLine()}
          <div class="row">
            <button type="submit" disabled={busy}>{busy ? "Saving…" : "Set password"}</button>
          </div>
        </form>
        <p class="links">
          <button type="button" class="link" disabled={busy} onclick={() => void run(migrateKeyToVault)}>
            Store it in the system password vault instead
          </button>
        </p>
      {:else}
        <h2 id="key-modal-title">Encryption key not found</h2>
        <p class="hint">
          {#if status.mode === "keychain"}
            Your entries are encrypted, but their key couldn't be read from the iOS Keychain &mdash; most often right
            after the phone restarts, before it has been unlocked once. No new key has been created, so nothing has
            been lost.
          {:else}
            Your entries are encrypted, but their key couldn't be read &mdash; most often because access to the system
            password vault was denied or the vault isn't available yet. No new key has been created, so nothing has
            been lost.
          {/if}
        </p>
        {@render errorLine()}
        <div class="row">
          <button type="button" disabled={busy} onclick={() => void run(retryKeyResolution)}>Try again</button>
          <button type="button" class="secondary" disabled={busy} onclick={() => go("restore")}>Use backup key</button>
        </div>
        <p class="links">
          <button type="button" class="link" onclick={() => go("reset")}>Start over with a new key</button>
        </p>
      {/if}
      {#if onBreakScreen && view === "main"}
        <div class="skip">
          <button type="button" class="secondary" disabled={busy} onclick={skipBreak}>Skip this break</button>
          <p class="hint">Closes the break screen now. No reflection is saved for this break.</p>
        </div>
      {/if}
    </div>
  </div>
{/if}

{#snippet newPasswordFields()}
  <input type="password" bind:value={password} placeholder="New password" autocomplete="new-password" />
  <input type="password" bind:value={confirmPassword} placeholder="Confirm password" autocomplete="new-password" />
{/snippet}

{#snippet targetPicker()}
  <!-- iOS has only the Keychain (sent as the "vault" target). -->
  {#if status?.mode !== "keychain"}
  <fieldset>
    <legend>Keep the key in</legend>
    <label class="check"><input type="radio" bind:group={target} value="vault" /> System password vault</label>
    <label class="check"><input type="radio" bind:group={target} value="file" /> Password-protected file</label>
  </fieldset>
  {/if}
{/snippet}

{#snippet errorLine()}
  {#if error}
    <p class="hint error" role="alert">{error}</p>
  {/if}
{/snippet}

<style>
  .key-modal-backdrop {
    position: fixed;
    inset: 0;
    z-index: 10000;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 16px;
    background: rgba(0, 0, 0, 0.55);
  }

  .key-modal {
    background: var(--surface);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 14px;
    padding: 24px;
    width: 100%;
    max-width: 440px;
    max-height: 100%;
    overflow-y: auto;
    box-sizing: border-box;
  }

  h2 {
    margin: 0 0 10px;
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

  input[type="password"],
  textarea {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 8px;
    color: inherit;
    padding: 9px 10px;
    font-size: 14px;
    width: 100%;
    box-sizing: border-box;
  }

  textarea {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    resize: vertical;
  }

  fieldset {
    border: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  legend {
    font-size: 12px;
    color: var(--text-dim);
    margin-bottom: 4px;
  }

  .check {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
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
    display: flex;
    gap: 8px;
    align-items: center;
    flex-wrap: wrap;
    font-size: 13px;
    color: var(--text-dim);
  }

  .skip {
    margin-top: 18px;
    padding-top: 14px;
    border-top: 1px solid var(--border);
  }

  .skip .hint {
    margin: 8px 0 0;
  }

  button.link {
    background: none;
    border: none;
    padding: 0;
    color: var(--accent);
    font-size: 13px;
  }
</style>
