<script lang="ts">
  import { onMount } from "svelte";
  import "./app.css";
  import { page } from "$app/stores";
  import { goto } from "$app/navigation";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { check } from "@tauri-apps/plugin-updater";
  import { relaunch } from "@tauri-apps/plugin-process";
  import { isOnboardingCompleted } from "$lib/db";

  let { children } = $props();

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
  });
</script>

{#if isSpecialWindow}
  {@render children()}
{:else}
  <div class="app-shell">
    <nav>
      <span class="brand">Reflectodoro</span>
      <div class="links">
        {#each links as link}
          <a href={link.href} class:active={$page.url.pathname === link.href}>{link.label}</a>
        {/each}
      </div>
    </nav>
    <main>
      {@render children()}
    </main>
  </div>
{/if}

<style>
  .app-shell {
    display: flex;
    flex-direction: column;
    height: 100vh;
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
    /* Bottom inset keeps content clear of the gesture-nav bar; left/right
       cover a notch/cutout in landscape. Top is handled on `nav` instead. */
    padding-bottom: var(--safe-bottom);
    padding-left: var(--safe-left);
    padding-right: var(--safe-right);
  }
</style>
