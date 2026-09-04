use std::sync::atomic::Ordering;

use tauri::{AppHandle, Emitter, State};
#[cfg(target_os = "android")]
use tauri::Manager;
#[cfg(desktop)]
use tauri_plugin_autostart::ManagerExt;

use crate::overlay;
use crate::state::{AppState, OverlayState};
use crate::{
    BREAK_NOTIFICATION_PERSISTENT_ENABLED, FORCE_CLOSE_SHORTCUT_ENABLED, LAST_MEDIA_TOGGLE_AT,
    LAST_WELLNESS_CHECK_AT, MEDIA_PAUSE_ON_BREAK_ENABLED, OVERLAY_AUTO_CLOSE_MINUTES,
    POMODORO_ENABLED,
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
pub fn sync_breakit_config(state: State<AppState>, length: u32, include_special: bool) {
    let mut cfg = state.breakit_config.lock().unwrap();
    cfg.length = length.clamp(4, 64);
    cfg.include_special = include_special;
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
    POMODORO_ENABLED.store(enabled, Ordering::SeqCst);
    let _ = app.emit("pomodoro://enabled-changed", enabled);

    // Keeps the Android foreground service (and its AlarmManager backup) in
    // step with the toggle: starting it when the user turns Pomodoro mode
    // on (mirrors the same call in lib.rs's .setup(), for the already-on
    // default at launch) and stopping it when they turn it off, so
    // disabling actually lets Android reclaim the process instead of
    // leaving a phantom "running" notification behind.
    #[cfg(target_os = "android")]
    {
        let bridge = app.state::<crate::android_bridge::AndroidBridge<tauri::Wry>>();
        let result = if enabled {
            bridge.start_foreground_service()
        } else {
            bridge.stop_foreground_service()
        };
        if let Err(e) = result {
            log::error!("failed to toggle Android foreground service: {e:?}");
        }
    }
}

/// Read by the check-in window on mount to learn which slot triggered it.
#[tauri::command]
pub fn get_checkin_slot(state: State<AppState>) -> Option<String> {
    let slot = state.checkin_slot.lock().unwrap().clone();
    log::info!("get_checkin_slot -> {slot:?}");
    slot
}

/// Backs the Settings "Data" export/import feature. Plain `std::fs` rather
/// than tauri-plugin-fs: app-defined commands need no capability entry at
/// all, sidestepping that plugin's path-scope config entirely (the same
/// class of silent-permission trap already hit twice with sql/window
/// capabilities -- see CLAUDE.md). The path always comes from the native
/// dialog picker, not arbitrary user text input.
#[tauri::command]
pub fn read_text_file(path: String) -> Result<String, String> {
    std::fs::read_to_string(&path).map_err(|e| format!("failed to read {path}: {e}"))
}

#[tauri::command]
pub fn write_text_file(path: String, contents: String) -> Result<(), String> {
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

#[tauri::command]
pub fn set_overlay_auto_close_minutes(minutes: u32) {
    OVERLAY_AUTO_CLOSE_MINUTES.store(minutes.max(1), Ordering::SeqCst);
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
