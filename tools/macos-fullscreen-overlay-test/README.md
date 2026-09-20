# macOS full-screen overlay test

Automated check for one specific, repeatedly-regressed behavior:

> **The break overlay must appear over another app's full-screen Space.**

Run it:

```sh
(cd src-tauri && cargo build)          # once, or after any Rust change
tools/macos-fullscreen-overlay-test/run-test.sh
```

Takes about 40 seconds and needs **no permissions** — no Accessibility, no
Automation, no Screen Recording.

## Why this needs a test at all

This bug has now been "fixed" three times, and twice the fix looked correct from
inside the app. Every AppKit readback the app can make — `isVisible`,
`isOnActiveSpace`, `collectionBehavior`, `NSWindow.level`, the activation
policy — reported exactly the expected values while the user saw nothing but
their full-screen video. A real user's log for the most recent occurrence:

```
prepare_window_before_show: collectionBehavior=337 level=1000  app(policy,active)=(1, false)
configure_window:           collectionBehavior=337 level=1000  isOnActiveSpace=false
reassert_front_after_delay: overlay is off the active Space -- ordering it out and back in
reassert_front_after_delay: isVisible=true isOnActiveSpace=false (before re-place: false)
```

So the verdict here deliberately comes from **outside** the app:
`CGWindowListCopyWindowInfo(.optionOnScreenOnly)` lists only windows on the
*active* Space, which is ground truth the app itself cannot fake.

## How it works

| piece | role |
|---|---|
| `fullscreen-victim.swift` | A separate app that puts *itself* into real macOS fullscreen. Separate process matters: the restriction only applies to *another app's* full-screen Space. Self-drives rather than scripting Safari, so no Automation permission is needed; always leaves fullscreen and quits on its own (`VICTIM_SECONDS`). |
| `onscreen-windows.swift` | The external oracle: windows on the active Space, tab-separated. |
| `screen-size.swift` | Main screen size, so "covers the screen" can be asserted rather than assumed. |
| `run-test.sh` | Sequences the run and prints PASS/FAIL. |

The app is driven by `POMODORO_FORCE_BREAK_ON_START=1` (see `force_break_on_start`
in `src-tauri/src/lib.rs`), with `POMODORO_FORCE_BREAK_DELAY_S` and
`POMODORO_FORCE_BREAK_MINUTES` controlling when the break fires and how long it
is held open.

**Ordering is load-bearing.** The app is launched *first* and the victim goes
full-screen *second*, because launching the app shows its main window, which
makes macOS switch away from the full-screen Space — the break then opens over
an ordinary desktop and passes for the wrong reason. That happened while this
harness was being written, which is why the script also asserts the victim is
still on the active Space before believing a pass.

It also asserts the overlay's origin is `0,0`, not just its Space: a positioning
bug found by this harness left the overlay at `y=-480` on a 1080-tall screen
(covering the top 600px only) while every Space-level check still read as a pass.

## Results

- `PASS` — overlay covers the active Space while the full-screen app owns it.
- `FAIL` — overlay never reached that Space (the shipped bug), or reached it
  without covering it.
- `INCONCLUSIVE` (exit 2) — the victim wasn't on the active Space, so the
  full-screen case was never actually exercised. Not a pass.

## Notes

- It stops any running instance of the app first, because
  `tauri-plugin-single-instance` would otherwise hand the launch to the existing
  process and the `POMODORO_FORCE_BREAK_*` environment would never apply.
- The break is force-closed by killing the process, so nothing is written to
  `reflection` — but the app does use your real `pomodoro.db`, and if
  media-pause-on-break is enabled the break will send a play/pause media key.
- Verify any change to this harness against the pre-fix code (drop the
  `with_accessory_policy` wrap in `overlay::precreate_windows`) and confirm it
  reports FAIL. A test for this bug that cannot fail is worse than no test,
  since every naive check passes on a desktop Space.
