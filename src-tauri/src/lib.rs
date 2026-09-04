#[cfg(target_os = "android")]
mod android_bridge;
mod breakit;
mod commands;
mod db;
mod grid;
mod hook;
mod media;
mod native_overlay;
mod overlay;
mod state;

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Local};
#[cfg(desktop)]
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
#[cfg(desktop)]
use tauri::tray::TrayIconBuilder;
#[cfg(desktop)]
use tauri::Emitter;
#[cfg(desktop)]
use tauri::WindowEvent;
use tauri::{AppHandle, Manager};
#[cfg(desktop)]
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use grid::Phase;
use state::{AppState, OverlayState};

/// Debug builds (`npm run tauri dev`) run in dev mode by default: the overlay
/// shows a visible "Close (DEV)" button and skips the Alt-Tab/Win-key hook.
/// Override with POMODORO_DEV_MODE=0/1 if you need to test enforcement in a
/// debug build, or force it on in a release build for QA.
fn resolve_dev_mode() -> bool {
    match std::env::var("POMODORO_DEV_MODE").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => cfg!(debug_assertions),
    }
}

pub(crate) static POMODORO_ENABLED: AtomicBool = AtomicBool::new(true);

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

async fn run_scheduler(app: AppHandle) {
    let mut last_phase: Option<Phase> = None;
    let mut expected_wake: Option<DateTime<Local>> = None;

    loop {
        let now = Local::now();

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
                        let (len, include_special) = {
                            let app_state = app.state::<AppState>();
                            let cfg = app_state.breakit_config.lock().unwrap();
                            (cfg.length, cfg.include_special)
                        };
                        let challenge = breakit::generate_challenge(len, include_special);
                        let this_slot_start = slot.start_iso();
                        {
                            let state = app.state::<AppState>();
                            let mut ov = state.overlay.lock().unwrap();
                            *ov = OverlayState::opened_for(this_slot_start.clone(), challenge);
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
            "open" => {
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            }
            "toggle" => {
                let enabled = !POMODORO_ENABLED.load(Ordering::SeqCst);
                POMODORO_ENABLED.store(enabled, Ordering::SeqCst);
                let label = if enabled {
                    "Disable Pomodoro Mode"
                } else {
                    "Enable Pomodoro Mode"
                };
                let _ = toggle_item.set_text(label);
                let _ = app.emit("pomodoro://enabled-changed", enabled);
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
    #[cfg(target_os = "windows")]
    unsafe {
        std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", "--disable-gpu");
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
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
                let _ = win.set_focus();
            }
        }));
    }

    builder = builder
        .manage(AppState::new(dev_mode))
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
            commands::can_draw_overlays,
            commands::request_draw_overlays_permission,
            commands::can_schedule_exact_alarms,
            commands::request_schedule_exact_alarm_permission,
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
            }

            // Hidden, built immediately: gives WebView2 a head start on the
            // startup blank-page race before anything tries to show these.
            // See overlay::WEBVIEW_WARMUP.
            overlay::precreate_windows(&handle);

            let scheduler_handle = handle.clone();
            tauri::async_runtime::spawn(run_scheduler(scheduler_handle));

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
