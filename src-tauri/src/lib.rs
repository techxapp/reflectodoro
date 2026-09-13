#[cfg(target_os = "android")]
mod android_bridge;
mod breakit;
mod commands;
mod db;
mod grid;
mod hook;
mod import;
mod log_export;
mod macos_overlay;
mod media;
mod native_overlay;
mod overlay;
mod p2p_sync;
mod screen_time;
mod state;

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Local};
#[cfg(desktop)]
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
#[cfg(desktop)]
use tauri::tray::TrayIconBuilder;
#[cfg(desktop)]
use tauri::WindowEvent;
use tauri::{AppHandle, Emitter, Manager};
#[cfg(desktop)]
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use grid::Phase;
use state::{AppState, OverlayState};

/// Debug builds (`npm run tauri dev`) run in dev mode by default: the overlay
/// shows a visible "Close (DEV)" button. The Alt-Tab/Win-key/Cmd-Tab
/// suppression hooks install unconditionally regardless of dev mode (dev
/// builds and release builds behave identically there now) -- the "Close
/// (DEV)" button, plus the always-registered Ctrl/Cmd+Alt+Shift+F12 kill
/// switch, are the way to escape the overlay while testing. Override with
/// POMODORO_DEV_MODE=0/1 if you need to test enforcement in a debug build, or
/// force it on in a release build for QA.
fn resolve_dev_mode() -> bool {
    match std::env::var("POMODORO_DEV_MODE").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => cfg!(debug_assertions),
    }
}

pub(crate) static POMODORO_ENABLED: AtomicBool = AtomicBool::new(true);

/// Epoch-millis resume time for an active "snooze" (Pomodoro mode
/// temporarily paused from the main window's dropdown) -- 0 means no snooze
/// is pending. Set by `commands::snooze_pomodoro`, cleared by
/// `apply_pomodoro_enabled` (so any manual On/Off, from either the main
/// window or the tray, cancels a pending snooze rather than leaving a stale
/// timestamp that could later flip `POMODORO_ENABLED` back on unexpectedly).
/// Checked by wall-clock comparison inside `run_scheduler`'s own poll loop --
/// deliberately not a `tokio::time::sleep`-based timer, since that's
/// `Instant`/`CLOCK_MONOTONIC`-based and would suffer the exact same
/// suspend/Doze bug `ANDROID_POLL_INTERVAL` exists to work around (see its
/// doc comment below).
pub(crate) static POMODORO_SNOOZE_UNTIL_MS: AtomicI64 = AtomicI64::new(0);

/// The duration originally chosen for the active snooze (if any) -- purely so
/// the frontend can re-select the right dropdown option on boot/reload;
/// `POMODORO_SNOOZE_UNTIL_MS` alone is what actually governs resume timing.
pub(crate) static POMODORO_SNOOZE_MINUTES: AtomicU32 = AtomicU32::new(0);

pub(crate) const SNOOZE_MIN_MINUTES: u32 = 30;
pub(crate) const SNOOZE_MAX_MINUTES: u32 = 120;

/// How often `run_scheduler`'s loop re-checks wall-clock time against
/// `POMODORO_SNOOZE_UNTIL_MS` while a snooze is pending, on every platform
/// (not just Android) -- capping `sleep_dur` to this is what makes the poll
/// actually catch the expiry promptly instead of desktop sleeping for up to
/// ~25 minutes until the next grid boundary.
const SNOOZE_POLL_INTERVAL: StdDuration = StdDuration::from_secs(30);

/// Whether Ctrl+Alt+Shift+F12 (Cmd+Option+Shift+F12 on macOS) actually
/// force-closes the overlay. Backed by
/// `app_setting.force_close_shortcut_enabled`; the frontend loads that value
/// and pushes it here on boot and on every Settings save (see
/// `sync_force_close_shortcut_enabled` in commands.rs), the same pattern as
/// `breakit_config`. Defaults to `true` here too, matching the migration's
/// default, so the shortcut still works during the brief window before the
/// frontend's first sync completes.
pub(crate) static FORCE_CLOSE_SHORTCUT_ENABLED: AtomicBool = AtomicBool::new(true);

/// Whether entering a break best-effort-pauses whatever media is playing --
/// on Windows by querying System Media Transport Controls and pausing only
/// sessions actually playing, on macOS by toggling the hardware Play/Pause
/// media key (see media.rs for why those two approaches differ). Backed by
/// `app_setting.media_pause_on_break_enabled`; the frontend loads that value
/// and pushes it here on boot and on every Settings save, the same pattern as
/// `FORCE_CLOSE_SHORTCUT_ENABLED`. Defaults to `true` here too, matching the
/// migration's default.
pub(crate) static MEDIA_PAUSE_ON_BREAK_ENABLED: AtomicBool = AtomicBool::new(true);

/// Android only: whether the break notification (see overlay.rs's Android
/// arm of spawn_or_update_overlay / android_bridge.rs's trigger_break_screen)
/// is posted as non-dismissible (can't be swiped away, only cleared once the
/// break actually resolves) versus a normal dismissible one. Backed by
/// `app_setting.break_notification_persistent_enabled`; same load/push
/// pattern as `MEDIA_PAUSE_ON_BREAK_ENABLED`. Defaults to `true` here too,
/// matching the migration's default -- the whole point of this notification
/// is to stop the user from using the phone for something else during a
/// break, not just to politely mention it.
pub(crate) static BREAK_NOTIFICATION_PERSISTENT_ENABLED: AtomicBool = AtomicBool::new(true);

/// Whether foreground-app focus tracking is running (see screen_time.rs).
/// Backed by `app_setting.screen_time_tracking_enabled`; same load/push
/// pattern as `MEDIA_PAUSE_ON_BREAK_ENABLED`. Defaults to `true` here too,
/// matching the migration's default -- on by default. Checked inside
/// screen_time.rs's `record_focus_change`, not by installing/uninstalling the
/// OS watcher, so toggling it never touches a live hook.
pub(crate) static SCREEN_TIME_TRACKING_ENABLED: AtomicBool = AtomicBool::new(true);

/// macOS only: whether a break additionally hides the menu bar and Dock (see
/// macos_overlay.rs). This is the *only* part of macOS break enforcement
/// gated behind a Settings toggle -- following the user to every Space
/// (including another app's full-screen Space) and blocking Cmd+Tab are both
/// always on, dev_mode or not, same tier as fullscreen/always-on-top. Backed
/// by `app_setting.macos_hide_menu_bar_dock_enabled`; same load/push pattern
/// as `MEDIA_PAUSE_ON_BREAK_ENABLED`. Defaults to `false` here too, matching
/// the migration's default -- opt-in, unlike the other toggles above, since
/// hiding system UI is a materially more disruptive change to the user's
/// desktop than anything else this app does.
pub(crate) static MACOS_HIDE_MENU_BAR_DOCK_ENABLED: AtomicBool = AtomicBool::new(false);

/// macOS-only media-toggle guard state (see media.rs's macos_impl module for
/// the full rationale). Both are RFC3339/ISO8601 UTC strings, same convention
/// as `reflection.created_at`/`wellness_check.created_at`, so they're safe to
/// compare lexicographically. `LAST_MEDIA_TOGGLE_AT` is written by media.rs
/// itself the instant it fires the toggle (and separately persisted to
/// `app_setting.last_toggle_time` by the frontend via the
/// `media-toggle://recorded` event, so it survives a crash/relaunch mid-break).
/// `LAST_WELLNESS_CHECK_AT` is pushed from the frontend on boot and again
/// after every completed (non-skipped) check-in -- see
/// `sync_last_wellness_check_at` in commands.rs.
pub(crate) static LAST_MEDIA_TOGGLE_AT: Mutex<Option<String>> = Mutex::new(None);
pub(crate) static LAST_WELLNESS_CHECK_AT: Mutex<Option<String>> = Mutex::new(None);

/// How long after a break ends the overlay force-closes even without a
/// reflection. Backed by `app_setting.overlay_auto_close_minutes`; the
/// frontend loads that value and pushes it here on boot and on every
/// Settings save (see `sync_overlay_auto_close_to_backend` in db.ts), the
/// same pattern as `breakit_config`/`FORCE_CLOSE_SHORTCUT_ENABLED`. Defaults
/// to 5 here too, matching the migration's default, so the timeout still
/// applies during the brief window before the frontend's first sync
/// completes. See `overlay::schedule_auto_close`.
pub(crate) static OVERLAY_AUTO_CLOSE_MINUTES: AtomicU32 = AtomicU32::new(5);

/// How far past its intended wake instant a loop iteration has to land
/// before it's treated as "the process was just suspended/hibernated
/// through this", rather than ordinary scheduling jitter. `tokio::time::sleep`
/// is driven by a monotonic clock that itself stops ticking across a real
/// suspend, so comparing wall-clock `Local::now()` against the wake instant
/// computed *before* sleeping is what actually detects the gap.
///
/// This threshold only gates *re-evaluating* the current phase (resetting
/// `last_phase` so a resume landing back inside the same nominal phase still
/// gets checked) -- it does NOT by itself decide whether to force-close an
/// unresolved overlay. That's `OVERLAY_AUTO_CLOSE_MINUTES` (the user's own
/// configured grace period), same bar `schedule_auto_close` applies to an
/// ordinary (non-suspend) unresolved break. An earlier version force-closed
/// on any gap over this flat 120s, on every platform, regardless of the
/// user's configured grace period -- so closing a laptop lid for 3 minutes
/// during a break force-closed the overlay immediately, while just sitting
/// at the desk not responding bought the user the full grace period (5 min
/// default) before the same thing happened. See `run_scheduler`.
const SUSPEND_GAP_THRESHOLD: StdDuration = StdDuration::from_secs(120);

/// Android only: caps how long a single scheduler sleep waits before
/// re-checking wall-clock time against the grid. `tokio::time::sleep`
/// schedules against `Instant`/`CLOCK_MONOTONIC`, which does not advance
/// while the CPU is actually suspended (unlike `CLOCK_BOOTTIME`) -- so a
/// single multi-minute sleep spanning a real Doze/deep-suspend period keeps
/// waiting for its full *remaining monotonic* duration even once the device
/// wakes, rather than firing as soon as wall-clock time has passed the
/// boundary. `BreakAlarmReceiver`'s `AlarmManager.setAlarmClock` chain only
/// guarantees a brief wake window, not a long enough one for a large
/// monotonic deficit to fully elapse -- so a break-end transition could be
/// delayed indefinitely while the phone sits idle. Polling short-circuits
/// that: each iteration recomputes `slot_for(Local::now())` from the wall
/// clock, so even a briefly-awake CPU is enough to notice the boundary was
/// already crossed, regardless of how much monotonic time that particular
/// sleep call thinks has passed. Desktop doesn't need this -- an actual
/// laptop suspend is caught by `SUSPEND_GAP_THRESHOLD` below once the single
/// long sleep does eventually return.
#[cfg(target_os = "android")]
pub(crate) const ANDROID_POLL_INTERVAL: StdDuration = StdDuration::from_secs(20);

fn generate_breakit_challenge(app: &AppHandle) -> String {
    let app_state = app.state::<AppState>();
    let cfg = app_state.breakit_config.lock().unwrap();
    let challenge = breakit::generate_challenge(cfg.length, cfg.include_special);
    log::info!(
        "generate_breakit_challenge: cfg.length={} include_special={} generated_len={} value={:?}",
        cfg.length,
        cfg.include_special,
        challenge.chars().count(),
        challenge
    );
    challenge
}

/// Applies a Pomodoro mode on/off change and keeps every side effect (event
/// emission, Android's foreground service + boot-recovery pref) in one place
/// -- shared by `commands::set_enabled` (the main window's On/Off dropdown
/// options), the tray "toggle" menu item, and `run_scheduler`'s own
/// snooze-expiry auto-resume below, so none of the three can drift out of
/// sync with each other. Always clears any pending snooze: a manual On/Off
/// from any of these three places should cancel a snooze outright rather than
/// leaving `POMODORO_SNOOZE_UNTIL_MS` armed to unexpectedly flip things back
/// later.
pub(crate) fn apply_pomodoro_enabled(app: &AppHandle, enabled: bool) {
    POMODORO_ENABLED.store(enabled, Ordering::SeqCst);
    POMODORO_SNOOZE_UNTIL_MS.store(0, Ordering::SeqCst);
    POMODORO_SNOOZE_MINUTES.store(0, Ordering::SeqCst);
    let _ = app.emit("pomodoro://enabled-changed", enabled);
    let _ = app.emit("pomodoro://snooze-changed", Option::<commands::SnoozeInfo>::None);

    // Keeps the Android foreground service (and its AlarmManager backup) in
    // step with the toggle: starting it when the user turns Pomodoro mode
    // on (mirrors the same call in lib.rs's .setup(), for the already-on
    // default at launch) and stopping it when they turn it off, so
    // disabling actually lets Android reclaim the process instead of
    // leaving a phantom "running" notification behind.
    #[cfg(target_os = "android")]
    {
        let bridge = app.state::<android_bridge::AndroidBridge<tauri::Wry>>();
        let result = if enabled {
            bridge.start_foreground_service()
        } else {
            bridge.stop_foreground_service()
        };
        if let Err(e) = result {
            log::error!("failed to toggle Android foreground service: {e:?}");
        }
        // So a reboot (BootCompletedReceiver, which runs before any Rust
        // runtime exists in that fresh process) can respect a deliberate
        // "off" choice instead of always re-arming everything -- see
        // PomodoroEnabledPref's doc comment (Kotlin).
        if let Err(e) = bridge.persist_pomodoro_enabled(enabled) {
            log::error!("failed to persist pomodoro-enabled preference: {e:?}");
        }
        // Clears any persisted snooze-until alongside the in-memory atomics
        // above, so a killed-and-relaunched process doesn't resurrect a
        // snooze that was already cancelled (manually, or by the scheduler's
        // own auto-resume) before it died. See commands::snooze_pomodoro.
        if let Err(e) = bridge.persist_pomodoro_snooze_until(0, 0) {
            log::error!("failed to clear persisted snooze-until: {e:?}");
        }
    }
}

async fn run_scheduler(app: AppHandle) {
    let mut last_phase: Option<Phase> = None;
    let mut expected_wake: Option<DateTime<Local>> = None;

    loop {
        let now = Local::now();

        // Wall-clock (not a separate timer) check for a pending snooze
        // (POMODORO_SNOOZE_UNTIL_MS -- see commands::snooze_pomodoro) having
        // expired. Deliberately not a `tokio::time::sleep`-based timer: that
        // would be `Instant`/`CLOCK_MONOTONIC`-based and could fire far later
        // than intended across a real suspend/Doze gap, the same class of bug
        // `ANDROID_POLL_INTERVAL` below exists to work around -- polling this
        // loop's own wall clock sidesteps it the same way. `sleep_dur` is
        // capped to `SNOOZE_POLL_INTERVAL` further below whenever a snooze is
        // pending so this check actually runs often enough to matter.
        if !POMODORO_ENABLED.load(Ordering::SeqCst) {
            let until_ms = POMODORO_SNOOZE_UNTIL_MS.load(Ordering::SeqCst);
            if until_ms != 0 && now.timestamp_millis() >= until_ms {
                log::info!("scheduler: snooze expired, resuming Pomodoro mode");
                apply_pomodoro_enabled(&app, true);
                // Same trick as the suspend-gap branch below: force the
                // transition check to re-evaluate the current phase from
                // scratch, so a snooze expiring while the wall clock is
                // already inside a Break window opens the overlay
                // immediately instead of waiting for the next real phase
                // transition (up to ~25 minutes away).
                last_phase = None;
            }
        }

        // If we woke up much later than the last iteration scheduled for,
        // the process was almost certainly suspended/hibernated in between.
        // A stale overlay left open from before the gap won't necessarily
        // hit the normal Work-phase-transition path below (e.g. if `now`
        // happens to land back inside a Break window, `last_phase` already
        // reads Break and no transition fires at all) -- so it's handled
        // directly here instead of relying on that path or the
        // OVERLAY_AUTO_CLOSE_MINUTES grace timer, which wouldn't even get
        // scheduled in that case.
        if let Some(expected) = expected_wake {
            let overslept = (now - expected).to_std().unwrap_or(StdDuration::ZERO);
            if overslept > SUSPEND_GAP_THRESHOLD {
                log::info!(
                    "scheduler: woke {}s later than expected -- treating as a suspend/hibernate gap",
                    overslept.as_secs()
                );
                // Only force-close an overlay the user never got a chance to
                // respond to once the suspend itself ran longer than their
                // own configured grace period -- the same bar
                // `schedule_auto_close` applies to an ordinary (non-suspend)
                // unresolved break. See `SUSPEND_GAP_THRESHOLD`'s doc comment
                // for why a flat 120s bar here was unfair relative to that.
                let grace = StdDuration::from_secs(
                    OVERLAY_AUTO_CLOSE_MINUTES.load(Ordering::SeqCst) as u64 * 60,
                );
                if overslept > grace {
                    overlay::force_close_stale_overlay(&app);
                }
                // Forces the transition check below to run regardless of
                // whether `now`'s phase nominally matches what it was before
                // the gap -- otherwise a resume that happens to land back
                // inside a Break window would see last_phase == Break, skip
                // the transition entirely, and leave a stale overlay (if it
                // wasn't force-closed above) unreplaced for the rest of that
                // live break. Reset unconditionally on any suspend-sized gap,
                // independent of the grace-period check above.
                last_phase = None;
            }
        }

        let slot = grid::slot_for(now);

        if last_phase != Some(slot.phase) {
            log::info!(
                "scheduler: phase transition {:?} -> {:?} at slot {}",
                last_phase,
                slot.phase,
                slot.start_iso()
            );
            if POMODORO_ENABLED.load(Ordering::SeqCst) {
                match slot.phase {
                    Phase::Break => {
                        let this_slot_start = slot.start_iso();
                        {
                            let state = app.state::<AppState>();
                            let mut ov = state.overlay.lock().unwrap();
                            *ov = OverlayState::opened_for(this_slot_start.clone(), generate_breakit_challenge(&app));
                        }
                        // Guards against the startup webview blank-page race
                        // when the app boots straight into a live break --
                        // a no-op once the app's been running a while.
                        overlay::wait_for_webview_warmup(&app).await;

                        // State was committed as `open` *before* the await
                        // above, and `spawn_or_update_overlay` doesn't re-read
                        // it -- it only checks whether the window happens to
                        // be visible. If something closed the overlay during
                        // the (up to ~6s) warmup wait -- the F12 kill switch,
                        // dev force-close, or a suspend-gap force-close, all
                        // of which reset state to `OverlayState::closed()` --
                        // showing the window here anyway would present a
                        // fullscreen, close-blocked, Win-key-suppressing
                        // window while `OverlayState.open == false`: the
                        // breakit challenge is `""` so that exit is gone, the
                        // Work-transition's `if ov.open` check does nothing so
                        // `time_expired` never gets set, and `slot_start`
                        // being empty means no auto-close ever gets armed --
                        // stuck until the next real phase transition, ~25
                        // minutes later. Re-checking here and skipping the
                        // show if something else already won that race lets
                        // that close stick, matching how every other
                        // force-close in this app already behaves (see
                        // schedule_auto_close's own slot-equality guard).
                        let still_current = {
                            let state = app.state::<AppState>();
                            let ov = state.overlay.lock().unwrap();
                            ov.open && ov.current_slot_start == this_slot_start
                        };
                        if still_current {
                            // The challenge generated above may have used
                            // AppState.breakit_config's hardcoded startup
                            // default rather than the user's saved setting --
                            // on a cold process start landing directly inside
                            // a live break (the common case for Android's
                            // BreakAlarmReceiver recovery path), the
                            // frontend's async `sync_breakit_config` push
                            // can't possibly have landed yet at that point.
                            // Regenerating here, after the warmup wait above
                            // has given it a real chance to land, is free:
                            // the overlay hasn't been shown to anyone yet
                            // either way.
                            {
                                let state = app.state::<AppState>();
                                let mut ov = state.overlay.lock().unwrap();
                                ov.breakit_challenge = generate_breakit_challenge(&app);
                            }
                            overlay::spawn_or_update_overlay(&app).await;
                        } else {
                            log::info!(
                                "scheduler: overlay for slot {this_slot_start} was closed during webview warmup -- skipping stale show"
                            );
                        }
                    }
                    Phase::Work => {
                        let slot_start = {
                            let state = app.state::<AppState>();
                            let mut ov = state.overlay.lock().unwrap();
                            if ov.open {
                                ov.time_expired = true;
                            }
                            ov.current_slot_start.clone()
                        };
                        overlay::try_close_if_unlocked(&app);
                        // If the unlock formula didn't already close it above
                        // (no reflection yet), force-close it after the
                        // configured grace period regardless -- see
                        // OVERLAY_AUTO_CLOSE_MINUTES and schedule_auto_close's
                        // own guard against a slot that already moved on.
                        if !slot_start.is_empty() {
                            overlay::schedule_auto_close(&app, slot_start);
                        }
                    }
                }
            }
            last_phase = Some(slot.phase);
        }

        let now_before_sleep = Local::now();
        let sleep_dur = (slot.end - now_before_sleep)
            .to_std()
            .unwrap_or(StdDuration::from_secs(1));
        #[cfg(target_os = "android")]
        let sleep_dur = sleep_dur.min(ANDROID_POLL_INTERVAL);
        // Caps the sleep on every platform (not just Android) whenever a
        // snooze is pending, so the wall-clock check above actually runs
        // often enough to resume close to on time -- without this, desktop
        // would otherwise sleep until the next grid boundary (up to ~25
        // minutes away) regardless of how soon the snooze is due to expire.
        let snooze_pending = !POMODORO_ENABLED.load(Ordering::SeqCst)
            && POMODORO_SNOOZE_UNTIL_MS.load(Ordering::SeqCst) != 0;
        let sleep_dur = if snooze_pending {
            sleep_dur.min(SNOOZE_POLL_INTERVAL)
        } else {
            sleep_dur
        };
        // Refreshes MainActivity.lastSchedulerHeartbeatAt every iteration
        // (at least every ANDROID_POLL_INTERVAL, thanks to the cap above) so
        // BreakAlarmReceiver can tell a genuinely live scheduler apart from
        // one whose task died without taking the whole process down with it
        // -- see MainActivity.isSchedulerAlive's doc comment.
        #[cfg(target_os = "android")]
        {
            let bridge = app.state::<android_bridge::AndroidBridge<tauri::Wry>>();
            if let Err(e) = bridge.report_scheduler_heartbeat() {
                log::warn!("report_scheduler_heartbeat failed: {e:?}");
            }
        }
        // `expected_wake` has to reflect *this specific sleep's* actual
        // duration, not the raw slot boundary (`slot.end`) -- on Android,
        // where `sleep_dur` gets capped to `ANDROID_POLL_INTERVAL` (20s)
        // above, setting it to the uncapped `slot.end` (up to ~25 minutes
        // away) meant `now - expected_wake` at the top of the next iteration
        // was always deeply negative (the next iteration wakes ~20s later,
        // nowhere near that distant boundary), so `.to_std()` always failed,
        // `unwrap_or(ZERO)` always won, and the suspend-gap branch above
        // could never fire on Android at all -- it was checking against a
        // wake time this loop was never actually trying to hit.
        expected_wake = Some(
            now_before_sleep
                + chrono::Duration::from_std(sleep_dur).unwrap_or(chrono::Duration::seconds(1)),
        );
        tokio::time::sleep(sleep_dur).await;
    }
}

// Shared by the tray "Open" item and the single-instance re-launch handler --
// both need to bring the (hidden, not destroyed -- see the CloseRequested
// handler below) main window back. Errors are logged rather than silently
// discarded (`let _ = ...`) because a failure here is otherwise invisible:
// exactly the "no thrown error visible to the user" failure mode this
// codebase has already hit twice (see CLAUDE.md's Known gotchas).
//
// macOS-only: `window.show()`/`set_focus()` alone can fail to visually raise
// the window if the whole app (NSApp), not just this window, is inactive --
// a known Tauri/macOS gap. `AppHandle::show()` on macOS is a distinct,
// app-level call (NSApp unhide + activate, akin to undoing Cmd+H) with no
// equivalent/need on Windows or Linux.
#[cfg(desktop)]
fn open_main_window(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    if let Err(e) = app.show() {
        log::error!("failed to activate app on macOS before showing main window: {e:?}");
    }
    if let Some(win) = app.get_webview_window("main") {
        if let Err(e) = win.show() {
            log::error!("failed to show main window: {e:?}");
        }
        if let Err(e) = win.set_focus() {
            log::error!("failed to focus main window: {e:?}");
        }
    } else {
        log::error!("open_main_window: no window labeled \"main\" found");
    }
}

#[cfg(desktop)]
fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    let open_item = MenuItem::with_id(app, "open", "Open Reflectodoro", true, None::<&str>)?;
    let toggle_item = MenuItem::with_id(app, "toggle", "Disable Pomodoro Mode", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open_item, &toggle_item, &separator, &quit_item])?;

    TrayIconBuilder::new()
        .menu(&menu)
        .icon(app.default_window_icon().unwrap().clone())
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "open" => open_main_window(app),
            "toggle" => {
                let enabled = !POMODORO_ENABLED.load(Ordering::SeqCst);
                apply_pomodoro_enabled(app, enabled);
                let label = if enabled {
                    "Disable Pomodoro Mode"
                } else {
                    "Enable Pomodoro Mode"
                };
                let _ = toggle_item.set_text(label);
            }
            "quit" => {
                // Deliberately a hard process exit, not app.exit()/window.close():
                // this must work unconditionally as a kill switch even while the
                // break overlay's close-requested handler is actively blocking
                // normal window close attempts.
                std::process::exit(0);
            }
            _ => {}
        })
        .build(app)?;

    Ok(())
}

#[cfg(desktop)]
fn setup_dev_kill_switch(app: &AppHandle) -> anyhow::Result<()> {
    // Always registered (not just in dev builds): cheap insurance against a
    // stuck overlay in production too. Task Manager and tray Quit are the
    // other two independent kill switches. Registration itself is
    // unconditional; whether it actually does anything is gated on
    // FORCE_CLOSE_SHORTCUT_ENABLED (Settings toggle) so disabling it doesn't
    // require fighting the OS over re-registering/unregistering a global
    // hotkey at runtime.
    //
    // No Android equivalent is registered: swipe-away-from-Recents / Force
    // Stop in Android Settings is always available regardless of anything
    // this app does, the same structural role Task Manager plays on
    // desktop -- see the Android release plan.
    // macOS convention swaps Ctrl->Cmd and Alt->Option: Cmd+Option+Shift+F12
    // there, Ctrl+Alt+Shift+F12 everywhere else. Modifiers::SUPER maps to Cmd
    // on macOS in tauri-plugin-global-shortcut.
    let modifiers = if cfg!(target_os = "macos") {
        Modifiers::SUPER | Modifiers::ALT | Modifiers::SHIFT
    } else {
        Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT
    };
    let shortcut = Shortcut::new(Some(modifiers), Code::F12);
    app.global_shortcut().on_shortcut(shortcut, move |app, _shortcut, event| {
        if event.state() == ShortcutState::Pressed
            && FORCE_CLOSE_SHORTCUT_ENABLED.load(Ordering::SeqCst)
        {
            overlay::close_overlay(app);
        }
    })?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // WebView2's GPU-accelerated compositor loses its DirectX swapchain when
    // the physical display powers off (monitor sleep/blank, distinct from
    // the process-suspend gap SUSPEND_GAP_THRESHOLD handles) and never
    // requests a repaint when the display comes back -- so a webview left
    // showing across a monitor-off/on cycle (most consequentially: the break
    // overlay, which is meant to be inescapable) renders solid white until
    // something else forces a repaint (right-click's context menu, a
    // reload). Disabling GPU acceleration for WebView2's own subprocess
    // sidesteps the whole bug class; must be set before the first
    // WebviewWindow is built, since WebView2 reads it only when its
    // environment is created, so this has to run before any window --
    // including ones tauri.conf.json declares -- comes into existence.
    // Windows-only: WebView2 is the Windows-only webview backend (macOS/
    // Linux use WKWebView/WebKitGTK, unaffected). Safety: single-threaded at
    // this point, before the Tokio runtime or any other thread starts.
    //
    // `--disable-features=CalculateNativeWinOcclusion` is a second, separate
    // workaround bundled in here for the same reason (must precede window
    // creation): Chromium's Windows-only native window occlusion tracker
    // marks a window "occluded" (and throttles its renderer/timers, same
    // treatment as a backgrounded tab) whenever another window fully covers
    // it on screen -- exactly what happens to "main" for the whole
    // break+check-in duration, since the overlay is always-on-top/fullscreen
    // and the check-in popup that follows it is always-on-top/maximized.
    // Symptom actually reported in the wild: the main window's countdown
    // frozen on a stale time after the check-in window closed and "main"
    // became visible again -- a stuck-renderer bug distinct from the
    // solid-white swapchain-loss one above (that one needs a monitor
    // power-cycle to reproduce; this one just needs an ordinary break to run
    // its course with another window on top of "main" the whole time).
    #[cfg(target_os = "windows")]
    unsafe {
        std::env::set_var(
            "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
            "--disable-gpu --disable-features=CalculateNativeWinOcclusion",
        );
    }

    let dev_mode = resolve_dev_mode();

    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default();

    // Single Instance must be the very first plugin registered (per the
    // plugin's own docs) so it can intercept a second launch before
    // anything else -- window/tray/overlay setup, the scheduler, etc. --
    // has a chance to run. When a second instance starts (e.g. an autostart
    // entry firing while a previous instance is still shutting down, or the
    // user double-launching the AppImage), this callback runs in the
    // *existing* instance and the new process exits immediately instead of
    // standing up a second WebView/EGL/tray/scheduler in parallel -- which
    // is what produced the "two instances fighting over the same state"
    // symptoms seen after enabling autostart. Desktop-only for the same
    // reason as the autostart/global-shortcut/updater/process block below:
    // no Android/iOS equivalent, and Cargo.toml already excludes the crate
    // from that target.
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            open_main_window(app);
        }));
    }

    builder = builder
        .manage(AppState::new(dev_mode))
        .manage(p2p_sync::P2pState::new())
        .plugin(tauri_plugin_notification::init())
        .plugin(
            tauri_plugin_sql::Builder::default()
                .add_migrations(db::DB_URL, db::migrations())
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_log::Builder::new()
                .targets([
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir {
                        file_name: None,
                    }),
                    // Also mirrors into each window's devtools console, so
                    // frontend `@tauri-apps/plugin-log` calls (info/warn/error)
                    // land in the same log file as these Rust-side ones --
                    // useful for the hidden checkin/overlay windows,
                    // which don't have a devtools window open by default to
                    // read console output from directly.
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Webview),
                ])
                .level(log::LevelFilter::Info)
                // Overrides two defaults that would otherwise quietly destroy
                // exactly the evidence a real-device bug report needs:
                // RotationStrategy::KeepOne (the plugin's default) doesn't
                // archive on rotation, it *deletes* the whole file and starts
                // over -- and the default max_file_size (40KB) is small
                // enough that ordinary Info-level logging (a phase transition
                // + breakit challenge every ~25min, plus the macOS overlay
                // path's logging) can fill and wipe it in about a day of
                // normal use. KeepSome(10) archives up to 10 rotated,
                // date-stamped files instead of deleting, and 50KB (roughly
                // a day of normal-use logging before rotating) keeps a bug
                // reported "sometime today" almost certainly still in the
                // active file or the most recent archive, while keeping
                // each individual file small enough to paste into a
                // conversation whole -- 11 files at this cap span roughly
                // 1-2 weeks of retained history. See CLAUDE.md's
                // "Debugging from production logs".
                .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepSome(10))
                .max_file_size(50_000)
                .build(),
        );

    // autostart/global-shortcut/updater/process are all desktop concepts
    // with no Android equivalent attempted in this port -- Cargo.toml
    // already excludes these crates from the Android/iOS dependency graph,
    // so referencing them unconditionally here is a hard compile error on
    // mobile, not just dead functionality.
    #[cfg(desktop)]
    {
        builder = builder
            .plugin(tauri_plugin_autostart::init(
                tauri_plugin_autostart::MacosLauncher::LaunchAgent,
                None,
            ))
            .plugin(tauri_plugin_global_shortcut::Builder::new().build())
            .plugin(tauri_plugin_updater::Builder::new().build())
            .plugin(tauri_plugin_process::init());
    }

    #[cfg(target_os = "android")]
    {
        builder = android_bridge::register(builder);
    }

    builder
        .invoke_handler(tauri::generate_handler![
            commands::get_overlay_state,
            commands::is_dev_mode,
            commands::current_os,
            commands::sync_breakit_config,
            commands::mark_reflection_entered,
            commands::report_reflection_save_failure,
            commands::close_after_save_failure,
            commands::breakit_attempt,
            commands::dev_force_close,
            commands::get_enabled,
            commands::set_enabled,
            commands::snooze_pomodoro,
            commands::get_snooze_until,
            commands::get_checkin_slot,
            commands::read_text_file,
            commands::write_text_file,
            commands::get_autostart_enabled,
            commands::set_autostart_enabled,
            commands::get_force_close_shortcut_enabled,
            commands::set_force_close_shortcut_enabled,
            commands::get_overlay_auto_close_minutes,
            commands::set_overlay_auto_close_minutes,
            commands::get_media_pause_on_break_enabled,
            commands::set_media_pause_on_break_enabled,
            commands::sync_media_toggle_guard,
            commands::sync_last_wellness_check_at,
            commands::get_break_notification_persistent_enabled,
            commands::set_break_notification_persistent_enabled,
            commands::get_macos_hide_menu_bar_dock_enabled,
            commands::set_macos_hide_menu_bar_dock_enabled,
            commands::get_screen_time_tracking_enabled,
            commands::set_screen_time_tracking_enabled,
            commands::get_current_session_snapshot,
            commands::get_hostname,
            commands::can_draw_overlays,
            commands::request_draw_overlays_permission,
            commands::can_schedule_exact_alarms,
            commands::request_schedule_exact_alarm_permission,
            commands::can_query_usage_stats,
            commands::request_usage_stats_permission,
            import::import_data,
            log_export::export_last_log_file,
            log_export::export_log_archive,
            p2p_sync::start_pairing,
            p2p_sync::cancel_pairing,
            p2p_sync::browse_pairing_candidates,
            p2p_sync::confirm_pairing,
            p2p_sync::get_paired_devices,
            p2p_sync::browse_online_paired_devices,
            p2p_sync::forget_paired_device,
            p2p_sync::sync_with_device,
            p2p_sync::resync_advertised_name,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();

            // Deliberately NOT force-enabling autostart here on every boot --
            // the Settings toggle now lets the user turn it off, and the OS
            // registration itself is the only record of that choice (see
            // commands::get/set_autostart_enabled). Force-enabling on every
            // launch would silently undo an explicit "off" the next time the
            // app starts. Defaulting it on for a brand new install instead
            // happens once, from the frontend -- see `ensureDefaultAutostart`
            // in db.ts, gated on the same first-run marker `findMissedSlots`
            // already uses.

            log::info!("app setup starting, dev_mode={dev_mode}");

            #[cfg(desktop)]
            {
                setup_tray(&handle)?;
                setup_dev_kill_switch(&handle)?;

                // Closing the main window (X button / Alt+F4) must hide it,
                // not destroy it -- the default Tauri behavior. Without this,
                // the window is gone for good after the first close, and the
                // tray's "Open Reflectodoro" / single-instance re-launch
                // handlers (both just get_webview_window("main").show()) find
                // nothing to show and silently no-op. The scheduler and tray
                // keep running headless in the background either way, which
                // is the whole point of having a tray icon.
                if let Some(win) = handle.get_webview_window("main") {
                    let win_to_hide = win.clone();
                    win.on_window_event(move |event| {
                        if let WindowEvent::CloseRequested { api, .. } = event {
                            api.prevent_close();
                            let _ = win_to_hide.hide();
                        }
                    });
                }
            }

            // POMODORO_ENABLED defaults to true and isn't persisted on any
            // platform (see its declaration above), so starting the
            // foreground service unconditionally here matches that same
            // default rather than needing to read a toggle that hasn't had
            // a chance to change yet at this point in startup -- subsequent
            // toggles go through set_enabled in commands.rs instead.
            #[cfg(target_os = "android")]
            {
                let bridge = handle.state::<android_bridge::AndroidBridge<tauri::Wry>>();
                if let Err(e) = bridge.start_foreground_service() {
                    log::error!("failed to start Android foreground service: {e:?}");
                }
                native_overlay::install_channel(&handle);

                // Restores a snooze that was still pending when the previous
                // process incarnation died -- some OEM skins kill a
                // foreground-service process outright when the user swipes
                // it from Recent Apps, which would otherwise silently reset
                // POMODORO_ENABLED/POMODORO_SNOOZE_UNTIL_MS back to their
                // Rust defaults and cancel the pause. Must run before
                // run_scheduler is spawned below, so its very first loop
                // iteration already sees the correct atomics. See
                // commands::snooze_pomodoro and apply_pomodoro_enabled.
                match bridge.get_persisted_pomodoro_snooze_until() {
                    Ok((until_ms, minutes)) if until_ms > Local::now().timestamp_millis() => {
                        POMODORO_ENABLED.store(false, Ordering::SeqCst);
                        POMODORO_SNOOZE_UNTIL_MS.store(until_ms, Ordering::SeqCst);
                        // Restoring `minutes` alongside `until_ms` matters:
                        // the main window's dropdown selects its displayed
                        // <option> off SnoozeInfo.minutes, and leaving this
                        // atomic at its default 0 matched none of the fixed
                        // option values, rendering the dropdown blank/empty
                        // on reopen even though the pause itself was still
                        // correctly in effect.
                        POMODORO_SNOOZE_MINUTES.store(minutes, Ordering::SeqCst);
                        log::info!("setup: restored persisted snooze until {until_ms} ({minutes} min)");
                    }
                    Ok((until_ms, _)) if until_ms != 0 => {
                        // Stale -- already expired while the process was
                        // dead. Clear it so a future restart doesn't have to
                        // reason about staleness again.
                        if let Err(e) = bridge.persist_pomodoro_snooze_until(0, 0) {
                            log::error!("failed to clear stale persisted snooze-until: {e:?}");
                        }
                    }
                    Ok(_) => {}
                    Err(e) => log::error!("failed to read persisted snooze-until: {e:?}"),
                }
            }

            // Hidden, built immediately: gives WebView2 a head start on the
            // startup blank-page race before anything tries to show these.
            // See overlay::WEBVIEW_WARMUP.
            overlay::precreate_windows(&handle);

            let scheduler_handle = handle.clone();
            tauri::async_runtime::spawn(run_scheduler(scheduler_handle));

            // Installed unconditionally, like hook.rs's keyboard hook: the
            // Settings toggle is checked inside the callback instead (see
            // SCREEN_TIME_TRACKING_ENABLED). The flush loop is what batches
            // whatever real focus switches accumulated into one event per
            // minute -- it never invents rows on its own.
            screen_time::start_tracking(&handle);
            let screen_time_handle = handle.clone();
            tauri::async_runtime::spawn(screen_time::run_flush_loop(screen_time_handle));

            // P2P LAN device pairing/sync (Settings -> "Paired devices" /
            // "Import from device"): one always-on TCP listener (accepts
            // both pairing and sync connections, see p2p_sync.rs) plus LAN
            // advertisement so other paired/pairing devices can find this
            // one. Both are best-effort background setup, like the pieces
            // above -- a failure here (e.g. the port is already in use, or
            // this device has no usable network interface) logs and leaves
            // the rest of the app unaffected.
            let p2p_listener_handle = handle.clone();
            tauri::async_runtime::spawn(p2p_sync::run_listener(p2p_listener_handle));
            p2p_sync::advertise_self(&handle);

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
