<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import "./app.css";
  import { page } from "$app/stores";
  import { goto } from "$app/navigation";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { check } from "@tauri-apps/plugin-updater";
  import { relaunch } from "@tauri-apps/plugin-process";
  import { isOnboardingCompleted, listenForScreenTimeSessionBatches } from "$lib/db";

  let { children } = $props();

  // How much of the viewport the on-screen keyboard is currently covering.
  // visualViewport's resize event is wired up here but doesn't actually fire
  // on its own: MainActivity.kt's enableEdgeToEdge() opts this Activity out
  // of the classic windowSoftInputMode adjustResize/adjustPan behavior
  // (confirmed on a real API 29 device -- window.innerHeight and
  // visualViewport.height stayed identical with the keyboard visibly up).
  // The real height instead comes from MainActivity's own
  // getWindowVisibleDisplayFrame-based listener via __setKeyboardInset,
  // with visualViewport kept as a harmless extra signal in case it ever
  // does fire (e.g. on non-edge-to-edge builds).
  let unlistenScreenTime: UnlistenFn | null = null;
  let keyboardInset = $state(0);
  let nativeKeyboardInset = 0;
  let mainEl: HTMLElement | undefined = $state();

  function updateKeyboardInset() {
    const vv = window.visualViewport;
    const vvInset = vv ? Math.max(0, window.innerHeight - vv.height) : 0;
    keyboardInset = Math.max(vvInset, nativeKeyboardInset);
  }

  /** window.innerHeight never shrinks here (see keyboardInset above), so
   * el.scrollIntoView({block:"center"}) would center against the *full*
   * screen height and could still land a field behind the keyboard. This
   * scrolls `main` (the actual scroll container) only as far as needed to
   * clear whatever keyboardInset currently says is covered. */
  function scrollFieldAboveKeyboard(el: HTMLElement) {
    const rect = el.getBoundingClientRect();
    const visibleBottom = window.innerHeight - keyboardInset;
    const margin = 16;
    if (rect.bottom > visibleBottom - margin) {
      mainEl?.scrollBy({ top: rect.bottom - visibleBottom + margin, behavior: "smooth" });
    } else if (rect.top < margin) {
      mainEl?.scrollBy({ top: rect.top - margin, behavior: "smooth" });
    }
  }

  /** Delegated to `main` (focusin bubbles, unlike focus) so every regular
   * tab's inputs get keyboard-avoidance for free instead of each page
   * wiring its own focus handler. */
  function onMainFocusIn(e: FocusEvent) {
    const el = e.target as HTMLElement;
    if (!(el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement)) return;
    // Gives the keyboard's open animation a moment to start -- scrolling
    // immediately can land at the pre-keyboard scroll position, and
    // __setKeyboardInset (below) re-scrolls once the real height is known.
    setTimeout(() => scrollFieldAboveKeyboard(el), 150);
  }

  const links = [
    { href: "/", label: "Timer" },
    { href: "/entries", label: "Entries" },
    { href: "/settings", label: "Settings" },
    { href: "/about", label: "About" },
  ];

  const isSpecialWindow = $derived(
    $page.url.pathname === "/overlay" ||
      $page.url.pathname === "/checkin" ||
      $page.url.pathname === "/onboarding",
  );

  /** Runs once per app boot (main window only). Checks GitHub Releases'
   * latest.json for a newer signed build; if found and the user accepts,
   * downloads, installs, and relaunches into the new version. */
  onMount(async () => {
    if (isSpecialWindow) return;
    try {
      const update = await check();
      if (update && confirm(`A new version (${update.version}) is available. Update now?`)) {
        await update.downloadAndInstall();
        await relaunch();
      }
    } catch (e) {
      console.error("update check failed", e);
    }
  });

  /** Registered here, not in the home route's own +page.svelte, because
   * this layout is the thing that actually stays mounted across client-side
   * tab navigation ("/" -> "/entries" -> "/settings" -> ...) within the main
   * window -- +page.svelte for "/" unmounts (and tears down its listeners)
   * the instant the user navigates to another tab, same as any other route.
   * Screen-time batches Rust flushes while the user is sitting on, say, the
   * Entries tab checking for new data were being silently and permanently
   * dropped as a result: Tauri's emit() returns Ok even when nothing is
   * listening (confirmed against tauri 2.11.5's source -- emit_js_filter
   * just iterates whatever handlers exist for that webview/event pair and
   * is a no-op if there are none), so PENDING_FLUSH was drained in Rust with
   * no error anywhere and no way to re-request what was lost. Gated on
   * !isSpecialWindow so the separate overlay/checkin *windows* (desktop) --
   * which each run this same root layout in their own webview -- don't also
   * register it and double-save every batch; on Android, where overlay/
   * checkin are reached by client-side navigation within this one window
   * instead (see the Android-only onMount below), the layout never
   * unmounts for that navigation either, so this listener stays live
   * through it without needing a separate carve-out. */
  onMount(async () => {
    if (isSpecialWindow) return;
    unlistenScreenTime = await listenForScreenTimeSessionBatches();
  });

  onDestroy(() => {
    unlistenScreenTime?.();
  });

  /** Android has exactly one Activity/window -- there's no OS-level
   * always-on-top second window the way desktop has, and creating a second
   * WebviewWindow there doesn't layer a new surface over "main", it replaces
   * the single Activity's visible content (confirmed empirically). So on
   * Android the break overlay and check-in are reached by client-side
   * navigation within this same window instead, driven by the same events
   * Rust already emits unconditionally on every platform -- overlay.rs's
   * spawn_or_update_overlay/close_overlay/open_checkin_for_slot just skip
   * the window-creation calls on mobile and emit state regardless. Desktop
   * doesn't need this: those events there only update state inside windows
   * Rust has already shown/hidden directly. */
  onMount(async () => {
    const os = await invoke<string>("current_os");
    if (os !== "android") return;

    // First-launch-only: routes to the permissions onboarding screen before
    // the user sees anything else. Checked once here rather than in
    // onboarding's own onMount so a completed onboarding never even briefly
    // flashes the main UI before redirecting away and back.
    if (!(await isOnboardingCompleted()) && $page.url.pathname !== "/onboarding") {
      void goto("/onboarding");
    }

    await listen<{ open: boolean }>("overlay://state", (event) => {
      if (event.payload.open) {
        if ($page.url.pathname !== "/overlay") void goto("/overlay");
      } else if ($page.url.pathname === "/overlay") {
        void goto("/");
      }
    });
    await listen("checkin://slot", () => {
      if ($page.url.pathname !== "/checkin") void goto("/checkin");
    });

    (window as unknown as Record<string, unknown>).__setKeyboardInset = (px: number) => {
      nativeKeyboardInset = px || 0;
      updateKeyboardInset();
      const active = document.activeElement;
      if (nativeKeyboardInset > 0 && active instanceof HTMLElement && active !== document.body) {
        scrollFieldAboveKeyboard(active);
      }
    };
    window.visualViewport?.addEventListener("resize", updateKeyboardInset);
    updateKeyboardInset();
  });
</script>

{#if isSpecialWindow}
  {@render children()}
{:else}
  <div class="app-shell" style="--kb-inset: {keyboardInset}px">
    <nav>
      <span class="brand">Reflectodoro</span>
      <div class="links">
        {#each links as link}
          <a href={link.href} class:active={$page.url.pathname === link.href}>{link.label}</a>
        {/each}
      </div>
    </nav>
    <main bind:this={mainEl} onfocusin={onMainFocusIn}>
      {@render children()}
    </main>
  </div>
{/if}

<style>
  .app-shell {
    display: flex;
    flex-direction: column;
    height: 100vh;
    /* Explicit rather than left to inherit through from body's -- on at
       least one real device (Honor/MediaTek) the whole shell (nav and the
       backdrop behind main's content cards, both otherwise transparent) was
       left showing a stale black frame after the Android native break
       overlay (a separate fullscreen WindowManager surface, see
       NativeOverlayManager.kt) closed, even though content that already had
       its own explicit background (the cards, the active/hover tab pills)
       repainted correctly. Painting the shell's own box explicitly removes
       its reliance on this WebView correctly compositing a transparent
       layer through to whatever was behind it after another window stopped
       covering the screen. */
    background: var(--bg);
  }

  nav {
    display: flex;
    align-items: center;
    justify-content: space-between;
    /* Top inset pushes the nav below the status bar (Android edge-to-edge
       draws under it by default); left/right cover a notch/cutout in
       landscape. Bottom is handled on `main` instead, not here. */
    padding: calc(12px + var(--safe-top)) calc(20px + var(--safe-right)) 12px
      calc(20px + var(--safe-left));
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
    flex-wrap: wrap;
    row-gap: 8px;
  }

  .brand {
    font-weight: 600;
    font-size: 14px;
    opacity: 0.8;
  }

  .links {
    display: flex;
    gap: 4px;
  }

  .links a {
    padding: 6px 14px;
    border-radius: 8px;
    text-decoration: none;
    color: inherit;
    font-size: 14px;
    opacity: 0.7;
  }

  .links a:hover {
    opacity: 1;
    background: var(--surface-2);
  }

  .links a.active {
    opacity: 1;
    background: var(--accent-soft);
    color: var(--accent);
  }

  main {
    flex: 1;
    overflow-y: auto;
    /* Bottom inset keeps content clear of the gesture-nav bar, plus
       --kb-inset's extra room so a focused field near the bottom (e.g. the
       Timer tab's task list) can still be scrolled up above an open
       keyboard; left/right cover a notch/cutout in landscape. Top is
       handled on `nav` instead. */
    padding-bottom: calc(var(--safe-bottom) + var(--kb-inset, 0px));
    padding-left: var(--safe-left);
    padding-right: var(--safe-right);
  }
</style>
