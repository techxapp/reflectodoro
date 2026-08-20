# CLAUDE.md — Tauri + Svelte Project Instructions

## Project Shape
- Svelte version: (4 stores/`$:` vs 5 runes — pin this, patterns below differ)
- Tauri version: (v1 vs v2 — command/event APIs differ)
- State management: (specify)

Verify actual versions from `package.json` / `Cargo.toml` / `tauri.conf.json` before assuming.

## Core Principle
Tauri apps are inherently concurrent: Rust commands/events, the webview loop, and Svelte's
batched reactivity all interleave. Never assume ordering or completion unless it's enforced
in code. Treat every `invoke()`, `listen()`, and reactive block as possibly out of order.

## Race Conditions — Watch For These

1. **Out-of-order `invoke()` responses** (e.g. search-as-you-type). Guard with an
   incrementing request ID; ignore responses that don't match the latest.
2. **Events emitted before `listen()` is attached.** `listen()` is async — `await` it
   before triggering backend work that may emit the event, or have the backend expose a
   "get current status" command as a fallback.
3. **Leaked/duplicate listeners.** Every `listen()` needs an `unlisten()` in cleanup
   (`onDestroy` / `$effect` return). Guard against unmounting before the `listen()` promise
   resolves (a `cancelled` flag).
4. **Stale closures.** Async callbacks capture state at creation time — read current
   state at the moment of use, not from an earlier-captured variable.
5. **Async inside `$:`/`$effect` without a staleness guard.** Never `await` in a bare
   reactive block — always pair with a request-ID check like (1).
6. **Double-invocation on remount.** Guard non-idempotent commands (writes, spawns) with
   an in-flight flag.
7. **Rust-side shared state races.** Don't hold a sync `Mutex` guard across an `.await`;
   use `tokio::sync::Mutex` or a single-writer channel if ordering across calls matters.
8. **New window/webview races.** Don't push data to a freshly created window — wait for a
   ready handshake from it first.

## Other Runtime Practices
- Type every `invoke()` call; catch and surface rejections, don't let them vanish.
- Use commands for request/response; use events only for genuine backend-initiated pushes.
- Debounce user-driven triggers in addition to request-ID guards.
- Clean up all subscriptions/timers/observers, not just `listen()`.
- No duplicate sources of truth for derived state — use `$derived`/`$:`, not manually
  synced copies (this masquerades as a race but is a sync bug).
- When debugging ordering issues, log with correlation IDs, and artificially delay Rust
  commands in dev to make races reproducible.

## Pre-Merge Checklist (async/event changes)
- [ ] Every `listen()` has cleanup, guarded against unmount-before-registration
- [ ] Rapid-fire `invoke()` calls use a request-ID/cancellation guard
- [ ] No unguarded `await` inside `$:` / `$effect`
- [ ] Non-idempotent commands guard against double-invocation
- [ ] No Rust `Mutex` guard held across `.await`
- [ ] `invoke()` errors are caught and surfaced
- [ ] New windows use a ready-handshake before receiving data

## When Adding Async Code Here
1. Default to request-ID guards for anything triggered by repeatable user input.
2. Pair every `listen()` with cleanup in the same scope it's created in.
3. Flag unenforced ordering assumptions, even if not asked to fix them.
4. Prefer command return values over events unless a genuine backend push is needed.
5. Note whether any touched Rust lock is held across an `.await`.