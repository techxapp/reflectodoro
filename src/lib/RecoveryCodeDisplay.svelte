<script lang="ts">
  /**
   * Shows a freshly issued app lock recovery code (app_lock.rs) exactly once.
   * Done stays disabled until the user confirms they've saved it, because the
   * code exists nowhere else in plaintext. Shared by Settings' App lock card
   * and the lock screen.
   */
  let { code, onDone }: { code: string; onDone: () => void } = $props();

  let saved = $state(false);
  let copied = $state(false);

  async function copy() {
    try {
      await navigator.clipboard.writeText(code);
      copied = true;
    } catch (e) {
      // No clipboard access (some webviews): the code is on screen to copy by hand.
      console.warn("recovery code copy failed", e);
      copied = false;
    }
  }
</script>

<div class="recovery">
  <p class="title">Your recovery code</p>
  <p class="code" aria-label="Recovery code">{code}</p>
  <p class="hint">
    If you forget your PIN, this code lets you set a new one without erasing anything. It is shown only once and works
    only once &mdash; using it gives you a new code. Keep it in a password manager or on paper, not on this device.
  </p>
  <div class="row">
    <button type="button" class="secondary" onclick={copy}>{copied ? "Copied" : "Copy"}</button>
  </div>
  <label class="check">
    <input type="checkbox" bind:checked={saved} />
    I've saved this code somewhere safe
  </label>
  <button type="button" disabled={!saved} onclick={onDone}>Done</button>
</div>

<style>
  .recovery {
    display: flex;
    flex-direction: column;
    gap: 10px;
    align-items: flex-start;
  }

  .title {
    margin: 0;
    font-size: 14px;
    font-weight: 600;
  }

  .code {
    margin: 0;
    padding: 10px 12px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 8px;
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 17px;
    letter-spacing: 0.08em;
    user-select: all;
    word-break: break-all;
  }

  .hint {
    margin: 0;
    color: var(--text-dim);
    font-size: 13px;
    line-height: 1.5;
  }

  .row {
    display: flex;
    gap: 10px;
  }

  .check {
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
</style>
