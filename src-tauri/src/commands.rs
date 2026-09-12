use std::sync::atomic::Ordering;

use chrono::TimeZone;
use tauri::{AppHandle, Emitter, State};
#[cfg(target_os = "android")]
use tauri::Manager;
#[cfg(desktop)]
use tauri_plugin_autostart::ManagerExt;

use crate::breakit;
use crate::overlay;
use crate::screen_time;
use crate::state::{AppState, OverlayState};
use crate::{
    apply_pomodoro_enabled, BREAK_NOTIFICATION_PERSISTENT_ENABLED, FORCE_CLOSE_SHORTCUT_ENABLED,
    LAST_MEDIA_TOGGLE_AT, LAST_WELLNESS_CHECK_AT, MACOS_HIDE_MENU_BAR_DOCK_ENABLED,
    MEDIA_PAUSE_ON_BREAK_ENABLED, OVERLAY_AUTO_CLOSE_MINUTES, POMODORO_ENABLED,
    POMODORO_SNOOZE_MINUTES, POMODORO_SNOOZE_UNTIL_MS, SCREEN_TIME_TRACKING_ENABLED,
    SNOOZE_MAX_MINUTES, SNOOZE_MIN_MINUTES,
};

#[tauri::command]
pub fn get_overlay_state(state: State<AppState>) -> OverlayState {
    state.overlay.lock().unwrap().clone()
}

#[tauri::command]
pub fn is_dev_mode(state: State<AppState>) -> bool {
    state.dev_mode
}

/// Lets the frontend render OS-appropriate shortcut text (e.g. the force-close
/// kill switch is Ctrl+Alt+Shift+F12 on Windows but Cmd+Option+Shift+F12 on
/// macOS -- see setup_dev_kill_switch in lib.rs).
#[tauri::command]
pub fn current_os() -> &'static str {
    std::env::consts::OS
}

/// Pushes the breakit challenge length/charset (loaded by the frontend from
/// app_setting) into Rust state. Called once on app boot and again whenever
/// Settings saves, so SQLite stays the source of truth while the scheduler
/// still has a fast in-memory copy to use when it spawns a fresh overlay.
#[tauri::command]
pub fn sync_breakit_config(app: AppHandle, state: State<AppState>, length: u32, include_special: bool) {
    let (len, include_special) = {
        let mut cfg = state.breakit_config.lock().unwrap();
        cfg.length = length.clamp(4, 64);
        cfg.include_special = include_special;
        (cfg.length, cfg.include_special)
    };
    log::info!("sync_breakit_config: length={len} include_special={include_special}");

    // Cold-start config race (see run_scheduler's Break arm, lib.rs): this
    // sync can still land after a break has already opened and shown a
    // challenge generated from the pre-sync default -- run_scheduler's own
    // post-warmup regeneration narrows that window but can't close it
    // outright (confirmed live: a real device's frontend boot took ~7.5s,
    // past the 6s warmup). Rather than widen that window further, correct
    // any already-open, not-yet-solved overlay in place the moment the real
    // config finally arrives, however late. Lock ordering: `breakit_config`
    // above is already released before `overlay` is taken below -- never
    // nest them the other way, since run_scheduler nests overlay-then-
    // breakit_config (opened_for/generate_breakit_challenge) and holding
    // both at once in opposite orders across two threads is a deadlock.
    let corrected = {
        let mut overlay = state.overlay.lock().unwrap();
        if overlay.open && !overlay.breakit_matched {
            overlay.breakit_challenge = breakit::generate_challenge(len, include_special);
            true
        } else {
            false
        }
    };
    if corrected {
        log::info!("sync_breakit_config: corrected already-open overlay's challenge");
        overlay::emit_state(&app);
    }
}

#[tauri::command]
pub fn mark_reflection_entered(app: AppHandle, state: State<AppState>) -> OverlayState {
    {
        let mut overlay = state.overlay.lock().unwrap();
        overlay.reflection_entered = true;
    }
    overlay::try_close_if_unlocked(&app);
    state.overlay.lock().unwrap().clone()
}

/// How many consecutive `report_reflection_save_failure` calls (for this one
/// overlay occurrence) it takes before `close_after_save_failure` actually
/// closes the overlay.
const SAVE_FAILURE_ESCAPE_THRESHOLD: u32 = 2;

/// Called by the overlay page each time its own reflection-save attempt
/// (saveReflection, or the follow-up mark_reflection_entered invoke) rejects
/// -- i.e. the DB genuinely won't accept the write. Returns the new count so
/// the frontend knows when to reveal `close_after_save_failure`'s escape
/// hatch, without the frontend itself being the one deciding when that
/// threshold is met -- see `OverlayState::save_failure_count`'s doc comment
/// for why that server-side counter matters. A no-op (returns 0) if the
/// overlay isn't even open -- nothing to report a failure against.
#[tauri::command]
pub fn report_reflection_save_failure(state: State<AppState>) -> u32 {
    let mut overlay = state.overlay.lock().unwrap();
    if !overlay.open {
        return 0;
    }
    overlay.save_failure_count += 1;
    overlay.save_failure_count
}

/// Last-resort escape from the break overlay when the reflection genuinely
/// can't be saved (a locked or full DB, most concretely) -- gated on at
/// least `SAVE_FAILURE_ESCAPE_THRESHOLD` reported failures for *this*
/// occurrence (see `report_reflection_save_failure`), not callable outright:
/// a break the user simply doesn't want to do can't be talked out of by
/// invoking this once, only by first hitting real, repeated save failures.
/// Bypasses the unlock formula the same way the F12 kill switch and dev
/// force-close already do -- no reflection is recorded for this slot, so no
/// wellness check-in opens either (see `close_overlay`).
#[tauri::command]
pub fn close_after_save_failure(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    let count = state.overlay.lock().unwrap().save_failure_count;
    if count < SAVE_FAILURE_ESCAPE_THRESHOLD {
        return Err(format!(
            "not enough reported save failures yet ({count}/{SAVE_FAILURE_ESCAPE_THRESHOLD})"
        ));
    }
    overlay::close_overlay(&app);
    Ok(())
}

#[tauri::command]
pub fn breakit_attempt(app: AppHandle, state: State<AppState>, input: String) -> OverlayState {
    {
        let mut overlay = state.overlay.lock().unwrap();
        if overlay.open && input == overlay.breakit_challenge {
            overlay.breakit_matched = true;
        }
    }
    overlay::try_close_if_unlocked(&app);
    state.overlay.lock().unwrap().clone()
}

#[tauri::command]
pub fn get_enabled() -> bool {
    POMODORO_ENABLED.load(Ordering::SeqCst)
}

#[tauri::command]
pub fn set_enabled(app: AppHandle, enabled: bool) {
    apply_pomodoro_enabled(&app, enabled);
}

/// A pending "snooze" (Pomodoro mode temporarily paused from the main
/// window's dropdown, auto-resuming on its own -- see `snooze_pomodoro`).
/// Snake_case fields, no camelCase rename: matches `OverlayState`'s existing
/// convention, read directly (e.g. `current_slot_start`) on the frontend.
#[derive(Clone, serde::Serialize)]
pub struct SnoozeInfo {
    pub resume_at: String,
    pub minutes: u32,
}

/// Pauses Pomodoro mode for `minutes` (clamped to `[SNOOZE_MIN_MINUTES,
/// SNOOZE_MAX_MINUTES]`), auto-resuming on its own once that time passes --
/// see `run_scheduler`'s wall-clock poll of `POMODORO_SNOOZE_UNTIL_MS` in
/// lib.rs for why this is a poll rather than a timer. Returns the resolved
/// `SnoozeInfo` so the caller doesn't have to race the
/// `pomodoro://snooze-changed` event for its own same-window update (see
/// best_practices.md on preferring command return values).
///
/// Android-specific and load-bearing: unlike a permanent Off
/// (`apply_pomodoro_enabled`), this deliberately never calls
/// `stop_foreground_service` -- stopping it is what lets Android reclaim
/// (kill) the process while backgrounded, which would leave nothing running
/// to notice the snooze expiring. It also persists the boot-recovery
/// preference as `true`, not `false`: the durable preference stays "on"
/// through a snooze, and only a real permanent Off should persist `false`.
/// Known accepted limitation: there's no persisted resume-at, so a full
/// device reboot mid-snooze comes back enabled rather than resuming the
/// remaining snooze -- the same non-persistence already documented for
/// `POMODORO_ENABLED` itself (see CLAUDE.md's Android section).
#[tauri::command]
pub fn snooze_pomodoro(app: AppHandle, minutes: u32) -> SnoozeInfo {
    let minutes = minutes.clamp(SNOOZE_MIN_MINUTES, SNOOZE_MAX_MINUTES);
    let resume_at = chrono::Local::now() + chrono::Duration::minutes(minutes as i64);
    let resume_at_iso = resume_at.to_rfc3339();

    POMODORO_ENABLED.store(false, Ordering::SeqCst);
    POMODORO_SNOOZE_UNTIL_MS.store(resume_at.timestamp_millis(), Ordering::SeqCst);
    POMODORO_SNOOZE_MINUTES.store(minutes, Ordering::SeqCst);

    let info = SnoozeInfo {
        resume_at: resume_at_iso,
        minutes,
    };
    log::info!("snooze_pomodoro: pausing for {minutes} min, resuming at {}", info.resume_at);

    let _ = app.emit("pomodoro://enabled-changed", false);
    let _ = app.emit("pomodoro://snooze-changed", Some(info.clone()));

    #[cfg(target_os = "android")]
    {
        let bridge = app.state::<crate::android_bridge::AndroidBridge<tauri::Wry>>();
        if let Err(e) = bridge.start_foreground_service() {
            log::error!("failed to (re)start Android foreground service for snooze: {e:?}");
        }
        if let Err(e) = bridge.persist_pomodoro_enabled(true) {
            log::error!("failed to persist pomodoro-enabled preference during snooze: {e:?}");
        }
    }

    info
}

/// Read by the main window on boot to restore a snooze already in progress
/// (e.g. after a page reload) -- see `snooze_pomodoro`.
#[tauri::command]
pub fn get_snooze_until() -> Option<SnoozeInfo> {
    let until_ms = POMODORO_SNOOZE_UNTIL_MS.load(Ordering::SeqCst);
    if until_ms == 0 {
        return None;
    }
    let resume_at = chrono::Local.timestamp_millis_opt(until_ms).single()?;
    Some(SnoozeInfo {
        resume_at: resume_at.to_rfc3339(),
        minutes: POMODORO_SNOOZE_MINUTES.load(Ordering::SeqCst),
    })
}

/// Read by the check-in window on mount to learn which slot triggered it.
#[tauri::command]
pub fn get_checkin_slot(state: State<AppState>) -> Option<String> {
    let slot = state.checkin_slot.lock().unwrap().clone();
    log::info!("get_checkin_slot -> {slot:?}");
    slot
}

/// Rejects anything without a `.json` extension. The path is only ever
/// expected to come from the export/import dialog pickers, which already
/// filter to JSON -- this is a second, server-side check so these two
/// commands can't be repurposed into a generic "read/write any file the
/// process can touch" primitive by whatever calls `invoke` directly (a
/// future `{@html}`/`innerHTML` mistake, most concretely -- see these
/// commands' own doc comment for why that combination matters here).
fn require_json_extension(path: &str) -> Result<(), String> {
    let has_json_ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("json"));
    if has_json_ext {
        Ok(())
    } else {
        Err("only .json files are supported".into())
    }
}

/// Backs the Settings "Data" export/import feature. Plain `std::fs` rather
/// than tauri-plugin-fs: app-defined commands need no capability entry at
/// all, sidestepping that plugin's path-scope config entirely (the same
/// class of silent-permission trap already hit twice with sql/window
/// capabilities -- see CLAUDE.md). The path always comes from the native
/// dialog picker, not arbitrary user text input -- but that's a frontend
/// convention, not something enforced here structurally, and this
/// deliberately sidesteps tauri-plugin-fs's own path scoping. Combined with
/// this app's null CSP and `sql:allow-execute`, an unscoped file read/write
/// would turn any future script-injection bug into read-any-file-plus-
/// arbitrary-SQL; the `.json`-extension check above is the cheap mitigation
/// available without knowing the picker's chosen directory ahead of time.
#[tauri::command]
pub fn read_text_file(path: String) -> Result<String, String> {
    require_json_extension(&path)?;
    std::fs::read_to_string(&path).map_err(|e| format!("failed to read {path}: {e}"))
}

#[tauri::command]
pub fn write_text_file(path: String, contents: String) -> Result<(), String> {
    require_json_extension(&path)?;
    std::fs::write(&path, contents).map_err(|e| format!("failed to write {path}: {e}"))
}

/// Reflects the actual OS registration state (Windows Run registry key /
/// equivalent elsewhere) -- this is the single source of truth for on/off,
/// not anything stored in app_setting. See `ensureDefaultAutostart` in
/// db.ts for why a *separate* DB flag exists to track whether the user has
/// ever made an explicit choice at all.
///
/// No Android implementation: `tauri-plugin-autostart` isn't linked on
/// mobile at all (see Cargo.toml's target-gated dependencies), so this
/// always reports/no-ops there rather than being omitted -- the frontend
/// calls these unconditionally and shouldn't need a platform check just to
/// avoid an invoke error.
#[cfg(desktop)]
#[tauri::command]
pub fn get_autostart_enabled(app: AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[cfg(not(desktop))]
#[tauri::command]
pub fn get_autostart_enabled(_app: AppHandle) -> bool {
    false
}

#[cfg(desktop)]
#[tauri::command]
pub fn set_autostart_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    let auto = app.autolaunch();
    let result = if enabled { auto.enable() } else { auto.disable() };
    result.map_err(|e| e.to_string())
}

#[cfg(not(desktop))]
#[tauri::command]
pub fn set_autostart_enabled(_app: AppHandle, _enabled: bool) -> Result<(), String> {
    Err("autostart is not supported on this platform currently.".into())
}

/// Mirrors app_setting.force_close_shortcut_enabled -- loaded and pushed here
/// by the frontend on boot and on every Settings save (see
/// loadAndSyncForceCloseShortcutSetting in db.ts).
#[tauri::command]
pub fn get_force_close_shortcut_enabled() -> bool {
    FORCE_CLOSE_SHORTCUT_ENABLED.load(Ordering::SeqCst)
}

#[tauri::command]
pub fn set_force_close_shortcut_enabled(enabled: bool) {
    FORCE_CLOSE_SHORTCUT_ENABLED.store(enabled, Ordering::SeqCst);
}

/// Mirrors app_setting.overlay_auto_close_minutes -- loaded and pushed here
/// by the frontend on boot and on every Settings save (see
/// loadAndSyncOverlayAutoClose in db.ts).
#[tauri::command]
pub fn get_overlay_auto_close_minutes() -> u32 {
    OVERLAY_AUTO_CLOSE_MINUTES.load(Ordering::SeqCst)
}

/// Upper bound mirrors the one already enforced for breakit_length just above
/// this pattern (sync_breakit_config's `clamp(4, 64)`): without one, `u32::MAX`
/// minutes (~8,100 years) silently defeats the documented last-resort
/// force-close, since nothing ever waits that long. 60 (1h) is generous
/// grace for an overlay whose nominal break is 5 minutes.
#[tauri::command]
pub fn set_overlay_auto_close_minutes(minutes: u32) {
    OVERLAY_AUTO_CLOSE_MINUTES.store(minutes.clamp(1, 60), Ordering::SeqCst);
}

/// Mirrors app_setting.media_pause_on_break_enabled -- loaded and pushed here
/// by the frontend on boot and on every Settings save (see
/// loadAndSyncMediaPauseOnBreakSetting in db.ts).
#[tauri::command]
pub fn get_media_pause_on_break_enabled() -> bool {
    MEDIA_PAUSE_ON_BREAK_ENABLED.load(Ordering::SeqCst)
}

#[tauri::command]
pub fn set_media_pause_on_break_enabled(enabled: bool) {
    MEDIA_PAUSE_ON_BREAK_ENABLED.store(enabled, Ordering::SeqCst);
}

/// Mirrors app_setting.macos_hide_menu_bar_dock_enabled -- loaded and pushed
/// here by the frontend on boot and on every Settings save (see
/// loadAndSyncMacosHideMenuBarDockSetting in db.ts). This only gates menu
/// bar/Dock hiding; Space-following and the Cmd+Tab block are always on (see
/// macos_overlay.rs's module doc). Registered unconditionally (not
/// `#[cfg(target_os = "macos")]`) so the frontend can call it on any platform
/// without a per-platform invoke gate; the flag itself is simply never read
/// on non-macOS (see overlay.rs's spawn_or_update_overlay).
#[tauri::command]
pub fn get_macos_hide_menu_bar_dock_enabled() -> bool {
    MACOS_HIDE_MENU_BAR_DOCK_ENABLED.load(Ordering::SeqCst)
}

#[tauri::command]
pub fn set_macos_hide_menu_bar_dock_enabled(enabled: bool) {
    MACOS_HIDE_MENU_BAR_DOCK_ENABLED.store(enabled, Ordering::SeqCst);
}

/// Pushes both halves of the macOS media-toggle guard (see media.rs) into
/// Rust state. Called once on main-window boot, after the frontend loads
/// `app_setting.last_toggle_time` and the most recent `wellness_check.created_at`.
#[tauri::command]
pub fn sync_media_toggle_guard(last_toggle_at: Option<String>, last_wellness_check_at: Option<String>) {
    *LAST_MEDIA_TOGGLE_AT.lock().unwrap() = last_toggle_at;
    *LAST_WELLNESS_CHECK_AT.lock().unwrap() = last_wellness_check_at;
}

/// Called after a check-in is actually saved (not skipped/auto-closed) so the
/// macOS media-toggle guard resets within the current session, not just after
/// a restart. See `submit()` in checkin/+page.svelte.
#[tauri::command]
pub fn sync_last_wellness_check_at(at: String) {
    *LAST_WELLNESS_CHECK_AT.lock().unwrap() = Some(at);
}

/// Mirrors app_setting.screen_time_tracking_enabled -- loaded and pushed here
/// by the frontend on boot and on every Settings save (see
/// loadAndSyncScreenTimeTrackingSetting in db.ts).
#[tauri::command]
pub fn get_screen_time_tracking_enabled() -> bool {
    SCREEN_TIME_TRACKING_ENABLED.load(Ordering::SeqCst)
}

/// Switching this off closes out and flushes whatever session was in
/// progress right away (see screen_time::flush_now), rather than leaving it
/// stranded in memory until tracking is switched back on -- at which point
/// it would otherwise be recorded as one session spanning the entire
/// tracking-disabled period.
#[tauri::command]
pub fn set_screen_time_tracking_enabled(app: AppHandle, enabled: bool) {
    SCREEN_TIME_TRACKING_ENABLED.store(enabled, Ordering::SeqCst);
    if enabled {
        // Start attributing whatever is focused right now instead of waiting
        // for the next window switch.
        screen_time::resync_current_focus(&app);
    } else {
        screen_time::flush_now(&app);
    }
}

/// The app currently in focus and how long it's been focused, read straight
/// from memory -- nothing is written and no row is created. The Entries page
/// blends this into today's breakdown so the in-progress app shows a live
/// total without needing periodic DB writes (see screen_time.rs's module
/// doc). `None` when nothing is being tracked right now (tracking off, focus
/// on Reflectodoro itself, or no capture on this platform yet).
#[tauri::command]
pub fn get_current_session_snapshot() -> Option<CurrentSessionSnapshot> {
    screen_time::current_session_elapsed_ms().map(|(app_id, display_name, elapsed_ms)| {
        CurrentSessionSnapshot { app_id, display_name, elapsed_ms }
    })
}

#[derive(serde::Serialize)]
pub struct CurrentSessionSnapshot {
    pub app_id: String,
    pub display_name: String,
    pub elapsed_ms: i64,
}

/// This device's hostname, used to seed `app_setting.device_name` once on
/// first boot so screen-time rows carry which machine they came from (the
/// user can rename it in Settings afterwards). Read-only, same shape as
/// `current_os`.
///
/// Deliberately env/procfs based rather than pulling in a crate for it.
/// Returns "" where that isn't available -- an empty device name is a
/// perfectly fine state (the Entries breakdown just doesn't show a device
/// label), and the Settings field lets the user type one in.
#[cfg(not(target_os = "android"))]
#[tauri::command]
pub fn get_hostname() -> String {
    #[cfg(windows)]
    {
        std::env::var("COMPUTERNAME").unwrap_or_default()
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/sys/kernel/hostname")
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        std::env::var("HOSTNAME").unwrap_or_default()
    }
}

/// Android has no traditional hostname -- `Settings.Global.DEVICE_NAME`
/// (falling back to `Build.MODEL`) is the closest equivalent, read via
/// `NativeBridgePlugin.kt::getDeviceName`. Previously always returned ""
/// here (no Android arm existed at all), which meant device_name stayed
/// empty forever on Android -- see p2p_sync.rs's "P2P LAN sync" section in
/// CLAUDE.md for where a blank device_name actually showed up (the paired
/// devices list falling back to a raw device_id).
#[cfg(target_os = "android")]
#[tauri::command]
pub fn get_hostname(app: AppHandle) -> String {
    let bridge = app.state::<crate::android_bridge::AndroidBridge<tauri::Wry>>();
    match bridge.get_device_name() {
        Ok(v) => v.get("value").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        Err(e) => {
            log::error!("get_hostname (Android) failed: {e:?}");
            String::new()
        }
    }
}

/// Whether the native break overlay (native_overlay.rs) can actually be
/// shown -- surfaced to onboarding/Settings so it only prompts for a grant
/// that's actually missing. When false, spawn_or_update_overlay's Android arm
/// falls back to a break notification instead (see trigger_break_screen's
/// Kotlin side).
#[cfg(target_os = "android")]
#[tauri::command]
pub fn can_draw_overlays(app: AppHandle) -> bool {
    let bridge = app.state::<crate::android_bridge::AndroidBridge<tauri::Wry>>();
    match bridge.can_draw_overlays() {
        Ok(v) => v.get("value").and_then(|x| x.as_bool()).unwrap_or(false),
        Err(e) => {
            log::error!("can_draw_overlays failed: {e:?}");
            false
        }
    }
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
pub fn can_draw_overlays() -> bool {
    true
}

/// Opens the system settings screen for the grant -- there is no in-app
/// runtime-dialog form of this permission. No-op on desktop.
#[cfg(target_os = "android")]
#[tauri::command]
pub fn request_draw_overlays_permission(app: AppHandle) {
    let bridge = app.state::<crate::android_bridge::AndroidBridge<tauri::Wry>>();
    if let Err(e) = bridge.request_draw_overlays_permission() {
        log::error!("request_draw_overlays_permission failed: {e:?}");
    }
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
pub fn request_draw_overlays_permission() {}

/// Whether the AlarmManager backup (BreakScheduling.kt's scheduleNextAlarm)
/// can use setAlarmClock's real-exact/foreground-launch-exempt path rather
/// than its degraded inexact fallback -- surfaced to onboarding/Settings so
/// it only prompts for a grant that's actually missing.
#[cfg(target_os = "android")]
#[tauri::command]
pub fn can_schedule_exact_alarms(app: AppHandle) -> bool {
    let bridge = app.state::<crate::android_bridge::AndroidBridge<tauri::Wry>>();
    match bridge.can_schedule_exact_alarms() {
        Ok(v) => v.get("value").and_then(|x| x.as_bool()).unwrap_or(false),
        Err(e) => {
            log::error!("can_schedule_exact_alarms failed: {e:?}");
            false
        }
    }
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
pub fn can_schedule_exact_alarms() -> bool {
    true
}

/// Opens the system settings screen for the "Alarms & reminders" grant --
/// there is no in-app runtime-dialog form of this permission. No-op on
/// desktop.
#[cfg(target_os = "android")]
#[tauri::command]
pub fn request_schedule_exact_alarm_permission(app: AppHandle) {
    let bridge = app.state::<crate::android_bridge::AndroidBridge<tauri::Wry>>();
    if let Err(e) = bridge.request_schedule_exact_alarm_permission() {
        log::error!("request_schedule_exact_alarm_permission failed: {e:?}");
    }
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
pub fn request_schedule_exact_alarm_permission() {}

/// Whether the special-access "Usage access" grant (`PACKAGE_USAGE_STATS`)
/// is on -- screen_time.rs's Android polling depends on it; surfaced to
/// Settings so it only prompts for a grant that's actually missing. Without
/// it, `screen_time_session` simply stays empty on Android.
#[cfg(target_os = "android")]
#[tauri::command]
pub fn can_query_usage_stats(app: AppHandle) -> bool {
    let bridge = app.state::<crate::android_bridge::AndroidBridge<tauri::Wry>>();
    match bridge.can_query_usage_stats() {
        Ok(v) => v.get("value").and_then(|x| x.as_bool()).unwrap_or(false),
        Err(e) => {
            log::error!("can_query_usage_stats failed: {e:?}");
            false
        }
    }
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
pub fn can_query_usage_stats() -> bool {
    true
}

/// Opens the system Usage Access settings screen -- there is no in-app
/// runtime-dialog form of this permission. No-op on desktop.
#[cfg(target_os = "android")]
#[tauri::command]
pub fn request_usage_stats_permission(app: AppHandle) {
    let bridge = app.state::<crate::android_bridge::AndroidBridge<tauri::Wry>>();
    if let Err(e) = bridge.request_usage_stats_permission() {
        log::error!("request_usage_stats_permission failed: {e:?}");
    }
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
pub fn request_usage_stats_permission() {}

/// Mirrors app_setting.break_notification_persistent_enabled -- loaded and
/// pushed here by the frontend on boot and on every Settings save (see
/// loadAndSyncBreakNotificationPersistentSetting in db.ts). Android only in
/// effect (see overlay.rs's spawn_or_update_overlay), but readable/settable
/// cross-platform like the other toggles so the Settings page doesn't need
/// its own platform branching just to read a stored value.
#[tauri::command]
pub fn get_break_notification_persistent_enabled() -> bool {
    BREAK_NOTIFICATION_PERSISTENT_ENABLED.load(Ordering::SeqCst)
}

#[tauri::command]
pub fn set_break_notification_persistent_enabled(enabled: bool) {
    BREAK_NOTIFICATION_PERSISTENT_ENABLED.store(enabled, Ordering::SeqCst);
}

/// Only available when dev_mode is on -- bypasses the unlock formula entirely.
/// A permanent, non-dev-gated safety net also exists via the global shortcut
/// (see lib.rs) and the tray Quit item.
#[tauri::command]
pub fn dev_force_close(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    if !state.dev_mode {
        return Err("dev mode is not enabled".into());
    }
    overlay::close_overlay(&app);
    Ok(())
}
