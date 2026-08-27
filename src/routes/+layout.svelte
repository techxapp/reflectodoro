<script lang="ts">
  import { onMount } from "svelte";
  import "./app.css";
  import { page } from "$app/stores";
  import { check } from "@tauri-apps/plugin-updater";
  import { relaunch } from "@tauri-apps/plugin-process";

  let { children } = $props();

  const links = [
    { href: "/", label: "Timer" },
    { href: "/entries", label: "Entries" },
    { href: "/settings", label: "Settings" },
    { href: "/about", label: "About" },
  ];

  const isSpecialWindow = $derived(
    $page.url.pathname === "/overlay" || $page.url.pathname === "/checkin",
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
    padding: 12px 20px;
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
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
  }
</style>
