<script lang="ts">
  /**
   * Settings -> Advanced -> Encryption key (desktop only; see key_store.rs).
   * Moves the one at-rest key between the OS vault and a password-protected
   * file, changes that password, reveals the key for backup, and restores a
   * backed-up key. Locked/missing states are handled by KeyUnlockModal, which
   * covers the whole window; this card only acts on an unlocked key.
   */
  import { onMount, onDestroy } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import {
    getKeyStorageStatus,
    migrateKeyToFile,
    migrateKeyToVault,
    changeKeyFilePassword,
    revealEncryptionKey,
    restoreEncryptionKey,
    KEY_STATE_EVENT,
    MIN_KEY_PASSWORD_CHARS,
    type KeyStorageStatus,
    type KeyLocation,
  } from "$lib/db";

  /** How long a revealed key stays on screen. */
  const REVEAL_SECONDS = 60;

  let status = $state<KeyStorageStatus | null>(null);
  let panel = $state<null | "toFile" | "changePassword" | "reveal" | "restore">(null);
  let currentPassword = $state("");
  let password = $state("");
  let confirmPassword = $state("");
  let backupKey = $state("");
  let target = $state<KeyLocation>("vault");
  let busy = $state(false);
  let error = $state("");
  let notice = $state("");

  let revealedKey = $state("");
  let copied = $state(false);
  let hideTimer: ReturnType<typeof setTimeout> | null = null;
  let unlisten: UnlistenFn | null = null;

  const unlocked = $derived(status?.state === "unlocked");
  const inFile = $derived(status?.mode === "password_file");

  async function refresh() {
    try {
      status = await getKeyStorageStatus();
    } catch (e) {
      error = String(e);
    }
  }

  onMount(async () => {
    unlisten = await listen(KEY_STATE_EVENT, () => {
      void refresh();
    });
    await refresh();
  });

  onDestroy(() => {
    unlisten?.();
    hideKey();
  });

  function clearFields() {
    currentPassword = "";
    password = "";
    confirmPassword = "";
    backupKey = "";
    error = "";
  }

  function open(next: typeof panel) {
    clearFields();
    notice = "";
    target = inFile ? "file" : "vault";
    panel = panel === next ? null : next;
  }

  function newPasswordProblem(): string {
    if (password.length < MIN_KEY_PASSWORD_CHARS) return `Use at least ${MIN_KEY_PASSWORD_CHARS} characters.`;
    if (password !== confirmPassword) return "The passwords don't match.";
    return "";
  }

  async function run(action: () => Promise<void>, done: string) {
    busy = true;
    error = "";
    try {
      await action();
      clearFields();
      panel = null;
      notice = done;
      await refresh();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  function submitToFile(e: Event) {
    e.preventDefault();
    const problem = newPasswordProblem();
    if (problem) {
      error = problem;
      return;
    }
    void run(() => migrateKeyToFile(password), "Key moved to a password-protected file.");
  }

  function moveToVault() {
    if (!confirm("Move the key into the system password vault? You won't be asked for a password at startup anymore."))
      return;
    notice = "";
    void run(migrateKeyToVault, "Key moved to the system password vault.");
  }

  function submitChangePassword(e: Event) {
    e.preventDefault();
    const problem = newPasswordProblem();
    if (problem) {
      error = problem;
      return;
    }
    void run(() => changeKeyFilePassword(currentPassword, password), "Password changed.");
  }

  async function reveal(e?: Event) {
    e?.preventDefault();
    if (!inFile && !confirm("Show your encryption key on screen? Make sure nobody else can see it.")) return;
    busy = true;
    error = "";
    try {
      revealedKey = await revealEncryptionKey(inFile ? currentPassword : undefined);
      copied = false;
      currentPassword = "";
      panel = null;
      hideTimer = setTimeout(hideKey, REVEAL_SECONDS * 1000);
    } catch (err) {
      error = String(err);
    } finally {
      busy = false;
    }
  }

  function hideKey() {
    if (hideTimer) clearTimeout(hideTimer);
    hideTimer = null;
    revealedKey = "";
    copied = false;
  }

  async function copyKey() {
    try {
      await navigator.clipboard.writeText(revealedKey);
      copied = true;
    } catch (e) {
      error = `Couldn't copy: ${e}`;
    }
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
    void run(
      () => restoreEncryptionKey(backupKey, target, target === "file" ? password : undefined),
      "Key restored.",
    );
  }
</script>

<!-- Android's Keystore key has nothing to manage; Settings also hides this
     card there, but only once current_os has answered. -->
{#if status?.mode !== "keystore"}
<section class="card">
  <h2>Encryption key</h2>
  <p class="hint">
    Your reflections, task lists, check-ins and screen time are encrypted on this device with a random key the app
    generates for you. By default that key is kept in your system password vault (Keychain, Credential Manager or
    Secret Service), so you never have to type anything.
  </p>
  <p class="hint">
    You can instead keep it in a <strong>password-protected file</strong> in the app's folder. You'll enter the
    password each time the app starts; it's held only in memory until the app quits, and
    <strong>a forgotten password can't be recovered</strong> &mdash; only a backed-up key can. The key never leaves
    this device: paired devices each use their own.
  </p>

  {#if status}
    <p class="location">
      Stored in:
      <strong>
        {#if status.mode === "vault"}System password vault
        {:else if status.mode === "password_file"}Password-protected file
        {:else}Not set up{/if}
      </strong>
      {#if !unlocked}<span class="hint"> &mdash; locked; unlock it from the prompt first.</span>{/if}
    </p>

    {#if unlocked}
      <div class="actions">
        {#if inFile}
          <button type="button" disabled={busy} onclick={moveToVault}>Move to system vault</button>
          <button type="button" class="secondary" disabled={busy} onclick={() => open("changePassword")}>
            Change password&hellip;
          </button>
        {:else}
          <button type="button" disabled={busy} onclick={() => open("toFile")}>
            Move to password-protected file&hellip;
          </button>
        {/if}
        <button
          type="button"
          class="secondary"
          disabled={busy}
          onclick={() => (inFile ? open("reveal") : void reveal())}
        >
          Show encryption key
        </button>
        <button type="button" class="secondary" disabled={busy} onclick={() => open("restore")}>
          Restore key from backup&hellip;
        </button>
      </div>

      {#if panel === "toFile"}
        <form onsubmit={submitToFile}>
          <p class="hint">Choose a password. You'll need it every time the app starts.</p>
          {@render newPasswordFields()}
          <button type="submit" disabled={busy}>Move key</button>
        </form>
      {:else if panel === "changePassword"}
        <form onsubmit={submitChangePassword}>
          <input type="password" bind:value={currentPassword} placeholder="Current password" autocomplete="current-password" />
          {@render newPasswordFields()}
          <button type="submit" disabled={busy || !currentPassword}>Change password</button>
        </form>
      {:else if panel === "reveal"}
        <form onsubmit={reveal}>
          <input type="password" bind:value={currentPassword} placeholder="Key file password" autocomplete="current-password" />
          <button type="submit" disabled={busy || !currentPassword}>Show key</button>
        </form>
      {:else if panel === "restore"}
        <form onsubmit={submitRestore}>
          <p class="hint">
            Replaces the key this device uses with one you backed up earlier. It's only accepted if it can read your
            existing entries.
          </p>
          <textarea bind:value={backupKey} rows="2" spellcheck="false" autocomplete="off" placeholder="Backed-up key"
          ></textarea>
          <fieldset>
            <legend>Keep the key in</legend>
            <label class="checkbox"><input type="radio" bind:group={target} value="vault" /> System password vault</label>
            <label class="checkbox"><input type="radio" bind:group={target} value="file" /> Password-protected file</label>
          </fieldset>
          {#if target === "file"}
            {@render newPasswordFields()}
          {/if}
          <button type="submit" disabled={busy}>Restore key</button>
        </form>
      {/if}

      {#if revealedKey}
        <div class="revealed">
          <code>{revealedKey}</code>
          <p class="hint warn">
            Anyone with this key and a copy of your data can read all your entries. If you copy it, keep it only in a
            secure password manager &mdash; never in a note, email or chat. Hidden again in {REVEAL_SECONDS} seconds.
          </p>
          <div class="actions">
            <button type="button" onclick={copyKey}>{copied ? "Copied" : "Copy"}</button>
            <button type="button" class="secondary" onclick={hideKey}>Hide</button>
          </div>
        </div>
      {/if}
    {/if}
  {/if}

  {#if error}
    <p class="hint error" role="alert">{error}</p>
  {:else if notice}
    <p class="hint saved">{notice}</p>
  {/if}
</section>
{/if}

{#snippet newPasswordFields()}
  <input type="password" bind:value={password} placeholder="New password" autocomplete="new-password" />
  <input type="password" bind:value={confirmPassword} placeholder="Confirm password" autocomplete="new-password" />
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

  .hint.warn,
  .hint.error {
    color: var(--danger);
  }

  .hint.saved {
    color: #3a9d5d;
  }

  .location {
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
    max-width: 420px;
  }

  form .hint {
    margin: 0;
  }

  form button {
    align-self: flex-start;
  }

  input[type="password"],
  textarea {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 8px;
    color: inherit;
    padding: 8px 10px;
    font-size: 14px;
    width: 100%;
    box-sizing: border-box;
  }

  textarea,
  code {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
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

  label.checkbox {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
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

  .revealed {
    margin-top: 16px;
    padding: 14px;
    border: 1px solid var(--danger);
    border-radius: 10px;
  }

  .revealed code {
    display: block;
    word-break: break-all;
    font-size: 13px;
    user-select: all;
  }
</style>
